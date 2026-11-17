//! Real binary startup/HTTPS checks with ephemeral, locally generated certificates.
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "ottplay TLS chain {}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn openssl(&self, args: &[&str]) {
        let output = Command::new("openssl")
            .args(args)
            .current_dir(&self.0)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "openssl {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn key(&self, name: &str, algorithm: &str) {
        let output = format!("{name}.key");
        let mut args = vec!["genpkey", "-out", &output, "-algorithm"];
        let option;
        if let Some(bits) = algorithm.strip_prefix("rsa:") {
            option = format!("rsa_keygen_bits:{bits}");
            args.extend(["RSA", "-pkeyopt", &option]);
        } else if let Some(curve) = algorithm.strip_prefix("ec:") {
            option = format!("ec_paramgen_curve:{curve}");
            args.extend(["EC", "-pkeyopt", &option]);
        } else if algorithm == "pss" {
            args.extend(["RSA-PSS", "-pkeyopt", "rsa_keygen_bits:2048"]);
        } else {
            args.push("ED25519");
        }
        self.openssl(&args);
    }
    fn sign(&self, name: &str, issuer: &str, ca: bool, serial: &str) {
        let key = format!("{name}.key");
        let csr = format!("{name}.csr");
        let cert = format!("{name}.pem");
        let issuer_cert = format!("{issuer}.pem");
        let issuer_key = format!("{issuer}.key");
        let extension = if ca {
            "basicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\n"
        } else {
            "basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=IP:127.0.0.1\n"
        };
        fs::write(self.0.join("sign.ext"), extension).unwrap();
        self.openssl(&[
            "req",
            "-new",
            "-key",
            &key,
            "-out",
            &csr,
            "-subj",
            &format!("/CN=Synthetic {name}"),
        ]);
        self.openssl(&[
            "x509",
            "-req",
            "-sha256",
            "-days",
            "1",
            "-in",
            &csr,
            "-CA",
            &issuer_cert,
            "-CAkey",
            &issuer_key,
            "-set_serial",
            serial,
            "-extfile",
            "sign.ext",
            "-out",
            &cert,
        ]);
    }
    fn certificates(&self, root: &str, intermediate: Option<&str>, leaf: &str) {
        self.key("root", root);
        fs::write(self.0.join("root.cnf"), "[req]\ndistinguished_name=dn\nx509_extensions=ca\nprompt=no\n[dn]\nCN=Synthetic root\n[ca]\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\n").unwrap();
        self.openssl(&[
            "req", "-new", "-x509", "-sha256", "-days", "1", "-key", "root.key", "-out",
            "root.pem", "-config", "root.cnf",
        ]);
        let issuer = if let Some(algorithm) = intermediate {
            self.key("intermediate", algorithm);
            self.sign("intermediate", "root", true, "2");
            "intermediate"
        } else {
            "root"
        };
        self.key("leaf", leaf);
        self.sign("leaf", issuer, false, "3");
        let mut chain = fs::read(self.0.join("leaf.pem")).unwrap();
        if intermediate.is_some() {
            chain.extend(fs::read(self.0.join("intermediate.pem")).unwrap());
        }
        // Deliberately include the root: only supplied certificates are in this policy's scope.
        chain.extend(fs::read(self.0.join("root.pem")).unwrap());
        fs::write(self.0.join("chain.pem"), chain).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn run_server(fixture: &Fixture, accepted: bool, case: &str) {
    let http = TcpListener::bind("127.0.0.1:0").unwrap();
    let https = TcpListener::bind("127.0.0.1:0").unwrap();
    let http_port = http.local_addr().unwrap().port();
    let tls_port = https.local_addr().unwrap().port();
    // Rejected chains must fail validation before attempting to bind either port.
    // Keep both reservations for that case: probing released ephemeral ports can
    // connect to an unrelated listener instead of the child under test.
    let _reservations = if accepted {
        drop((http, https));
        None
    } else {
        Some((http, https))
    };
    let log = fs::File::create(fixture.0.join("server.log")).unwrap();
    let mut server = Server(
        Command::new(env!("CARGO_BIN_EXE_ottplay-server"))
            .args([
                "--host",
                "127.0.0.1",
                "--port",
                &http_port.to_string(),
                "--https-port",
                &tls_port.to_string(),
                "--cert",
                "chain.pem",
                "--key",
                "leaf.key",
            ])
            .current_dir(&fixture.0)
            .env_clear()
            // Winsock loads system providers using SystemRoot on Windows.
            .envs(std::env::var_os("SystemRoot").map(|value| ("SystemRoot", value)))
            .env("EPG_URLS", "http://127.0.0.1:1/synthetic-unavailable")
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    let readiness = format!("ottplay-server: https://127.0.0.1:{tls_port}");
    loop {
        if let Some(status) = server.0.try_wait().unwrap() {
            let message = fs::read_to_string(fixture.0.join("server.log")).unwrap();
            assert!(!accepted, "server rejected strong fixture: {message}");
            assert!(
                !status.success(),
                "server exited successfully for a rejected TLS fixture: {message}"
            );
            assert!(
                message.contains("TLS certificate"),
                "server must reject the certificate before binding reserved ports: {message}"
            );
            return;
        }
        // Only this child's post-bind announcement proves it owns the listener.
        // A raw TCP probe can connect to another owner of the released port.
        if accepted
            && fs::read_to_string(fixture.0.join("server.log"))
                .unwrap()
                .lines()
                .any(|line| line == readiness)
        {
            verified_health(&fixture.0, tls_port, case);
            return;
        }
        assert!(
            Instant::now() < deadline,
            "server did not {} within startup deadline: {}",
            if accepted {
                "start verified HTTPS"
            } else {
                "reject the TLS fixture before binding reserved ports"
            },
            fs::read_to_string(fixture.0.join("server.log")).unwrap()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn read_prefix(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?.take(limit).read_to_end(&mut bytes)?;
    Ok(bytes)
}

// OpenSSL 3.5 apps_ssl_info_callback state labels, not evidence of bytes sent
// or completed reads. A negative callback exit is not necessarily fatal.
fn handshake_state_evidence(bytes: &[u8], truncated: bool) -> String {
    const STATES: &[(&str, &str)] = &[
        ("before SSL initialization", "BeforeInit"),
        ("SSLv3/TLS write client hello", "AtClientHello"),
        ("SSLv3/TLS read server hello", "AtServerHello"),
        ("TLSv1.3 read encrypted extensions", "AtEncryptedExtensions"),
        ("SSLv3/TLS read server certificate", "AtCertificate"),
        (
            "TLSv1.3 read server certificate verify",
            "AtCertificateVerify",
        ),
        ("SSLv3/TLS read server key exchange", "AtServerKeyExchange"),
        ("SSLv3/TLS read server done", "AtServerDone"),
        ("SSLv3/TLS write client key exchange", "AtClientKeyExchange"),
        ("SSLv3/TLS write change cipher spec", "AtWriteChangeCipher"),
        ("SSLv3/TLS read change cipher spec", "AtReadChangeCipher"),
        ("SSLv3/TLS write finished", "AtWriteFinished"),
        ("SSLv3/TLS read finished", "AtReadFinished"),
        ("SSL negotiation finished successfully", "AtNegotiated"),
        ("SSLv3/TLS read server session ticket", "AtSessionTicket"),
        ("error", "AtErrorState"),
    ];
    let complete = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |i| i + 1);
    let (mut seen_state, mut last_state, mut unknown, mut exit_zero, mut exit_negative) =
        (0u16, "Unobserved", 0usize, 0usize, 0usize);
    for line in bytes[..complete]
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let Ok(line) = std::str::from_utf8(line) else {
            unknown += 1;
            continue;
        };
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.starts_with("-----BEGIN ") {
            break;
        }
        let Some(state) = line.strip_prefix("SSL_connect:") else {
            unknown += 1;
            continue;
        };
        let state = if let Some(state) = state.strip_prefix("error in ") {
            exit_negative += 1;
            state
        } else if let Some(state) = state.strip_prefix("failed in ") {
            exit_zero += 1;
            state
        } else {
            state
        };
        if let Some((index, (_, name))) = STATES
            .iter()
            .enumerate()
            .find(|(_, (raw, _))| *raw == state)
        {
            seen_state |= 1u16 << index;
            last_state = name;
        } else {
            unknown += 1;
        }
    }
    format!("seen_state={seen_state} last_state={last_state} callback_exit_zero={exit_zero} callback_exit_negative={exit_negative} unobserved_lines={unknown} partial={} truncated={truncated}", complete != bytes.len())
}

// None means not attempted; Some records only the API's success/failure.
// A successful kill does not imply reap or cleanup of unrelated descendants.
#[derive(Default)]
struct ClientLifecycle {
    spawn: Option<bool>,
    stdin_written: Option<bool>,
    exit_success: Option<bool>,
    poll_error: bool,
    kill: Option<bool>,
    reap: Option<bool>,
}
fn stop_client(client: &mut Child, lifecycle: &mut ClientLifecycle) {
    lifecycle.kill = Some(client.kill().is_ok());
    let result = client.wait();
    lifecycle.reap = Some(result.is_ok());
    if let Ok(status) = result {
        lifecycle.exit_success = Some(status.success());
    }
}
fn attempt_diagnostics(
    path: &Path,
    case: &str,
    lifecycle: &ClientLifecycle,
    started: Instant,
) -> String {
    format!(
        "spawn={:?} stdin_written={:?} exit_success={:?} poll_error={} kill={:?} reap={:?}; elapsed_ms={}; {}",
        lifecycle.spawn, lifecycle.stdin_written, lifecycle.exit_success,
        lifecycle.poll_error, lifecycle.kill, lifecycle.reap,
        started.elapsed().as_millis(), client_diagnostics(path, case)
    )
}

fn client_diagnostics(path: &Path, case: &str) -> String {
    let stdout_path = path.join("client.stdout.log");
    let stdout = match (fs::metadata(&stdout_path), read_prefix(&stdout_path, 12)) {
        (Ok(metadata), Ok(prefix)) => format!(
            "{} bytes; HTTP 200 prefix: {}",
            metadata.len(),
            prefix.starts_with(b"HTTP/1.1 200")
        ),
        (Err(error), _) | (_, Err(error)) => format!("unavailable: {error}"),
    };
    let excerpt = |name: &str, limit: u64| match read_prefix(&path.join(name), limit) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            // Starting at byte zero prevents exposing a PEM block's middle.
            text.split("-----BEGIN ").next().unwrap_or("").to_owned()
        }
        Err(error) => format!("unavailable: {error}"),
    };
    let stderr_path = path.join("client.stderr.log");
    let states = match (fs::metadata(&stderr_path), read_prefix(&stderr_path, 2048)) {
        (Ok(metadata), Ok(bytes)) => handshake_state_evidence(&bytes, metadata.len() > 2048),
        _ => "unobserved=stderr_read_error".to_owned(),
    };
    format!(
        "fixture: {case}\nclient stdout: {stdout}\nclient TLS states (bounded prefix, unknown lines omitted):\n{states}\nserver log (up to 8192 bytes, before any PEM):\n{}",
        excerpt("server.log", 8192)
    )
}

fn verified_health(path: &Path, port: u16, case: &str) {
    // Files cannot back up an unread pipe while the parent waits for exit.
    let stdout = fs::File::create(path.join("client.stdout.log")).unwrap();
    let stderr = fs::File::create(path.join("client.stderr.log")).unwrap();
    let started = Instant::now();
    let mut lifecycle = ClientLifecycle::default();
    let spawned = Command::new("openssl")
        .args([
            "s_client",
            "-quiet",
            "-state",
            "-connect",
            &format!("127.0.0.1:{port}"),
            "-CAfile",
            "root.pem",
            "-verify_ip",
            "127.0.0.1",
            "-verify_return_error",
        ])
        .current_dir(path)
        .stdin(Stdio::piped())
        .stdout(stdout)
        .stderr(stderr)
        .spawn();
    lifecycle.spawn = Some(spawned.is_ok());
    let mut client = spawned.unwrap_or_else(|_| {
        panic!(
            "HTTPS client spawn failed; {}",
            attempt_diagnostics(path, case, &lifecycle, started)
        )
    });
    let written = client
        .stdin
        .take()
        .unwrap()
        .write_all(b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    lifecycle.stdin_written = Some(written.is_ok());
    if written.is_err() {
        stop_client(&mut client, &mut lifecycle);
        panic!(
            "HTTPS client stdin failed; {}",
            attempt_diagnostics(path, case, &lifecycle, started)
        );
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while client
        .try_wait()
        .unwrap_or_else(|_| {
            lifecycle.poll_error = true;
            stop_client(&mut client, &mut lifecycle);
            panic!(
                "HTTPS client exit poll failed; {}",
                attempt_diagnostics(path, case, &lifecycle, started)
            )
        })
        .is_none()
    {
        if Instant::now() >= deadline {
            stop_client(&mut client, &mut lifecycle);
            panic!(
                "HTTPS client timeout; {}",
                attempt_diagnostics(path, case, &lifecycle, started)
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
    let reaped = client.wait();
    lifecycle.reap = Some(reaped.is_ok());
    let status = reaped.unwrap_or_else(|_| {
        panic!(
            "HTTPS client reap failed; {}",
            attempt_diagnostics(path, case, &lifecycle, started)
        )
    });
    lifecycle.exit_success = Some(status.success());
    let stdout = read_prefix(&path.join("client.stdout.log"), 12).unwrap();
    assert!(
        status.success(),
        "HTTPS verification failed: {status}\n{}",
        attempt_diagnostics(path, case, &lifecycle, started)
    );
    assert!(
        stdout.starts_with(b"HTTP/1.1 200"),
        "Unexpected HTTPS response: {}",
        attempt_diagnostics(path, case, &lifecycle, started)
    );
}

#[test]
fn client_diagnostics_are_useful_bounded_and_do_not_disclose_payloads() {
    let fixture = Fixture::new();
    let stdout = b"HTTP/1.1 200 OK\r\n\r\nPRIVATE_RESPONSE_BODY";
    fs::write(fixture.0.join("client.stdout.log"), stdout).unwrap();
    for (redacted_log, long_log) in [
        ("client.stderr.log", "server.log"),
        ("server.log", "client.stderr.log"),
    ] {
        fs::write(
            fixture.0.join(redacted_log),
            "before-pem\n-----BEGIN SYNTHETIC DATA-----\nPEM_PAYLOAD_SENTINEL\n-----END SYNTHETIC DATA-----\n",
        )
        .unwrap();
        fs::write(
            fixture.0.join(long_log),
            format!(
                "ordinary diagnostic\n{}LOG_TAIL_SENTINEL",
                "x".repeat(32768)
            ),
        )
        .unwrap();
        let report = client_diagnostics(&fixture.0, "synthetic formatter case");
        assert!(report.contains("synthetic formatter case"));
        assert!(report.contains(&format!("{} bytes", stdout.len())));
        assert!(report.contains("HTTP 200 prefix: true"));
        if redacted_log == "server.log" {
            assert!(report.contains("before-pem"));
        } else {
            assert!(report.contains("ordinary diagnostic"));
        }
        assert!(report.contains("last_state=Unobserved"));
        for hidden in [
            "PRIVATE_RESPONSE_BODY",
            "PEM_PAYLOAD_SENTINEL",
            "LOG_TAIL_SENTINEL",
        ] {
            assert!(!report.contains(hidden), "diagnostic disclosed {hidden}");
        }
        assert!(
            report.len() < 16384,
            "failure diagnostics must stay bounded"
        );
    }
    let evidence = handshake_state_evidence(
        b"SSL_connect:SSLv3/TLS write client hello\r\nSSL_connect:error in SSLv3/TLS read server hello\nSSL_connect:failed in error\nCLIENT_CANARY_URL_SECRET\nSSL_connect:SSL negotiation finished successfully",
        true,
    );
    assert!(evidence.contains("seen_state=32774"));
    assert!(evidence.contains("last_state=AtErrorState"));
    assert!(evidence.contains("callback_exit_zero=1 callback_exit_negative=1"));
    assert!(!evidence
        .split_whitespace()
        .any(|field| field.starts_with("exit_zero=")));
    assert!(!evidence
        .split_whitespace()
        .any(|field| field.starts_with("exit_negative=")));
    assert!(evidence.contains("unobserved_lines=1 partial=true truncated=true"));
    assert!(!evidence.contains("CLIENT_CANARY"));
    assert!(!evidence.contains("AtNegotiated"));
    let after_pem = handshake_state_evidence(
        b"-----BEGIN SYNTHETIC DATA-----\nSSL_connect:SSL negotiation finished successfully\n",
        false,
    );
    assert!(after_pem.contains("seen_state=0 last_state=Unobserved"));
    fs::write(
        fixture.0.join("client.stderr.log"),
        b"CLIENT_CANARY_URL_SECRET\n",
    )
    .unwrap();
    let lifecycle = ClientLifecycle {
        spawn: Some(true),
        stdin_written: Some(true),
        exit_success: Some(false),
        poll_error: false,
        kill: Some(true),
        reap: Some(true),
    };
    let report = attempt_diagnostics(
        &fixture.0,
        "synthetic lifecycle",
        &lifecycle,
        Instant::now(),
    );
    assert!(report.contains("spawn=Some(true)"));
    assert!(report.contains("stdin_written=Some(true)"));
    assert!(report.contains("kill=Some(true) reap=Some(true)"));
    assert!(!report.contains("CLIENT_CANARY"));
}

#[test]
fn weak_supplied_intermediate_and_root_fail_before_any_listener() {
    for (root, intermediate) in [
        ("rsa:2048", Some("rsa:1024")),
        ("rsa:1024", None),
        ("rsa:2048", Some("ec:prime192v1")),
        ("rsa:2048", Some("rsa:2047")),
    ] {
        let fixture = Fixture::new();
        fixture.certificates(root, intermediate, "rsa:2048");
        run_server(&fixture, false, "rejected fixture");
    }
}
#[test]
fn supported_strong_chains_still_serve_verified_https() {
    for intermediate in [None, Some("rsa:2048"), Some("ec:secp224r1"), Some("pss")] {
        let fixture = Fixture::new();
        fixture.certificates("rsa:2048", intermediate, "rsa:2048");
        run_server(
            &fixture,
            true,
            &format!("supported chain: intermediate={intermediate:?}"),
        );
    }
}
#[test]
fn supported_ec_and_ed25519_leaf_keys_still_serve_verified_https() {
    for leaf in ["ec:prime256v1", "ec:secp384r1", "ec:secp521r1", "ed25519"] {
        let fixture = Fixture::new();
        fixture.certificates("rsa:2048", None, leaf);
        run_server(&fixture, true, &format!("supported leaf: {leaf}"));
    }
}

#[test]
fn malformed_certificate_and_nested_key_structures_fail_before_listening() {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use x509_cert::{
        der::{
            asn1::{Any, BitString},
            Decode, Encode,
        },
        Certificate,
    };
    let fixture = Fixture::new();
    fixture.certificates("rsa:2048", Some("rsa:2048"), "rsa:2048");
    fixture.openssl(&[
        "x509",
        "-in",
        "intermediate.pem",
        "-outform",
        "DER",
        "-out",
        "intermediate.der",
    ]);
    let original = fs::read(fixture.0.join("intermediate.der")).unwrap();
    let null = Any::from_der(&[5, 0]).unwrap();
    let mut cases = Vec::new();
    let mut trailing = original.clone();
    trailing.extend([5, 0]);
    cases.push(trailing);
    let mut fields = Vec::<Any>::from_der(&original).unwrap();
    fields.push(null.clone());
    cases.push(fields.to_der().unwrap());
    let mut fields = Vec::<Any>::from_der(&original).unwrap();
    let mut tbs = fields[0].decode_as::<Vec<Any>>().unwrap();
    // The generated v3 certificate has explicit version followed by its six required fields.
    let mut spki = tbs[6].decode_as::<Vec<Any>>().unwrap();
    spki.push(null.clone());
    tbs[6] = Any::encode_from(&spki).unwrap();
    fields[0] = Any::encode_from(&tbs).unwrap();
    cases.push(fields.to_der().unwrap());
    let mut certificate = Certificate::from_der(&original).unwrap();
    let key = &mut certificate
        .tbs_certificate
        .subject_public_key_info
        .subject_public_key;
    let mut rsa = Vec::<Any>::from_der(key.as_bytes().unwrap()).unwrap();
    rsa.push(null.clone());
    *key = BitString::from_bytes(&rsa.to_der().unwrap()).unwrap();
    cases.push(certificate.to_der().unwrap());
    let mut certificate = Certificate::from_der(&original).unwrap();
    certificate
        .tbs_certificate
        .subject_public_key_info
        .algorithm
        .parameters = Some(Any::encode_from(&1u8).unwrap());
    cases.push(certificate.to_der().unwrap());
    for certificate in cases {
        let pem = format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
            STANDARD.encode(certificate)
        );
        let mut chain = fs::read(fixture.0.join("leaf.pem")).unwrap();
        chain.extend(pem.as_bytes());
        chain.extend(fs::read(fixture.0.join("root.pem")).unwrap());
        fs::write(fixture.0.join("chain.pem"), chain).unwrap();
        run_server(&fixture, false, "rejected fixture");
    }
}
