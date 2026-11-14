//! Synthetic trust for the normal-binary production fixture only.
//! No system trust, insecure verification switch, listener or supplier download.
use reqwest::{Certificate, Client};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const COMMAND_LIMIT: Duration = Duration::from_secs(8);
const REQUEST_LIMIT: Duration = Duration::from_secs(3);
const WRONG_HOST: &str = "not-covered.fixture.invalid";

pub(super) struct SyntheticTls {
    root_pem: Vec<u8>,
    leaf: PathBuf,
    key: PathBuf,
    openssl_sha256: String,
    leaf_sha256: String,
    rejected_untrusted: bool,
    rejected_hostname: bool,
}

fn write_private(root: &Path, name: &str, bytes: &[u8]) {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(root.join(name))
        .expect("fresh synthetic TLS fixture file");
    file.write_all(bytes).expect("write synthetic TLS fixture");
    file.sync_all().expect("sync synthetic TLS fixture");
}

fn digest(path: &Path, limit: u64) -> String {
    let file = fs::File::open(path).expect("read owned TLS supplier/artifact");
    assert!(file.metadata().expect("TLS artifact metadata").len() <= limit);
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .expect("bounded TLS artifact read");
    assert!(bytes.len() as u64 <= limit);
    format!("{:x}", Sha256::digest(bytes))
}

fn openssl_path() -> PathBuf {
    let path = if let Some(path) = std::env::var_os("OTTPLAY_MULTIVIEW_OPENSSL") {
        let path = PathBuf::from(path);
        assert!(path.is_absolute(), "configured OpenSSL must be absolute");
        path
    } else {
        std::env::split_paths(&std::env::var_os("PATH").expect("OpenSSL supplier PATH"))
            .filter(|directory| directory.is_absolute())
            .map(|directory| directory.join("openssl"))
            .find(|candidate| candidate.is_file())
            .expect("installed OpenSSL/LibreSSL is required; no download fallback")
    };
    let path = path.canonicalize().expect("canonical OpenSSL supplier");
    let metadata = fs::metadata(&path).expect("OpenSSL supplier metadata");
    let uid = unsafe { libc::geteuid() };
    assert!(
        metadata.is_file()
            && metadata.mode() & 0o111 != 0
            && metadata.mode() & 0o022 == 0
            && (metadata.uid() == 0 || metadata.uid() == uid),
        "OpenSSL supplier must be an owned non-writable executable"
    );
    path
}

// Remain in the outer fixture's owned process group. Do not detach the tool
// from the existing external hard deadline/process-group cleanup owner.
struct CertChild(Option<Child>);
impl Drop for CertChild {
    fn drop(&mut self) {
        let Some(mut child) = self.0.take() else {
            return;
        };
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => {
                let _ = child.kill();
            }
            Err(_) => {
                eprintln!("synthetic TLS child ownership uncertain; outer cleanup required");
                std::mem::forget(child);
                return;
            }
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Err(_) => break,
                Ok(None) => {}
            }
            if Instant::now() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        eprintln!("synthetic TLS child not confirmed reaped; outer cleanup required");
        std::mem::forget(child);
    }
}

fn openssl(tool: &Path, root: &Path, args: &[&str]) {
    let mut child = CertChild(Some(
        Command::new(tool)
            .args(args)
            .current_dir(root)
            .env_clear()
            .env("HOME", root)
            .env("OPENSSL_CONF", root.join("empty.cnf"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start installed synthetic certificate supplier"),
    ));
    let deadline = Instant::now() + COMMAND_LIMIT;
    loop {
        if let Some(status) = child
            .0
            .as_mut()
            .unwrap()
            .try_wait()
            .expect("certificate supplier status")
        {
            assert!(status.success(), "synthetic certificate command failed");
            return;
        }
        assert!(
            Instant::now() < deadline,
            "synthetic certificate command timed out"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

impl SyntheticTls {
    pub(super) fn generate(parent: &Path) -> Self {
        let root = parent.join("synthetic-tls");
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .expect("fresh private synthetic TLS directory");
        let tool = openssl_path();
        let openssl_sha256 = digest(&tool, 32 * 1024 * 1024);
        write_private(&root, "empty.cnf", b"");
        write_private(&root, "root.cnf", b"[req]\ndistinguished_name=dn\nx509_extensions=ca\nprompt=no\n[dn]\nCN=Synthetic production fixture root\n[ca]\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\n");
        openssl(
            &tool,
            &root,
            &[
                "req",
                "-new",
                "-x509",
                "-newkey",
                "ec",
                "-pkeyopt",
                "ec_paramgen_curve:P-256",
                "-nodes",
                "-days",
                "1",
                "-sha256",
                "-config",
                "root.cnf",
                "-keyout",
                "root.key",
                "-out",
                "root.pem",
            ],
        );
        openssl(
            &tool,
            &root,
            &[
                "req",
                "-new",
                "-newkey",
                "ec",
                "-pkeyopt",
                "ec_paramgen_curve:P-256",
                "-nodes",
                "-sha256",
                "-subj",
                "/CN=Synthetic production fixture listener",
                "-keyout",
                "leaf.key",
                "-out",
                "leaf.csr",
            ],
        );
        write_private(&root, "leaf.ext", b"basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\nsubjectAltName=IP:127.0.0.1\n");
        openssl(
            &tool,
            &root,
            &[
                "x509",
                "-req",
                "-sha256",
                "-days",
                "1",
                "-in",
                "leaf.csr",
                "-CA",
                "root.pem",
                "-CAkey",
                "root.key",
                "-set_serial",
                "2",
                "-extfile",
                "leaf.ext",
                "-out",
                "leaf.pem",
            ],
        );
        openssl(
            &tool,
            &root,
            &[
                "verify",
                "-purpose",
                "sslserver",
                "-CAfile",
                "root.pem",
                "leaf.pem",
            ],
        );
        for name in ["root.key", "root.pem", "leaf.key", "leaf.csr", "leaf.pem"] {
            fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o600))
                .expect("private synthetic certificate permissions");
        }
        let mut root_pem = Vec::new();
        fs::File::open(root.join("root.pem"))
            .expect("synthetic CA file")
            .take(64 * 1024 + 1)
            .read_to_end(&mut root_pem)
            .expect("bounded synthetic CA read");
        assert!(root_pem.len() <= 64 * 1024);
        Certificate::from_pem(&root_pem).expect("parse synthetic CA");
        Self {
            root_pem,
            leaf: root.join("leaf.pem"),
            key: root.join("leaf.key"),
            openssl_sha256,
            leaf_sha256: digest(&root.join("leaf.pem"), 64 * 1024),
            rejected_untrusted: false,
            rejected_hostname: false,
        }
    }
    pub(super) fn ca_pem(&self) -> &[u8] {
        &self.root_pem
    }
    pub(super) fn cert_path(&self) -> &Path {
        &self.leaf
    }
    pub(super) fn key_path(&self) -> &Path {
        &self.key
    }

    pub(super) async fn assert_rejections(&mut self, origin: &str) {
        let url = reqwest::Url::parse(origin).expect("synthetic HTTPS origin");
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        let peer = SocketAddr::new(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            url.port_or_known_default().unwrap(),
        );
        let build = || {
            Client::builder()
                .no_proxy()
                .http1_only()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(REQUEST_LIMIT)
                .timeout(REQUEST_LIMIT)
                .tls_built_in_root_certs(false)
        };
        let trusted = build()
            .add_root_certificate(Certificate::from_pem(&self.root_pem).unwrap())
            .build()
            .expect("synthetic CA-only TLS client");
        let positive = trusted
            .get(format!("{origin}/health"))
            .send()
            .await
            .unwrap_or_else(|_| {
                panic!("synthetic CA did not authenticate the actual normal-binary listener")
            });
        assert_eq!(positive.status(), reqwest::StatusCode::OK);
        let untrusted = build().build().expect("untrusted TLS negative client");
        assert!(
            untrusted
                .get(format!("{origin}/health"))
                .send()
                .await
                .is_err(),
            "client without the synthetic root must reject the normal listener"
        );
        self.rejected_untrusted = true;
        let wrong_name = build()
            .add_root_certificate(Certificate::from_pem(&self.root_pem).unwrap())
            .resolve(WRONG_HOST, peer)
            .build()
            .expect("pinned hostname-negative TLS client");
        assert!(
            wrong_name
                .get(format!("https://{WRONG_HOST}:{}/health", peer.port()))
                .send()
                .await
                .is_err(),
            "TLS hostname verification must remain enabled"
        );
        self.rejected_hostname = true;
        assert_eq!(
            trusted
                .get(format!("{origin}/health"))
                .send()
                .await
                .unwrap_or_else(|_| panic!("normal TLS listener failed after negative probes"))
                .status(),
            reqwest::StatusCode::OK
        );
    }
    pub(super) fn receipt(&self) -> Value {
        assert!(
            self.rejected_untrusted && self.rejected_hostname,
            "TLS negative qualification must complete before a pass receipt"
        );
        json!({"configured_api_transport":"https", "trust":"synthetic_ca_only",
            "ca_sha256":format!("{:x}", Sha256::digest(&self.root_pem)),
            "leaf_sha256":self.leaf_sha256,"openssl_sha256":self.openssl_sha256,
            "untrusted_ca_rejected":self.rejected_untrusted,"wrong_hostname_rejected":self.rejected_hostname})
    }
}
