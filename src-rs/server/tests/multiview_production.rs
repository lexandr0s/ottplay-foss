//! Normal-server-binary protocol qualification for the first protected media
//! adapter. This uses private HTTPS control/HLS and an owned HTTP MPEG-TS origin; it is not a native
//! player, UI, television, or visual-compositor qualification.
#![cfg(unix)]

#[path = "support/hls_production.rs"]
mod hls_production;
#[path = "support/multiview_production_config.rs"]
mod production_config;
#[path = "support/multiview_production_tls.rs"]
mod production_tls;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    io::{Read, Write},
    net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::OnceLock,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const STARTUP_LIMIT: Duration = Duration::from_secs(12);
const RUN_LIMIT: Duration = Duration::from_secs(600);
use production_config::{EXPIRING_TOKEN, TOKEN};
const PROFILE: &str = "http-mpegts-v1-640x360-10fps";
const INPUTS: &str = "OTTPLAY_MULTIVIEW_PRODUCTION_INPUTS";
use production_config::ProductionMode;

thread_local! {
    // Bound only this test thread's owned normal-server stderr, never an arbitrary file.
    static FAILURE_STDERR_ROOT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}
struct FailureStderrGuard;
impl FailureStderrGuard {
    fn install(root: &Path) -> Self {
        FAILURE_STDERR_ROOT.with(|slot| {
            assert!(
                slot.borrow().is_none(),
                "one owned normal fixture per thread"
            );
            *slot.borrow_mut() = Some(root.to_owned());
        });
        Self
    }
}
impl Drop for FailureStderrGuard {
    fn drop(&mut self) {
        FAILURE_STDERR_ROOT.with(|slot| *slot.borrow_mut() = None);
    }
}
fn request_stderr_diagnostic() -> String {
    FAILURE_STDERR_ROOT.with(|slot| {
        slot.borrow().as_ref().map_or_else(
            || "stderr_not_registered".to_owned(),
            |root| fixture_stderr_diagnostic(root, &root.join("server.stderr")),
        )
    })
}

fn redacted_startup_tail(bytes: &[u8], root: &Path) -> String {
    // Redact before truncating, so a boundary cannot expose part of a secret.
    let text = String::from_utf8_lossy(bytes)
        .replace(TOKEN, "[control-token]")
        .replace(EXPIRING_TOKEN, "[expiry-token]")
        .replace(
            "production_fixture_signing_key_aaaaaaaaaaaaaaaa",
            "[signing-key]",
        )
        .replace(root.to_string_lossy().as_ref(), "[owned-root]");
    let cleaned = text
        .split_whitespace()
        .map(|word| if word.contains("://") { "[url]" } else { word })
        .collect::<Vec<_>>()
        .join(" ");
    let mut start = cleaned.len().saturating_sub(8192);
    while !cleaned.is_char_boundary(start) {
        start += 1;
    }
    cleaned[start..].to_owned()
}

fn fixture_stderr_diagnostic(root: &Path, path: &Path) -> String {
    let read = || -> std::io::Result<Vec<u8>> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)?;
        let before = file.metadata()?;
        if !before.is_file()
            || before.nlink() != 1
            || before.uid() != unsafe { libc::geteuid() }
            || before.mode() & 0o077 != 0
            || before.len() > 65536
        {
            return Err(std::io::Error::other("stderr shape or bound"));
        }
        let mut bytes = Vec::new();
        (&file).take(65537).read_to_end(&mut bytes)?;
        let after = file.metadata()?;
        let identity = |value: &fs::Metadata| {
            (
                value.dev(),
                value.ino(),
                value.len(),
                value.mode(),
                value.nlink(),
                value.uid(),
                value.mtime(),
                value.mtime_nsec(),
                value.ctime(),
                value.ctime_nsec(),
            )
        };
        if bytes.len() > 65536
            || identity(&before) != identity(&after)
            || identity(&after) != identity(&fs::symlink_metadata(&path)?)
        {
            return Err(std::io::Error::other("stderr changed"));
        }
        Ok(bytes)
    };
    match read() {
        Ok(bytes) => format!(
            "stderr_bytes={} stderr_sha256={:x} stderr_tail={:?}",
            bytes.len(),
            Sha256::digest(&bytes),
            redacted_startup_tail(&bytes, root)
        ),
        Err(_) => "stderr_unavailable_or_outside_owned_bound".to_owned(),
    }
}

#[test]
fn startup_diagnostic_tail_is_bounded_and_redacts_fixture_secrets_and_urls() {
    assert_eq!(request_stderr_diagnostic(), "stderr_not_registered");
    {
        let _guard = FailureStderrGuard::install(Path::new("/nonexistent-owned-fixture"));
        FAILURE_STDERR_ROOT.with(|slot| {
            assert_eq!(
                slot.borrow().as_deref(),
                Some(Path::new("/nonexistent-owned-fixture"))
            );
        });
    }
    assert_eq!(request_stderr_diagnostic(), "stderr_not_registered");
    let root = Path::new("/owned/synthetic-fixture");
    let text=format!("{} {} production_fixture_signing_key_aaaaaaaaaaaaaaaa /owned/synthetic-fixture/operator.json http://fixture.invalid/path?token=hidden \u{1b}[31m invalid source credential policy",TOKEN,EXPIRING_TOKEN);
    let redacted = redacted_startup_tail(text.as_bytes(), root);
    for secret in [
        TOKEN,
        EXPIRING_TOKEN,
        "production_fixture_signing_key_aaaaaaaaaaaaaaaa",
        "/owned/synthetic-fixture",
        "fixture.invalid",
        "token=hidden",
    ] {
        assert!(!redacted.contains(secret));
    }
    assert!(redacted.contains("invalid source credential policy"));
    assert!(!format!("{redacted:?}").contains('\u{1b}'));
    let long = vec![b'x'; 65536];
    assert_eq!(redacted_startup_tail(&long, root).len(), 8192);
}

struct FixtureHttp {
    runtime: tokio::runtime::Runtime,
    client: reqwest::Client,
    https_control_requests: std::sync::atomic::AtomicUsize,
    https_media_requests: std::sync::atomic::AtomicUsize,
}
static FIXTURE_HTTP: OnceLock<FixtureHttp> = OnceLock::new();

fn install_http_client(ca_pem: Option<&[u8]>) {
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(30));
    if let Some(pem) = ca_pem {
        let certificate =
            reqwest::Certificate::from_pem(pem).expect("synthetic fixture CA certificate");
        builder = builder
            .tls_built_in_root_certs(false)
            .add_root_certificate(certificate);
    }
    let client = builder.build().expect("bounded fixture HTTP client");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("fixture HTTP runtime");
    assert!(
        FIXTURE_HTTP
            .set(FixtureHttp {
                runtime,
                client,
                https_control_requests: std::sync::atomic::AtomicUsize::new(0),
                https_media_requests: std::sync::atomic::AtomicUsize::new(0)
            })
            .is_ok(),
        "fixture HTTP client must be installed before first request"
    );
}

fn fixture_http() -> &'static FixtureHttp {
    FIXTURE_HTTP.get_or_init(|| {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(30))
            .build()
            .expect("bounded fixture HTTP client");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("fixture HTTP runtime");
        FixtureHttp {
            runtime,
            client,
            https_control_requests: std::sync::atomic::AtomicUsize::new(0),
            https_media_requests: std::sync::atomic::AtomicUsize::new(0),
        }
    })
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("wall clock after epoch")
        .as_millis() as u64
}

fn private_ip() -> IpAddr {
    // UDP connect only selects the OS route; no datagram is sent. The selected
    // address must be RFC1918, so no public or loopback origin is admitted.
    let socket = UdpSocket::bind("0.0.0.0:0").expect("owned route probe socket");
    socket.connect("192.0.2.1:9").expect("route selection");
    let address = socket.local_addr().expect("selected local address").ip();
    match address {
        IpAddr::V4(value) if value.is_private() && !value.is_loopback() => IpAddr::V4(value),
        _ => panic!("hosted Unix runner did not select an RFC1918 IPv4 address"),
    }
}

fn secure_home() -> PathBuf {
    let home = PathBuf::from(std::env::var_os("HOME").expect("HOME is required"));
    assert!(home.is_absolute(), "HOME must be absolute");
    let home = fs::canonicalize(home).expect("HOME must resolve");
    let uid = unsafe { libc::geteuid() };
    for ancestor in home.ancestors() {
        let metadata = fs::symlink_metadata(ancestor).expect("HOME ancestry inspectable");
        assert!(
            metadata.is_dir()
                && (metadata.uid() == 0 || metadata.uid() == uid)
                && metadata.mode() & 0o022 == 0,
            "HOME ancestry must be private"
        );
    }
    home
}

struct OwnedRoot(PathBuf);
impl OwnedRoot {
    fn new() -> Self {
        let path = secure_home().join(format!(
            ".ottplay-multiview-production-{}",
            uuid::Uuid::new_v4()
        ));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .expect("fresh private fixture root");
        Self(path)
    }
    fn write_private(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .expect("fresh private fixture file");
        file.write_all(bytes).expect("write private fixture file");
        file.sync_all().expect("sync private fixture file");
        path
    }
    fn remove(&mut self) -> std::io::Result<()> {
        fs::remove_dir_all(&self.0)
    }
}
impl Drop for OwnedRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

struct Upstream {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<HashMap<String, usize>>>,
    active: Arc<std::sync::atomic::AtomicUsize>,
    accept: Option<JoinHandle<()>>,
    hls: Option<Arc<hls_production::HlsSources>>,
}
impl Upstream {
    fn start(address: IpAddr, files: HashMap<String, Arc<Vec<u8>>>) -> Self {
        Self::start_mode(address, files, None)
    }
    fn start_mode(
        address: IpAddr,
        files: HashMap<String, Arc<Vec<u8>>>,
        hls: Option<Arc<hls_production::HlsSources>>,
    ) -> Self {
        let listener =
            TcpListener::bind(SocketAddr::new(address, 0)).expect("private fixture listener");
        listener
            .set_nonblocking(true)
            .expect("nonblocking fixture listener");
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(HashMap::new()));
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let worker_stop = stop.clone();
        let worker_requests = requests.clone();
        let worker_active = active.clone();
        let files = Arc::new(files);
        let worker_hls = hls.clone();
        let accept = thread::spawn(move || {
            let workers = Arc::new(Mutex::new(Vec::<JoinHandle<()>>::new()));
            while !worker_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let stop = worker_stop.clone();
                        let requests = worker_requests.clone();
                        let files = files.clone();
                        let hls = worker_hls.clone();
                        let active = worker_active.clone();
                        active.fetch_add(1, Ordering::AcqRel);
                        workers.lock().unwrap().push(thread::spawn(move || {
                            if let Some(hls) = hls {
                                let _ = hls.serve(stream, &stop);
                            } else {
                                let _ = serve_source(stream, &files, &requests, &stop);
                            }
                            active.fetch_sub(1, Ordering::AcqRel);
                        }));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => break,
                }
            }
            for worker in workers.lock().unwrap().drain(..) {
                let _ = worker.join();
            }
        });
        Self {
            address,
            stop,
            requests,
            active,
            accept: Some(accept),
            hls,
        }
    }
    fn request_counts(&self) -> HashMap<String, usize> {
        self.requests.lock().unwrap().clone()
    }
    fn active_connections(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }
    fn source_was_read(&self, mode: ProductionMode) -> bool {
        if mode == ProductionMode::RawMpegTs {
            return self.active_connections() > 0;
        }
        self.hls
            .as_ref()
            .unwrap()
            .audit()
            .segments
            .iter()
            .flatten()
            .any(|count| *count > 0)
    }
    fn confirm_cleanup(&self, output: &Path, stable_hls: bool) {
        let until = Instant::now() + Duration::from_secs(10);
        while fs::read_dir(output).unwrap().next().is_some() || self.active_connections() != 0 {
            assert!(
                Instant::now() < until,
                "source/output custody did not retire"
            );
            thread::sleep(Duration::from_millis(100));
        }
        if let Some(hls) = self.hls.as_ref().filter(|_| stable_hls) {
            let observation = hls.observe_retirement();
            let until = Instant::now() + Duration::from_secs(12);
            while Instant::now() < until {
                assert_eq!(self.active_connections(), 0, "retired HLS body resumed");
                assert!(fs::read_dir(output).unwrap().next().is_none());
                thread::sleep(Duration::from_millis(50));
            }
            hls.confirm_retired_scene(observation, output, self.active_connections());
        }
    }
}
impl Drop for Upstream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.accept.take() {
            let _ = thread.join();
        }
    }
}

fn serve_source(
    mut stream: TcpStream,
    files: &HashMap<String, Arc<Vec<u8>>>,
    requests: &Mutex<HashMap<String, usize>>,
    stop: &AtomicBool,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut request = Vec::with_capacity(1024);
    let mut one = [0u8; 1];
    while request.len() < 16 * 1024 {
        if stream.read(&mut one)? == 0 {
            return Ok(());
        }
        request.push(one[0]);
        if request.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let first = request
        .split(|byte| *byte == b'\n')
        .next()
        .unwrap_or_default();
    let fields: Vec<_> = first.split(|byte| *byte == b' ').collect();
    if fields.len() < 2 || fields[0] != b"GET" {
        return write_not_found(&mut stream);
    }
    let path = std::str::from_utf8(fields[1]).unwrap_or("");
    let Some(name) = path.strip_prefix('/') else {
        return write_not_found(&mut stream);
    };
    let Some(bytes) = files.get(name) else {
        return write_not_found(&mut stream);
    };
    {
        let mut requests = requests.lock().unwrap();
        let count = requests.entry(name.to_owned()).or_default();
        *count += 1;
    }
    write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: video/mp2t\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", bytes.len())?;
    // Every probe or decode subscription receives the same small fast bootstrap
    // prefix, then the remaining original-PTS stream is paced against wall time.
    // Subscription order is deliberately not used to infer the runtime role.
    let started = Instant::now();
    let total = bytes.len();
    let bootstrap = total.saturating_mul(2) / 210;
    for (index, chunk) in bytes.chunks(16 * 1024).enumerate() {
        if stop.load(Ordering::Acquire) {
            break;
        }
        let offset = (index + 1) * 16 * 1024;
        if offset > bootstrap {
            let target = Duration::from_secs(210).mul_f64(offset.min(total) as f64 / total as f64);
            if target > started.elapsed() {
                thread::sleep(target - started.elapsed());
            }
        }
        if stream.write_all(chunk).is_err() {
            break;
        }
    }
    Ok(())
}
fn write_not_found(stream: &mut TcpStream) -> std::io::Result<()> {
    stream.write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
}

fn http(
    method: &str,
    url: &str,
    token: Option<&str>,
    request_id: Option<&str>,
    body: Option<&Value>,
    limit: usize,
) -> (u16, Vec<u8>) {
    let fixture = fixture_http();
    let timeout = if method == "POST" {
        Duration::from_secs(30)
    } else {
        Duration::from_secs(8)
    };
    fixture.runtime.block_on(async {
        let operation = async {
            let mut request = fixture
                .client
                .request(
                    reqwest::Method::from_bytes(method.as_bytes()).expect("fixed HTTP method"),
                    url,
                )
                .timeout(timeout);
            if let Some(token) = token {
                request = request.bearer_auth(token);
            }
            if let Some(id) = request_id {
                request = request.header("x-request-id", id);
            }
            if let Some(value) = body {
                request = request.json(value);
            }
            let Ok(mut response) = request.send().await else {
                return (0, Vec::new());
            };
            if response
                .content_length()
                .is_some_and(|size| size > limit as u64)
            {
                panic!("fixture response exceeds byte limit");
            }
            let status = response.status().as_u16();
            let mut bytes = Vec::new();
            loop {
                let chunk = match response.chunk().await {
                    Ok(Some(chunk)) => chunk,
                    Ok(None) => break,
                    Err(_) => return (0, Vec::new()),
                };
                if bytes.len().saturating_add(chunk.len()) > limit {
                    panic!("fixture response exceeds byte limit");
                }
                bytes.extend_from_slice(&chunk);
            }
            (status, bytes)
        };
        tokio::time::timeout(timeout, operation)
            .await
            .unwrap_or((0, Vec::new()))
    })
}

fn api_error_diagnostic(endpoint: &str, request_id: &str, bytes: &[u8]) -> String {
    // Error responses are closed; never print arbitrary response authority fields.
    let parsed = if bytes.len() <= 8192 {
        serde_json::from_slice::<Value>(bytes).ok()
    } else {
        None
    };
    let code = parsed
        .as_ref()
        .filter(|value| value["version"] == 1)
        .and_then(|value| value["error"].as_str())
        .filter(|code| {
            matches!(
                *code,
                "backend_unavailable"
                    | "unsupported_platform"
                    | "unauthorized"
                    | "forbidden"
                    | "invalid_request"
                    | "revision_conflict"
                    | "resource_limit"
                    | "authority_revoked"
            )
        })
        .unwrap_or("unrecognized_or_oversized_error");
    let projected = serde_json::to_vec(&json!({"version":1,"error":code})).unwrap();
    format!("operation={endpoint:?} request_id_sha256={:x} response_bytes={} response_sha256={:x} response={:?}",
        Sha256::digest(request_id.as_bytes()),bytes.len(),Sha256::digest(bytes),
        redacted_startup_tail(&projected,Path::new("/fixture-owned-root")))
}

#[test]
fn api_failure_diagnostics_bind_request_hash_and_closed_redacted_response() {
    let request = "secret-request-marker";
    let bytes = br#"{"version":1,"error":"invalid_request","input_grant":"secret-response-grant"}"#;
    let diagnostic = api_error_diagnostic("sessions", request, bytes);
    assert!(
        diagnostic.contains("invalid_request")
            && diagnostic.contains(&format!("{:x}", Sha256::digest(request.as_bytes())))
    );
    assert!(!diagnostic.contains(request) && !diagnostic.contains("secret-response-grant"));
    let malicious = format!("{{\"version\":1,\"error\":\"{}\"}}", TOKEN);
    assert!(!api_error_diagnostic("sessions", request, malicious.as_bytes()).contains(TOKEN));
    assert!(api_error_diagnostic("sessions", request, &vec![b'x'; 8193]).contains("oversized"));
}

fn post(base: &str, endpoint: &str, token: &str, id: &str, body: &Value) -> Value {
    let (status, bytes) = http(
        "POST",
        &format!("{base}/multiview/v1/{endpoint}"),
        Some(token),
        Some(id),
        Some(body),
        2 * 1024 * 1024,
    );
    if reqwest::Url::parse(base).is_ok_and(|url| url.scheme() == "https") {
        fixture_http()
            .https_control_requests
            .fetch_add(1, Ordering::Relaxed);
    }
    if status != 200 {
        // Preserve the closed startup record before panic unwinds OwnedRoot.
        eprintln!(
            "normal_server_request_failure {}",
            request_stderr_diagnostic()
        );
    }
    assert_eq!(
        status,
        200,
        "private API operation {endpoint} returned HTTP {status}; {}",
        api_error_diagnostic(endpoint, id, &bytes)
    );
    serde_json::from_slice(&bytes).expect("valid private API response JSON")
}
fn get_url(url: &str, token: Option<&str>, limit: usize) -> (u16, Vec<u8>) {
    let response = http("GET", url, token, None, None, limit);
    if response.0 == 200 && reqwest::Url::parse(url).is_ok_and(|parsed| parsed.scheme() == "https")
    {
        fixture_http()
            .https_media_requests
            .fetch_add(1, Ordering::Relaxed);
    }
    response
}
fn get(base: &str, path: &str, token: Option<&str>, limit: usize) -> (u16, Vec<u8>) {
    let url = if path.starts_with("http://") || path.starts_with("https://") {
        path.to_owned()
    } else {
        format!("{base}{path}")
    };
    let response = http("GET", &url, token, None, None, limit);
    if reqwest::Url::parse(base).is_ok_and(|parsed| parsed.scheme() == "https") {
        fixture_http()
            .https_control_requests
            .fetch_add(1, Ordering::Relaxed);
    }
    response
}

fn assert_no_untrusted_cors(base: &str, path: &str) {
    let fixture = fixture_http();
    let url = format!("{base}{path}");
    let response = fixture.runtime.block_on(async {
        tokio::time::timeout(
            Duration::from_secs(5),
            fixture
                .client
                .request(reqwest::Method::OPTIONS, url)
                .header("Origin", "https://untrusted.example.test")
                .header("Access-Control-Request-Method", "GET")
                .timeout(Duration::from_secs(5))
                .send(),
        )
        .await
        .expect("bounded HTTPS CORS preflight")
        .expect("CORS preflight transport")
    });
    assert!(
        !response
            .headers()
            .contains_key("access-control-allow-origin"),
        "multiview-only reflected an untrusted CORS origin"
    );
}

fn status(base: &str, token: &str, context: &Value, id: &str) -> Value {
    post(
        base,
        "status",
        token,
        id,
        &json!({
            "version": 1,
            "server_epoch": context["server_epoch"],
            "session_id": context["session_id"],
            "native_expected": context["native_expected"],
        }),
    )
}

fn context_for_slot(status: &Value, slot_id: u64, native: &Value) -> Value {
    // Rebase each known slot identity on fresh session-level native revisions
    // from status. Cached per-slot bindings carry only that slot's generation
    // and source revision and may otherwise be stale after focus/audio changes.
    let mut current_native = status["native_expected"].clone();
    for field in ["slot_id", "slot_generation", "source_revision"] {
        current_native[field] = native[field].clone();
    }
    json!({"server_epoch":status["server_epoch"],"session_id":status["session_id"],
        "backend_expected":status["backend_expected"],"native_expected":current_native,
        "backend_slot_revision":slot(status, slot_id)["source_revision"]})
}
fn contexts(status: &Value) -> (Value, Value) {
    let first = json!({
        "server_epoch": status["server_epoch"],
        "session_id": status["session_id"],
        "backend_expected": status["backend_expected"],
        "native_expected": status["native_expected"],
        "backend_slot_revision": 0,
    });
    let mut second = first.clone();
    second["native_expected"]["slot_id"] = json!(1);
    second["native_expected"]["slot_generation"] = json!("native_slot_1");
    second["native_expected"]["source_revision"] = json!("native_source_1");
    second["backend_slot_revision"] = json!(0);
    (first, second)
}

fn catalog(base: &str, token: &str, context: &Value, id: &str) -> Value {
    post(
        base,
        "sources",
        token,
        id,
        &json!({"version":1,"context":context,"cursor":null}),
    )
}

fn grant(
    base: &str,
    token: &str,
    context: &Value,
    item: &Value,
    catalog_revision: &Value,
    suffix: &str,
) -> Value {
    let mut next = context["native_expected"].clone();
    next["source_revision"] = json!(format!("source_{suffix}"));
    post(
        base,
        "input-grants",
        token,
        &format!("grant_{suffix}"),
        &json!({
            "version":1,"context":context,"next_native":next,
            "source_ref":item["source_ref"],"item_ref":item["item_ref"],"item_revision":item["item_revision"],
            "identity_version":item["identity_version"],"favorite_sha256":item["favorite_sha256"],
            "catalog_revision":catalog_revision,"remaining_budget_ms":30000,
        }),
    )
}

fn control(
    base: &str,
    token: &str,
    context: &Value,
    id: &str,
    op: Value,
    next: Option<Value>,
) -> Value {
    post(
        base,
        "control",
        token,
        id,
        &json!({"version":1,"context":context,"next_native":next,"op":op}),
    )
}

fn create_session(base: &str, token: &str, server_epoch: &Value, native: Value, id: &str) -> Value {
    let geometry = disjoint_geometry(2);
    post(
        base,
        "sessions",
        token,
        id,
        &json!({"version":1,"server_epoch":server_epoch,
        "native_expected":native,"output_profile_id":PROFILE,"geometry":geometry}),
    )
}

fn disjoint_geometry(slot_count: usize) -> Value {
    let rects = if slot_count == 2 {
        vec![
            json!({"slot_id":0,"x":0,"y":0,"width":320,"height":360,"z":0}),
            json!({"slot_id":1,"x":320,"y":0,"width":320,"height":360,"z":1}),
        ]
    } else {
        assert_eq!(slot_count, 9);
        (0..9)
            .map(|slot| {
                let column = slot % 3;
                let row = slot / 3;
                let x = [0, 214, 428][column];
                let width = [214, 214, 212][column];
                json!({"slot_id":slot,"x":x,"y":row * 120,"width":width,"height":120,"z":slot})
            })
            .collect()
    };
    json!({"canvas":{"width":640,"height":360,"fps":10},"revision":1,"mode":"disjoint","rects":rects})
}

fn validate_fixture_geometry(
    value: Value,
) -> Result<
    ottplay_core::multiview::geometry::ValidatedGeometry,
    ottplay_core::multiview::geometry::GeometryError,
> {
    use ottplay_core::multiview::{
        broker::wire::Geometry,
        geometry::{CanvasSpec, ImportedRect, OverlapPolicy, ValidatedGeometry},
    };
    let value: Geometry =
        serde_json::from_value(value).expect("fixture geometry uses actual production DTO");
    let rects: Vec<_> = value
        .rects
        .iter()
        .map(|r| ImportedRect {
            slot_id: r.slot_id,
            x: r.x,
            y: r.y,
            width: r.width,
            height: r.height,
            z: r.z,
        })
        .collect();
    ValidatedGeometry::new(
        CanvasSpec {
            width: value.canvas.width,
            height: value.canvas.height,
            fps: value.canvas.fps,
        },
        value.revision,
        &rects,
        OverlapPolicy::Disjoint,
    )
}

#[test]
fn two_slot_fixture_geometry_passes_actual_core_validation() {
    let geometry = validate_fixture_geometry(disjoint_geometry(2)).unwrap();
    assert_eq!(geometry.rects().len(), 2);
    assert_eq!(geometry.canvas().width, 640);
}

#[test]
fn nine_slot_fixture_geometry_passes_actual_core_and_old_odd_grid_is_rejected() {
    use ottplay_core::multiview::geometry::GeometryError;
    let geometry = validate_fixture_geometry(disjoint_geometry(9)).unwrap();
    assert_eq!(geometry.rects().len(), 9);
    assert_eq!(
        geometry
            .rects()
            .iter()
            .map(|r| u64::from(r.width) * u64::from(r.height))
            .sum::<u64>(),
        640 * 360
    );
    assert_eq!(
        geometry
            .rects()
            .iter()
            .map(|r| (r.slot_id, r.z))
            .collect::<Vec<_>>(),
        (0..9).map(|id| (id, id)).collect::<Vec<_>>()
    );
    let mut old = disjoint_geometry(9);
    old["rects"][1]["width"] = json!(213);
    assert_eq!(
        validate_fixture_geometry(old).err(),
        Some(GeometryError::InvalidRect)
    );
}

fn native_for_slot(slot: u64) -> Value {
    json!({"runtime":"native_runtime_shared","expected":{
        "scene":"native_scene_shared","layout_revision":"layout_shared",
        "focus_revision":"focus_shared","audio_revision":"audio_shared"},
        "slot_id":slot,"slot_generation":format!("native_slot_{slot}"),
        "source_revision":format!("native_source_{slot}")})
}

fn create_session_slots(
    base: &str,
    token: &str,
    server_epoch: &Value,
    slot_count: usize,
    id: &str,
) -> Value {
    post(
        base,
        "sessions",
        token,
        id,
        &json!({"version":1,"server_epoch":server_epoch,
        "native_expected":native_for_slot(0),"output_profile_id":PROFILE,
        "geometry":disjoint_geometry(slot_count)}),
    )
}

fn snapshot(base: &str, token: &str, binding: &Value, id: &str) -> Value {
    status(base, token, binding, id)
}

fn slot<'a>(snapshot: &'a Value, id: u64) -> &'a Value {
    snapshot["slots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|slot| slot["slot_id"] == id)
        .expect("slot status")
}

fn request_source(
    base: &str,
    token: &str,
    ctx: &Value,
    selected: &Value,
    catalog_revision: &Value,
    suffix: &str,
) -> Value {
    let reply = grant(base, token, ctx, selected, catalog_revision, suffix);
    let started = control(
        base,
        token,
        &reply["context"],
        &format!("play_{suffix}"),
        json!({"name":"play","input_grant":reply["input_grant"]}),
        None,
    );
    started["context"].clone()
}

fn wait_slot(
    base: &str,
    token: &str,
    binding: &Value,
    id: u64,
    expected_item: &str,
    deadline: Instant,
) -> Value {
    loop {
        let current = snapshot(base, token, binding, "status_wait");
        let value = slot(&current, id);
        if value["state"] == "playing"
            && value["frames"].as_u64().unwrap_or(0) >= 2
            && value["source_ref"].as_str().is_some()
        {
            assert_eq!(
                value["item_ref"], expected_item,
                "slot reported a different granted source"
            );
            return current;
        }
        assert!(
            Instant::now() < deadline,
            "slot failed to reach Playing before bounded deadline"
        );
        thread::sleep(Duration::from_millis(250));
    }
}

fn renew_due(
    base: &str,
    token: &str,
    latest: &mut Value,
    slot_natives: &mut [Value],
    next_renew: &mut Instant,
    sequence: &mut u32,
) {
    if Instant::now() < *next_renew {
        return;
    }
    let current = snapshot(base, token, latest, "bounded_renew_status");
    let Some(slot_id) = current["slots"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|value| {
            let state = value["state"].as_str().unwrap_or("");
            let id = value["slot_id"].as_u64()?;
            matches!(state, "starting" | "preparing" | "playing" | "stalled").then_some(id)
        })
    else {
        *next_renew = Instant::now() + Duration::from_secs(18);
        return;
    };
    let context = context_for_slot(&current, slot_id, &slot_natives[slot_id as usize]);
    let response = control(
        base,
        token,
        &context,
        &format!("renew_{}", *sequence),
        json!({"name":"renew"}),
        None,
    );
    *sequence += 1;
    *latest = response["context"].clone();
    slot_natives[slot_id as usize] = latest["native_expected"].clone();
    *next_renew = Instant::now() + Duration::from_secs(18);
}

fn wait_all_slots(
    base: &str,
    token: &str,
    latest: &mut Value,
    slot_natives: &mut [Value],
    expected: &[String],
    next_renew: &mut Instant,
    renew_sequence: &mut u32,
    deadline: Instant,
) -> Value {
    loop {
        renew_due(
            base,
            token,
            latest,
            slot_natives,
            next_renew,
            renew_sequence,
        );
        let current = snapshot(base, token, latest, "status_wait_grid");
        let ready = expected.iter().enumerate().all(|(index, item)| {
            let value = slot(&current, index as u64);
            value["state"] == "playing"
                && value["frames"].as_u64().unwrap_or(0) >= 10
                && value["item_ref"] == item.as_str()
                && value["source_ref"].as_str().is_some()
        });
        if ready {
            return current;
        }
        assert!(
            Instant::now() < deadline,
            "all nine slots did not decode ten owned frames"
        );
        thread::sleep(Duration::from_millis(250));
    }
}

fn playlist_snapshot(base: &str, playlist_path: &str) -> Option<(u64, Vec<String>)> {
    let playlist_url =
        if playlist_path.starts_with("http://") || playlist_path.starts_with("https://") {
            reqwest::Url::parse(playlist_path).ok()?
        } else {
            reqwest::Url::parse(&format!("{base}{playlist_path}")).ok()?
        };
    let expected = reqwest::Url::parse(base).ok()?;
    assert_eq!(
        (
            playlist_url.scheme(),
            playlist_url.host_str(),
            playlist_url.port_or_known_default()
        ),
        (
            expected.scheme(),
            expected.host_str(),
            expected.port_or_known_default()
        ),
        "HLS playlist escaped the selected API/media listener origin"
    );
    assert_eq!(
        playlist_url.scheme(),
        "https",
        "HLS media must use the selected HTTPS listener"
    );
    let (code, playlist) = get_url(playlist_url.as_str(), None, 1024 * 1024);
    if code != 200 {
        return None;
    }
    let text = std::str::from_utf8(&playlist).ok()?;
    let sequence = text
        .lines()
        .find_map(|line| line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:"))?
        .parse()
        .ok()?;
    let segments = text
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| playlist_url.join(line).ok().map(|url| url.to_string()))
        .collect::<Option<Vec<_>>>()?;
    for url in &segments {
        let parsed = reqwest::Url::parse(url).ok()?;
        assert_eq!(
            (
                parsed.scheme(),
                parsed.host_str(),
                parsed.port_or_known_default()
            ),
            (
                expected.scheme(),
                expected.host_str(),
                expected.port_or_known_default()
            ),
            "HLS segment escaped the selected API/media listener origin"
        );
    }
    Some((sequence, segments))
}

fn fetch_segment(base: &str, url: &str) -> Option<Vec<u8>> {
    let expected = reqwest::Url::parse(base).ok()?;
    let parsed = reqwest::Url::parse(url).ok()?;
    if (
        parsed.scheme(),
        parsed.host_str(),
        parsed.port_or_known_default(),
    ) != (
        expected.scheme(),
        expected.host_str(),
        expected.port_or_known_default(),
    ) {
        return None;
    }
    let (status, bytes) = get_url(url, None, 8 * 1024 * 1024);
    (status == 200 && bytes.len() >= 188).then_some(bytes)
}

fn try_new_segment(
    base: &str,
    playlist: &str,
    seen: &mut HashSet<String>,
) -> Option<(u64, String, Vec<u8>)> {
    let (sequence, segments) = playlist_snapshot(base, playlist)?;
    for (index, url) in segments.iter().enumerate() {
        if !seen.contains(url) {
            if let Some(bytes) = fetch_segment(base, url) {
                seen.insert(url.clone());
                return Some((sequence.saturating_add(index as u64), url.clone(), bytes));
            }
        }
    }
    None
}

fn wait_new_segment(
    base: &str,
    playlist: &str,
    seen: &mut HashSet<String>,
    deadline: Instant,
) -> (u64, String, Vec<u8>) {
    loop {
        if let Some(segment) = try_new_segment(base, playlist, seen) {
            return segment;
        }
        assert!(
            Instant::now() < deadline,
            "new private HLS segment did not become readable"
        );
        thread::sleep(Duration::from_millis(250));
    }
}

fn run_bounded_filter(
    tool: &Path,
    args: &[&str],
    input: &[u8],
    output_limit: usize,
    allow_early_input_close: bool,
) -> (std::process::ExitStatus, Vec<u8>) {
    assert!(
        input.len() <= 8 * 1024 * 1024 && output_limit <= 2 * 1024 * 1024,
        "bounded media filter inputs required"
    );
    let child = Command::new(tool)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("configured media inspection tool");
    let mut child = ChildGuard(child);
    let mut stdin = child.0.stdin.take().expect("filter stdin pipe");
    let stdout = child.0.stdout.take().expect("filter stdout pipe");
    let input = input.to_vec();
    let writer = thread::spawn(move || stdin.write_all(&input));
    let reader = thread::spawn(move || {
        let mut stdout = stdout;
        let mut output = Vec::with_capacity(output_limit.min(256 * 1024));
        let mut overflow = false;
        let mut chunk = [0u8; 16 * 1024];
        loop {
            let read = stdout.read(&mut chunk)?;
            if read == 0 {
                break;
            }
            let available = output_limit.saturating_add(1).saturating_sub(output.len());
            let keep = read.min(available);
            output.extend_from_slice(&chunk[..keep]);
            overflow |= keep != read;
        }
        Ok::<_, std::io::Error>((output, overflow))
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.0.try_wait().expect("media filter child status") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.0.kill();
            let _ = child.0.wait();
            panic!("bounded media filter timed out");
        }
        thread::sleep(Duration::from_millis(10));
    };
    let write_result = writer.join().expect("media filter input writer thread");
    let (output, overflow) = reader
        .join()
        .expect("media filter output reader thread")
        .expect("read bounded media filter output");
    assert!(
        status.success(),
        "configured media filter rejected owned HLS bytes"
    );
    assert!(
        !overflow && output.len() <= output_limit,
        "media filter output exceeded byte limit"
    );
    if let Err(error) = write_result {
        assert!(
            allow_early_input_close && error.kind() == std::io::ErrorKind::BrokenPipe,
            "media filter closed its input unexpectedly"
        );
    }
    (status, output)
}

fn ffprobe_segment(ffprobe: &Path, bytes: &[u8]) {
    assert!(
        bytes.len() >= 188 && bytes.len() <= 8 * 1024 * 1024,
        "bounded TS output segment required"
    );
    let (_, output) = run_bounded_filter(
        ffprobe,
        &[
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type,codec_name,width,height,r_frame_rate,avg_frame_rate",
            "-of",
            "json",
            "-i",
            "pipe:0",
        ],
        bytes,
        256 * 1024,
        false,
    );
    let report: Value = serde_json::from_slice(&output).expect("ffprobe JSON");
    let streams = report["streams"].as_array().expect("ffprobe streams");
    let video = streams
        .iter()
        .find(|row| row["codec_type"] == "video")
        .expect("HLS video track");
    let audio = streams
        .iter()
        .find(|row| row["codec_type"] == "audio")
        .expect("HLS audio track");
    assert_eq!(
        video["codec_name"], "h264",
        "selected profile video codec differs"
    );
    assert_eq!(
        (video["width"].as_u64(), video["height"].as_u64()),
        (Some(640), Some(360)),
        "selected profile video dimensions differ"
    );
    assert!(
        video["r_frame_rate"] == "10/1" || video["avg_frame_rate"] == "10/1",
        "selected profile frame rate differs"
    );
    assert_eq!(
        audio["codec_name"], "aac",
        "selected profile audio codec differs"
    );
}

fn decode_segment_rgb(ffmpeg: &Path, bytes: &[u8]) -> Vec<u8> {
    assert!(
        bytes.len() >= 188 && bytes.len() <= 8 * 1024 * 1024,
        "bounded TS output segment required"
    );
    let (_, output) = run_bounded_filter(
        ffmpeg,
        &[
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            "pipe:0",
            "-vf",
            r"select=gte(n\,10)",
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "pipe:1",
        ],
        bytes,
        2 * 1024 * 1024,
        true,
    );
    assert_eq!(
        output.len(),
        640 * 360 * 3,
        "HLS frame dimensions differ from selected profile"
    );
    output
}

fn decode_segment_pcm(ffmpeg: &Path, bytes: &[u8]) -> Vec<i16> {
    assert!(
        bytes.len() >= 188 && bytes.len() <= 8 * 1024 * 1024,
        "bounded TS output segment required"
    );
    let (_, output) = run_bounded_filter(
        ffmpeg,
        &[
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            "pipe:0",
            "-vn",
            "-ac",
            "1",
            "-ar",
            "16000",
            "-f",
            "s16le",
            "pipe:1",
        ],
        bytes,
        512 * 1024,
        false,
    );
    assert!(
        output.len() >= 16_000 * 2 && output.len() <= 16_000 * 2 * 8,
        "decoded HLS PCM length outside bounded interval"
    );
    output
        .chunks_exact(2)
        .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
        .collect()
}

fn tone_power(samples: &[i16], frequency: f64) -> f64 {
    let (mut sin, mut cos) = (0.0, 0.0);
    for (index, sample) in samples.iter().take(16_000).enumerate() {
        let phase = std::f64::consts::TAU * frequency * index as f64 / 16_000.0;
        sin += f64::from(*sample) * phase.sin();
        cos += f64::from(*sample) * phase.cos();
    }
    sin * sin + cos * cos
}

fn assert_audio_tone(samples: &[i16], expected: f64) {
    assert!(
        audio_tone_matches(samples, expected),
        "selected output audio does not contain the granted source tone"
    );
}

fn audio_tone_matches(samples: &[i16], expected: f64) -> bool {
    let selected = tone_power(samples, expected);
    let alternatives = [330.0, 440.0, 550.0]
        .into_iter()
        .filter(|value| *value != expected)
        .map(|value| tone_power(samples, value))
        .fold(0.0, f64::max);
    selected > alternatives * 4.0 && selected > 1.0e10
}

fn assert_cell_color(rgb: &[u8], x: usize, y: usize, color_id: u8) {
    assert!(
        cell_matches_color(rgb, x, y, color_id),
        "slot cell center does not match its authorized source color"
    );
}

fn cell_matches_color(rgb: &[u8], x: usize, y: usize, color_id: u8) -> bool {
    let offset = (y * 640 + x) * 3;
    let (r, g, b) = (rgb[offset], rgb[offset + 1], rgb[offset + 2]);
    match color_id {
        1 => r > 140 && g < 90 && b < 90,
        2 => g > 140 && r < 90 && b < 90,
        3 => b > 140 && r < 90 && g < 90,
        _ => false,
    }
}

fn source_input_manifest(path: &Path) -> Vec<(u8, PathBuf, String, u64)> {
    let metadata = fs::symlink_metadata(path).expect("input manifest");
    assert!(
        metadata.is_file()
            && !metadata.file_type().is_symlink()
            && metadata.mode() & 0o222 == 0
            && metadata.len() <= 16 * 1024
    );
    let manifest: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(manifest["schema"], "ottplay.multiview.production-inputs.v1");
    assert_eq!(manifest["seconds"], 210);
    assert_eq!(manifest["fps"], 10);
    assert_eq!(manifest["width"], 96);
    assert_eq!(manifest["height"], 64);
    let entries = manifest["inputs"].as_array().unwrap();
    assert_eq!(entries.len(), 3);
    let colors = ["red", "lime", "blue"];
    let frequencies = [330, 440, 550];
    let mut result = Vec::new();
    for (index, item) in entries.iter().enumerate() {
        assert_eq!(item["id"], index as u64 + 1);
        assert_eq!(item["color"], colors[index]);
        assert_eq!(item["audio_frequency_hz"], frequencies[index]);
        let path = PathBuf::from(item["path"].as_str().unwrap());
        let metadata = fs::symlink_metadata(&path).expect("source file");
        assert!(
            metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata.mode() & 0o222 == 0
                && metadata.len() > 0
                && metadata.len() <= 16 * 1024 * 1024
        );
        let bytes = fs::read(&path).expect("owned source file");
        let digest = format!("{:x}", Sha256::digest(&bytes));
        assert_eq!(digest, item["sha256"].as_str().unwrap());
        assert_eq!(metadata.len(), item["bytes"].as_u64().unwrap());
        result.push((index as u8 + 1, path, digest, metadata.len()));
    }
    result
}

fn start_single_playing_context(
    base: &str,
    epoch: &Value,
    label: &str,
) -> (Value, String, String, String) {
    let mut native = native_for_slot(0);
    native["runtime"] = json!("native_runtime_sequential");
    native["expected"]["scene"] = json!(format!("scene_{label}"));
    native["slot_generation"] = json!(format!("slot_{label}"));
    native["source_revision"] = json!(format!("source_{label}"));
    let created = create_session(base, TOKEN, epoch, native, &format!("create_{label}"));
    let (ctx, _) = contexts(&created);
    let list = catalog(base, TOKEN, &ctx, &format!("catalog_{label}"));
    let ctx = request_source(
        base,
        TOKEN,
        &list["context"],
        &list["items"][0],
        &list["catalog_revision"],
        label,
    );
    let end = Instant::now() + Duration::from_secs(12);
    let mut current = wait_slot(base, TOKEN, &ctx, 0, "production_item_1", end);
    loop {
        if let (Some(path), Some(generation)) = (
            current["output"]["playlist_path"].as_str(),
            current["output"]["output_generation"].as_str(),
        ) {
            let path = path.to_owned();
            let generation = generation.to_owned();
            let (_, _, bytes) = wait_new_segment(base, &path, &mut HashSet::new(), end);
            assert!(
                bytes.len() >= 188 * 3
                    && [0, 188, 376]
                        .into_iter()
                        .all(|offset| bytes[offset] == 0x47)
            );
            let binding = context_for_slot(&current, 0, &ctx["native_expected"]);
            return (
                binding,
                path,
                current["session_id"]
                    .as_str()
                    .expect("owned session")
                    .to_owned(),
                generation,
            );
        }
        assert!(
            Instant::now() < end,
            "context never minted actual readable output"
        );
        thread::sleep(Duration::from_millis(100));
        current = snapshot(base, TOKEN, &ctx, &format!("output_{label}"));
    }
}

#[test]
#[ignore = "hosted production binary; private HTTP MPEG-TS, 210-second decode and normal HLS output qualification"]
fn normal_server_private_http_mpegts_renew_replace_and_cleanup() {
    run_normal_production(ProductionMode::RawMpegTs);
}
#[test]
#[ignore = "hosted production binary; private live HLS MPEG-TS input, real HTTPS, 185s strict sustained decode and owned cleanup"]
fn normal_server_private_http_hls_renew_replace_and_cleanup() {
    run_normal_production(ProductionMode::LiveHlsTs);
}
fn run_normal_production(mode: ProductionMode) {
    let started = Instant::now();
    let mut root = OwnedRoot::new();
    let _failure_stderr = FailureStderrGuard::install(&root.0);
    let mut tls = production_tls::SyntheticTls::generate(&root.0);
    install_http_client(Some(tls.ca_pem()));
    let public = root.0.join("public");
    fs::DirBuilder::new().mode(0o700).create(&public).unwrap();
    let inputs_path = PathBuf::from(std::env::var_os(INPUTS).expect("sealed source manifest"));
    let hls_sources = if mode == ProductionMode::LiveHlsTs {
        let path = PathBuf::from(
            std::env::var_os("OTTPLAY_MULTIVIEW_PRODUCTION_HLS_INPUTS")
                .expect("sealed HLS source manifest"),
        );
        Some(Arc::new(hls_production::HlsSources::load(&path)))
    } else {
        None
    };
    let inputs = if let Some(hls) = &hls_sources {
        hls.original_inputs()
    } else {
        source_input_manifest(&inputs_path)
    };
    let files: HashMap<String, Arc<Vec<u8>>> = inputs
        .iter()
        .map(|(id, path, _, _)| {
            let bytes = fs::read(path).expect("owned source bytes");
            (format!("source-{id}.ts"), Arc::new(bytes))
        })
        .collect();
    let private_peer = private_ip();
    let upstream = if mode == ProductionMode::RawMpegTs {
        Upstream::start(private_peer, files)
    } else {
        Upstream::start_mode(private_peer, files, hls_sources)
    };
    let source_origin = format!("http://{}:{}", private_peer, upstream.address.port());
    let api_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let decoy_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let api_port = api_listener.local_addr().unwrap().port();
    let decoy_port = decoy_listener.local_addr().unwrap().port();
    let api_port_text = api_port.to_string();
    let decoy_port_text = decoy_port.to_string();
    let api_origin = format!("https://127.0.0.1:{api_port}");
    let expires = now_ms() + 1_200_000;
    let short_expires = now_ms() + 60_000;
    let ffmpeg = std::env::var("OTTPLAY_MULTIVIEW_FFMPEG").expect("configured ffmpeg");
    let ffprobe = std::env::var("OTTPLAY_MULTIVIEW_FFPROBE").expect("configured ffprobe");
    let mkfifo = std::env::var("OTTPLAY_MULTIVIEW_MKFIFO").expect("configured mkfifo");
    let output = root.0.join("media-output");
    fs::DirBuilder::new().mode(0o700).create(&output).unwrap();
    let config = production_config::Config {
        mode,
        source_origin: &source_origin,
        private_peer,
        inputs: &inputs,
        api_origin: &api_origin,
        expires,
        short_expires,
        output: &output,
        ffmpeg: &ffmpeg,
        ffprobe: &ffprobe,
        mkfifo: &mkfifo,
    }
    .document();
    let config_path = root.write_private("operator.json", &serde_json::to_vec(&config).unwrap());
    drop((api_listener, decoy_listener));
    let mut server = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_ottplay-server"))
            .current_dir(&public)
            .env_clear()
            .env("HOME", secure_home())
            .env("EPG_URLS", "http://127.0.0.1:1/fixture-unavailable")
            .args([
                "--host",
                "127.0.0.1",
                "--port",
                &decoy_port_text,
                "--https-port",
                &api_port_text,
                "--multiview-only",
                "--multiview-config",
            ])
            .arg(&config_path)
            .arg("--cert")
            .arg(tls.cert_path())
            .arg("--key")
            .arg(tls.key_path())
            .stdout(
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(root.0.join("server.stdout"))
                    .unwrap(),
            )
            .stderr(
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(root.0.join("server.stderr"))
                    .unwrap(),
            )
            .spawn()
            .expect("normal production server binary"),
    );
    let startup_deadline = Instant::now() + STARTUP_LIMIT;
    let server_epoch;
    loop {
        if let Some(status) = server.0.try_wait().expect("server process status") {
            panic!(
                "normal server exited before capabilities response: {status}; {}",
                fixture_stderr_diagnostic(&root.0, &root.0.join("server.stderr"))
            );
        }
        let (code, bytes) = get(
            &api_origin,
            "/multiview/v1/capabilities",
            Some(TOKEN),
            256 * 1024,
        );
        if code == 200 {
            let capabilities: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(capabilities["max_inputs"], 9);
            assert_eq!(
                capabilities["qualified_input_adapters"],
                json!([mode.adapter()])
            );
            assert_eq!(capabilities["output_profiles"][0]["profile_id"], PROFILE);
            server_epoch = capabilities["server_epoch"].clone();
            fixture_http()
                .runtime
                .block_on(tls.assert_rejections(&api_origin));
            assert_eq!(
                get(
                    &format!("http://127.0.0.1:{decoy_port}"),
                    "/multiview/v1/capabilities",
                    Some(TOKEN),
                    64 * 1024
                )
                .0,
                404,
                "private production API must be absent from the unselected HTTP listener"
            );
            for path in ["/", "/index.html", "/proxy/fixture"] {
                let (route_status, _) = get(&api_origin, path, None, 256 * 1024);
                assert_eq!(
                    route_status, 404,
                    "--multiview-only unexpectedly served {path}"
                );
                assert_no_untrusted_cors(&api_origin, path);
            }
            assert_no_untrusted_cors(&api_origin, "/multiview/v1/capabilities");
            assert_eq!(get(&api_origin, "/health", None, 64 * 1024).0, 200);
            break;
        }
        assert!(
            Instant::now() < startup_deadline,
            "normal server did not bind selected private listener"
        );
        thread::sleep(Duration::from_millis(100));
    }

    // Exercise expiry with a real active decode/output. Renew only the active
    // session/output lease every 18s; the one-use 30s input grant is consumed
    // once and is never replayed. The 60s credential expiry remains the shorter
    // authority and must retire the active source and its output.
    let expiry_native = json!({"runtime":"native_runtime_expiry","expected":{"scene":"native_scene_expiry",
        "layout_revision":"layout_expiry","focus_revision":"focus_expiry","audio_revision":"audio_expiry"},
        "slot_id":0,"slot_generation":"slot_expiry","source_revision":"source_expiry"});
    let expiry_created = create_session(
        &api_origin,
        EXPIRING_TOKEN,
        &server_epoch,
        expiry_native,
        "create_expiry",
    );
    let (mut expiry_ctx, _) = contexts(&expiry_created);
    let expiry_list = catalog(&api_origin, EXPIRING_TOKEN, &expiry_ctx, "expiry_sources");
    expiry_ctx = expiry_list["context"].clone();
    expiry_ctx = request_source(
        &api_origin,
        EXPIRING_TOKEN,
        &expiry_ctx,
        &expiry_list["items"][0],
        &expiry_list["catalog_revision"],
        "expiry",
    );
    let expiry_deadline =
        Instant::now() + Duration::from_millis(short_expires.saturating_sub(now_ms()));
    let _ = wait_slot(
        &api_origin,
        EXPIRING_TOKEN,
        &expiry_ctx,
        0,
        "production_item_1",
        expiry_deadline,
    );
    for _ in 0..50 {
        if fs::read_dir(&output).unwrap().next().is_some() && upstream.source_was_read(mode) {
            break;
        }
        assert!(
            Instant::now() < expiry_deadline,
            "expiring credential never created active media output"
        );
        thread::sleep(Duration::from_millis(100));
    }
    assert!(
        fs::read_dir(&output).unwrap().next().is_some(),
        "credential expiry case had no active output to retire"
    );
    assert!(
        upstream.source_was_read(mode),
        "credential expiry case had no actual source read (raw requires active connection)"
    );
    let mut next_renewal = Instant::now() + Duration::from_secs(18);
    while Instant::now() + Duration::from_secs(2) < expiry_deadline {
        if Instant::now() >= next_renewal {
            let current = snapshot(
                &api_origin,
                EXPIRING_TOKEN,
                &expiry_ctx,
                "expiry_renew_status",
            );
            expiry_ctx = context_for_slot(&current, 0, &current["native_expected"]);
            let renewed = control(
                &api_origin,
                EXPIRING_TOKEN,
                &expiry_ctx,
                &format!("expiry_renew_{}", started.elapsed().as_secs()),
                json!({"name":"renew"}),
                None,
            );
            expiry_ctx = renewed["context"].clone();
            next_renewal = Instant::now() + Duration::from_secs(18);
        }
        thread::sleep(Duration::from_millis(100));
    }
    while now_ms() < short_expires {
        thread::sleep(Duration::from_millis(50));
    }
    let (expired_code, _) = get(
        &api_origin,
        "/multiview/v1/capabilities",
        Some(EXPIRING_TOKEN),
        256 * 1024,
    );
    assert_eq!(
        expired_code, 401,
        "expired control credential remained usable"
    );
    let mut expired = false;
    for _ in 0..30 {
        let (code, _) = http(
            "POST",
            &format!("{api_origin}/multiview/v1/status"),
            Some(EXPIRING_TOKEN),
            Some("expiry_status"),
            Some(
                &json!({"version":1,"server_epoch":expiry_ctx["server_epoch"],"session_id":expiry_ctx["session_id"],"native_expected":expiry_ctx["native_expected"]}),
            ),
            256 * 1024,
        );
        if code == 401 {
            expired = true;
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    assert!(expired, "expired session did not reject status");
    let cleanup_deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < cleanup_deadline
        && (fs::read_dir(&output).unwrap().next().is_some() || upstream.active_connections() != 0)
    {
        thread::sleep(Duration::from_millis(100));
    }
    assert!(
        fs::read_dir(&output).unwrap().next().is_none(),
        "expired active session output was not retired"
    );
    assert_eq!(
        upstream.active_connections(),
        0,
        "expired source decode connection was not closed"
    );
    upstream.confirm_cleanup(&output, true);

    // Fill all nine advertised slots in one disjoint 3x3 geometry. The three
    // sealed sources have distinct solid colors and tones so the encoded HLS
    // segment proves source-to-cell composition and selected audio ownership.
    let grid_created = create_session_slots(
        &api_origin,
        TOKEN,
        &server_epoch,
        9,
        "create_nine_slot_grid",
    );
    let mut latest = contexts(&grid_created).0;
    let mut slot_natives: Vec<Value> = (0..9).map(|slot| native_for_slot(slot as u64)).collect();
    let expected_items: Vec<String> = (0..9)
        .map(|index| format!("production_item_{}", index % 3 + 1))
        .collect();
    let mut source_refs = HashMap::new();
    let mut grid_next_renew = Instant::now() + Duration::from_secs(18);
    let mut grid_renew_sequence = 0;
    for slot_id in 0..9usize {
        renew_due(
            &api_origin,
            TOKEN,
            &mut latest,
            &mut slot_natives,
            &mut grid_next_renew,
            &mut grid_renew_sequence,
        );
        let current = snapshot(&api_origin, TOKEN, &latest, "grid_slot_status");
        let context = context_for_slot(&current, slot_id as u64, &slot_natives[slot_id]);
        let listing = catalog(&api_origin, TOKEN, &context, "grid_catalog");
        let selected = listing["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["item_ref"] == expected_items[slot_id])
            .unwrap()
            .clone();
        if slot_id < 3 {
            source_refs.insert(
                expected_items[slot_id].clone(),
                selected["source_ref"].clone(),
            );
        } else {
            assert_eq!(
                selected["source_ref"], source_refs[&expected_items[slot_id]],
                "reused catalog item changed source authority between slots"
            );
        }
        let context = listing["context"].clone();
        latest = request_source(
            &api_origin,
            TOKEN,
            &context,
            &selected,
            &listing["catalog_revision"],
            &format!("grid_{slot_id}"),
        );
        slot_natives[slot_id] = latest["native_expected"].clone();
    }
    let grid_status = snapshot(&api_origin, TOKEN, &latest, "grid_all_playing");
    let mut audio_context = context_for_slot(&grid_status, 0, &slot_natives[0]);
    let mut audio_native = audio_context["native_expected"].clone();
    audio_native["expected"]["audio_revision"] = json!("audio_grid_slot_zero");
    audio_context = control(
        &api_origin,
        TOKEN,
        &audio_context,
        "select_grid_audio",
        json!({"name":"audio","policy":"slot"}),
        Some(audio_native),
    )["context"]
        .clone();
    latest = audio_context;
    for native in &mut slot_natives {
        native["expected"]["audio_revision"] = json!("audio_grid_slot_zero");
    }
    slot_natives[0] = latest["native_expected"].clone();
    let grid_end = Instant::now() + Duration::from_secs(45);
    let grid_status = wait_all_slots(
        &api_origin,
        TOKEN,
        &mut latest,
        &mut slot_natives,
        &expected_items,
        &mut grid_next_renew,
        &mut grid_renew_sequence,
        grid_end,
    );
    let playlist = grid_status["output"]["playlist_path"]
        .as_str()
        .expect("private grid HLS playlist");
    let mut grid_seen = playlist_snapshot(&api_origin, playlist)
        .expect("grid HLS playlist before freshness barrier")
        .1
        .into_iter()
        .collect::<HashSet<_>>();
    // Ignore the first newly completed segment: it may contain frames queued
    // before the final slot reached its ten-frame readiness barrier.
    let _ = wait_new_segment(&api_origin, playlist, &mut grid_seen, grid_end);
    let (_, _, grid_segment) = wait_new_segment(&api_origin, playlist, &mut grid_seen, grid_end);
    let ffprobe = PathBuf::from(std::env::var("OTTPLAY_MULTIVIEW_FFPROBE").unwrap());
    let ffmpeg = PathBuf::from(std::env::var("OTTPLAY_MULTIVIEW_FFMPEG").unwrap());
    ffprobe_segment(&ffprobe, &grid_segment);
    let rgb = decode_segment_rgb(&ffmpeg, &grid_segment);
    for slot_id in 0..9usize {
        let x = [107, 320, 533][slot_id % 3];
        let y = [60, 180, 300][slot_id / 3];
        assert_cell_color(&rgb, x, y, (slot_id % 3 + 1) as u8);
    }
    let pcm = decode_segment_pcm(&ffmpeg, &grid_segment);
    assert_audio_tone(&pcm, 330.0);

    // Stop and close every real slot before the separate sustained replacement
    // scenario; verify source processes and private output are actually retired.
    for slot_id in 0..9usize {
        renew_due(
            &api_origin,
            TOKEN,
            &mut latest,
            &mut slot_natives,
            &mut grid_next_renew,
            &mut grid_renew_sequence,
        );
        let current = snapshot(&api_origin, TOKEN, &latest, "grid_stop_status");
        let context = context_for_slot(&current, slot_id as u64, &slot_natives[slot_id]);
        let mut next = context["native_expected"].clone();
        next["slot_generation"] = json!(format!("grid_stopped_{slot_id}"));
        next["source_revision"] = json!(format!("grid_stop_source_{slot_id}"));
        latest = control(
            &api_origin,
            TOKEN,
            &context,
            &format!("grid_stop_{slot_id}"),
            json!({"name":"stop"}),
            Some(next),
        )["context"]
            .clone();
        slot_natives[slot_id] = latest["native_expected"].clone();
    }
    let current = snapshot(&api_origin, TOKEN, &latest, "grid_close_status");
    let close_context = context_for_slot(&current, 0, &slot_natives[0]);
    let closed = control(
        &api_origin,
        TOKEN,
        &close_context,
        "grid_close",
        json!({"name":"off"}),
        None,
    );
    assert_eq!(closed["accepted"], true);
    let grid_cleanup = Instant::now() + Duration::from_secs(8);
    while Instant::now() < grid_cleanup
        && (fs::read_dir(&output).unwrap().next().is_some() || upstream.active_connections() != 0)
    {
        thread::sleep(Duration::from_millis(100));
    }
    assert!(
        fs::read_dir(&output).unwrap().next().is_none(),
        "nine-slot grid output was not retired after Off"
    );
    assert_eq!(
        upstream.active_connections(),
        0,
        "nine-slot source connections survived Off"
    );
    upstream.confirm_cleanup(&output, true);

    let native = json!({"runtime":"native_runtime_main","expected":{"scene":"native_scene_main",
        "layout_revision":"layout_main","focus_revision":"focus_main","audio_revision":"audio_main"},
        "slot_id":0,"slot_generation":"slot_main_0","source_revision":"source_main_0"});
    let created = create_session(&api_origin, TOKEN, &server_epoch, native, "create_main");
    let (ctx0, ctx1) = contexts(&created);
    let list0 = catalog(&api_origin, TOKEN, &ctx0, "sources_main_0");
    let ctx0 = list0["context"].clone();
    let mut item_map = HashMap::new();
    for row in list0["items"].as_array().unwrap() {
        item_map.insert(row["item_ref"].as_str().unwrap().to_owned(), row.clone());
    }
    let source_ref = list0["items"][0]["source_ref"].clone();
    assert_eq!(list0["items"].as_array().unwrap().len(), 3);
    let initial = snapshot(&api_origin, TOKEN, &ctx0, "status_before_slot_1");
    let ctx1 = context_for_slot(&initial, 1, &ctx1["native_expected"]);
    let ctx1_catalog = catalog(&api_origin, TOKEN, &ctx1, "sources_main_1");
    let ctx1 = ctx1_catalog["context"].clone();
    assert_eq!(ctx1_catalog["items"].as_array().unwrap().len(), 3);
    let source_a = item_map.get("production_item_1").unwrap();
    let source_b = item_map.get("production_item_2").unwrap();
    let source_c = item_map.get("production_item_3").unwrap();
    assert_eq!(source_a["source_ref"], source_ref);
    let revision = list0["catalog_revision"].clone();
    let mut ctx0 = request_source(&api_origin, TOKEN, &ctx0, source_a, &revision, "main_a");
    let initial = snapshot(&api_origin, TOKEN, &ctx0, "status_before_slot_1_play");
    let mut ctx1 = context_for_slot(&initial, 1, &ctx1["native_expected"]);
    let list1 = catalog(&api_origin, TOKEN, &ctx1, "sources_refresh_slot_1");
    ctx1 = list1["context"].clone();
    let mut ctx1 = request_source(
        &api_origin,
        TOKEN,
        &ctx1,
        source_b,
        &list1["catalog_revision"],
        "main_b",
    );
    let audio_status = snapshot(&api_origin, TOKEN, &ctx1, "main_audio_status");
    ctx0 = context_for_slot(&audio_status, 0, &ctx0["native_expected"]);
    let mut audio_native = ctx0["native_expected"].clone();
    audio_native["expected"]["audio_revision"] = json!("audio_main_slot_zero");
    ctx0 = control(
        &api_origin,
        TOKEN,
        &ctx0,
        "select_main_audio",
        json!({"name":"audio","policy":"slot"}),
        Some(audio_native),
    )["context"]
        .clone();
    let audio_status = snapshot(&api_origin, TOKEN, &ctx0, "main_audio_selected_status");
    ctx1 = context_for_slot(&audio_status, 1, &ctx1["native_expected"]);
    let end = Instant::now() + RUN_LIMIT;
    let _ = wait_slot(&api_origin, TOKEN, &ctx0, 0, "production_item_1", end);
    let first_b = wait_slot(&api_origin, TOKEN, &ctx1, 1, "production_item_2", end);
    let sustained_started = Instant::now();
    let mut output_checked = false;
    let mut replacement_output_checked = false;
    let mut next_renew = Instant::now() + Duration::from_secs(18);
    let replace_at = sustained_started + Duration::from_secs(92);
    let mut replaced = false;
    let mut replacement_seen = HashSet::new();
    let mut replacement_b_baseline = None;
    let mut first_replacement_sequence = None;
    let mut last_b_frames = slot(&first_b, 1)["frames"].as_u64().unwrap();
    let mut main_seen = playlist_snapshot(
        &api_origin,
        first_b["output"]["playlist_path"]
            .as_str()
            .expect("initial HLS playlist"),
    )
    .expect("initial HLS playlist snapshot")
    .1
    .into_iter()
    .collect::<HashSet<_>>();
    let mut late_progress_baseline = None;
    let mut last_b_progress = Instant::now();
    while Instant::now() < end && started.elapsed() < RUN_LIMIT {
        let last_status = snapshot(&api_origin, TOKEN, &ctx0, "long_status");
        let b_now = slot(&last_status, 1)["frames"].as_u64().unwrap();
        assert!(
            b_now >= last_b_frames,
            "sibling stream frame counter moved backwards"
        );
        if b_now > last_b_frames {
            last_b_frames = b_now;
            last_b_progress = Instant::now();
        }
        if late_progress_baseline.is_none()
            && sustained_started.elapsed() >= Duration::from_secs(170)
        {
            late_progress_baseline = Some(b_now);
        }
        assert!(
            slot(&last_status, 1)["source_ref"].as_str().is_some(),
            "sibling source authority disappeared"
        );
        if !output_checked {
            if let Some(path) = last_status["output"]["playlist_path"].as_str() {
                if let Some((_, _, bytes)) = try_new_segment(&api_origin, path, &mut main_seen) {
                    let ffprobe =
                        PathBuf::from(std::env::var("OTTPLAY_MULTIVIEW_FFPROBE").unwrap());
                    let ffmpeg = PathBuf::from(std::env::var("OTTPLAY_MULTIVIEW_FFMPEG").unwrap());
                    ffprobe_segment(&ffprobe, &bytes);
                    let rgb = decode_segment_rgb(&ffmpeg, &bytes);
                    assert_cell_color(&rgb, 160, 180, 1);
                    assert_cell_color(&rgb, 480, 180, 2);
                    assert_audio_tone(&decode_segment_pcm(&ffmpeg, &bytes), 330.0);
                    output_checked = true;
                }
            }
        }
        if !replaced && Instant::now() >= replace_at {
            let current = snapshot(&api_origin, TOKEN, &ctx0, "pre_stop");
            let before_playlist = current["output"]["playlist_path"]
                .as_str()
                .expect("pre-replacement HLS playlist");
            replacement_seen = playlist_snapshot(&api_origin, before_playlist)
                .expect("pre-replacement HLS playlist snapshot")
                .1
                .into_iter()
                .collect();
            replacement_b_baseline = Some(slot(&current, 1)["frames"].as_u64().unwrap());
            ctx0 = context_for_slot(&current, 0, &current["native_expected"]);
            let mut next = current["native_expected"].clone();
            next["slot_generation"] = json!("slot_main_0_replaced");
            next["source_revision"] = json!("source_main_0_replaced");
            let stopped = control(
                &api_origin,
                TOKEN,
                &ctx0,
                "stop_old_source",
                json!({"name":"stop"}),
                Some(next),
            );
            ctx0 = stopped["context"].clone();
            let list = catalog(&api_origin, TOKEN, &ctx0, "sources_after_stop");
            ctx0 = list["context"].clone();
            let current_c = list["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["item_ref"] == "production_item_3")
                .unwrap()
                .clone();
            ctx0 = request_source(
                &api_origin,
                TOKEN,
                &ctx0,
                &current_c,
                &list["catalog_revision"],
                "main_c",
            );
            replaced = true;
        }
        if replaced && !replacement_output_checked {
            let current = snapshot(&api_origin, TOKEN, &ctx0, "replacement_output_status");
            if slot(&current, 0)["item_ref"] == "production_item_3" {
                if let Some(path) = current["output"]["playlist_path"].as_str() {
                    if let Some((sequence, _, bytes)) =
                        try_new_segment(&api_origin, path, &mut replacement_seen)
                    {
                        let ffmpeg =
                            PathBuf::from(std::env::var("OTTPLAY_MULTIVIEW_FFMPEG").unwrap());
                        let rgb = decode_segment_rgb(&ffmpeg, &bytes);
                        let pcm = decode_segment_pcm(&ffmpeg, &bytes);
                        assert!(
                            !cell_matches_color(&rgb, 160, 180, 1),
                            "old source A appeared in a newly published post-replacement segment"
                        );
                        assert!(!audio_tone_matches(&pcm, 330.0),
                            "old source A audio appeared in a newly published post-replacement segment");
                        replacement_output_checked = cell_matches_color(&rgb, 160, 180, 3)
                            && cell_matches_color(&rgb, 480, 180, 2)
                            && audio_tone_matches(&pcm, 550.0);
                        if replacement_output_checked {
                            first_replacement_sequence = Some(sequence);
                        }
                    }
                }
            }
        }
        if Instant::now() >= next_renew {
            let current = snapshot(&api_origin, TOKEN, &ctx0, "renew_status");
            ctx0 = context_for_slot(&current, 0, &current["native_expected"]);
            let renewed = control(
                &api_origin,
                TOKEN,
                &ctx0,
                &format!("renew_{}", sustained_started.elapsed().as_secs()),
                json!({"name":"renew"}),
                None,
            );
            ctx0 = renewed["context"].clone();
            next_renew = Instant::now() + Duration::from_secs(18);
        }
        if replaced && sustained_started.elapsed() >= Duration::from_secs(185) {
            assert!(
                last_b_progress.elapsed() <= Duration::from_secs(8),
                "sibling frame counter stopped advancing during the late sustained interval"
            );
            break;
        }
        thread::sleep(Duration::from_millis(500));
    }
    assert!(
        replaced,
        "source replacement did not run within sustained test"
    );
    assert!(
        sustained_started.elapsed() >= Duration::from_secs(185),
        "main media session did not remain active for the qualified duration"
    );
    assert!(
        output_checked,
        "normal HLS output did not contain a probeable A/V segment"
    );
    assert!(
        replacement_output_checked,
        "replacement HLS output did not show the new source/audio without the stopped source"
    );
    assert!(
        last_b_frames > 2,
        "sibling did not continue producing frames during source replacement"
    );
    let after_replace = snapshot(&api_origin, TOKEN, &ctx0, "post_replace_status");
    assert!(
        slot(&after_replace, 1)["frames"].as_u64().unwrap() > replacement_b_baseline.unwrap_or(0),
        "sibling frame counter did not advance after source replacement"
    );
    let current = snapshot(&api_origin, TOKEN, &ctx0, "final_status");
    assert_eq!(slot(&current, 0)["source_ref"], source_c["source_ref"]);
    assert_eq!(slot(&current, 0)["item_ref"], source_c["item_ref"]);
    assert_eq!(slot(&current, 1)["item_ref"], source_b["item_ref"]);
    let late_playlist = current["output"]["playlist_path"]
        .as_str()
        .expect("late replacement HLS playlist");
    let (late_sequence, _, late_segment) = wait_new_segment(
        &api_origin,
        late_playlist,
        &mut replacement_seen,
        Instant::now() + Duration::from_secs(8),
    );
    assert!(
        late_sequence > first_replacement_sequence.unwrap_or(0),
        "late replacement output did not advance beyond the first confirmed new-source segment"
    );
    let late_ffmpeg = PathBuf::from(std::env::var("OTTPLAY_MULTIVIEW_FFMPEG").unwrap());
    let late_rgb = decode_segment_rgb(&late_ffmpeg, &late_segment);
    let late_pcm = decode_segment_pcm(&late_ffmpeg, &late_segment);
    assert!(
        !cell_matches_color(&late_rgb, 160, 180, 1),
        "old video source returned in late HLS output"
    );
    assert!(
        !audio_tone_matches(&late_pcm, 330.0),
        "old audio source returned in late HLS output"
    );
    assert_cell_color(&late_rgb, 160, 180, 3);
    assert_cell_color(&late_rgb, 480, 180, 2);
    assert_audio_tone(&late_pcm, 550.0);
    let final_status = snapshot(&api_origin, TOKEN, &ctx0, "final_progress_status");
    let final_b_frames = slot(&final_status, 1)["frames"].as_u64().unwrap();
    assert!(
        final_b_frames > slot(&after_replace, 1)["frames"].as_u64().unwrap(),
        "sibling frame counter did not advance during the final HLS sample window"
    );
    assert!(
        final_b_frames > late_progress_baseline.unwrap_or(0),
        "sibling source did not continue advancing through the late sustained interval"
    );
    let counts = upstream.request_counts();
    if mode == ProductionMode::RawMpegTs {
        for source in 1..=3 {
            assert!(
                counts
                    .get(&format!("source-{source}.ts"))
                    .copied()
                    .unwrap_or(0)
                    >= 2
            );
        }
    } else {
        let audit = upstream.hls.as_ref().unwrap().audit();
        assert!(audit.master.iter().all(|count| *count >= 2));
        assert!(audit.media.iter().all(|count| *count >= 2));
        assert!(audit.segments.iter().all(|source| source
            .iter()
            .filter(|count| **count > 0)
            .count()
            >= 8));
        assert_eq!(
            audit.future_denied, 0,
            "runtime requested unpublished or expired source window"
        );
    }
    let client = fixture_http();
    let https_control_requests = client.https_control_requests.load(Ordering::Relaxed);
    let https_media_requests = client.https_media_requests.load(Ordering::Relaxed);
    assert!(
        https_control_requests > 0,
        "private control API was not exercised over HTTPS"
    );
    assert!(
        https_media_requests > 0,
        "private HLS media was not exercised over HTTPS"
    );
    // Stop both active slots, close the session, and wait for the owned binary
    // and process tree to retire its private output directory before deletion.
    let mut latest_context = ctx0.clone();
    let mut slot_native = [
        ctx0["native_expected"].clone(),
        ctx1["native_expected"].clone(),
    ];
    for id in [0u8, 1u8] {
        let current = snapshot(&api_origin, TOKEN, &latest_context, "stop_final_status");
        let fresh = context_for_slot(&current, u64::from(id), &slot_native[usize::from(id)]);
        let mut next = fresh["native_expected"].clone();
        next["slot_generation"] = json!(format!("slot_final_{id}"));
        next["source_revision"] = json!(format!("source_final_{id}"));
        let response = control(
            &api_origin,
            TOKEN,
            &fresh,
            &format!("stop_final_{id}"),
            json!({"name":"stop"}),
            Some(next),
        );
        latest_context = response["context"].clone();
        slot_native[usize::from(id)] = latest_context["native_expected"].clone();
    }
    let current = snapshot(&api_origin, TOKEN, &latest_context, "close_status");
    let close_context = context_for_slot(&current, 0, &slot_native[0]);
    let close = control(
        &api_origin,
        TOKEN,
        &close_context,
        "close_main",
        json!({"name":"off"}),
        None,
    );
    assert_eq!(close["accepted"], true);
    upstream.confirm_cleanup(&output, true);
    let mut sequential_sessions = HashSet::new();
    let mut sequential_outputs = HashSet::new();
    for index in 0..17 {
        assert!(
            started.elapsed() < RUN_LIMIT,
            "17-context qualification exceeded budget"
        );
        let (ctx, output_path, session_id, generation) = start_single_playing_context(
            &api_origin,
            &server_epoch,
            &format!("sequential_{index}"),
        );
        assert!(sequential_sessions.insert(session_id));
        assert!(sequential_outputs.insert(generation));
        let closed = control(
            &api_origin,
            TOKEN,
            &ctx,
            &format!("sequential_off_{index}"),
            json!({"name":"off"}),
            None,
        );
        assert_eq!(closed["accepted"], true);
        upstream.confirm_cleanup(&output, false);
        let stale = get(&api_origin, &output_path, None, 256 * 1024).0;
        assert!(
            matches!(stale, 401 | 403 | 404 | 410),
            "retired output remained readable or failed unexpectedly"
        );
    }
    assert_eq!(sequential_sessions.len(), 17);
    assert_eq!(sequential_outputs.len(), 17);
    // A NEW actual Playing/output context must be alive at SIGTERM. There is
    // intentionally no Off between this observation and the owned signal.
    let (_active_ctx, _active_path, active_session, active_generation) =
        start_single_playing_context(&api_origin, &server_epoch, "active_sigterm");
    assert!(!sequential_sessions.contains(&active_session));
    assert!(!sequential_outputs.contains(&active_generation));
    assert!(fs::read_dir(&output).unwrap().next().is_some());
    assert!(upstream.source_was_read(mode));
    let signal = unsafe { libc::kill(server.0.id() as i32, libc::SIGTERM) };
    assert_eq!(signal, 0, "send daemon SIGTERM to owned normal server");
    let shutdown_deadline = Instant::now() + Duration::from_secs(10);
    let exit = loop {
        if let Some(exit) = server.0.try_wait().expect("owned server status") {
            break exit;
        }
        assert!(
            Instant::now() < shutdown_deadline,
            "normal server did not finish graceful backend shutdown"
        );
        thread::sleep(Duration::from_millis(100));
    };
    assert!(
        exit.success(),
        "normal server shutdown failed after backend cleanup"
    );
    for _ in 0..50 {
        if fs::read_dir(&output).unwrap().next().is_none() {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    assert!(
        fs::read_dir(&output).unwrap().next().is_none(),
        "normal binary output files were not retired"
    );
    upstream.confirm_cleanup(&output, true);
    assert!(
        started.elapsed() < RUN_LIMIT,
        "production fixture exceeded unchanged600-second work bound"
    );
    let hls_source_evidence = upstream.hls.as_ref().map(|hls| json!({"audit":hls.audit(),"inputs":hls.input_receipt(),"stable_retirement_interval_seconds":12}));
    drop(upstream);
    root.remove()
        .expect("remove owned private normal-binary fixture directory");
    assert!(
        !root.0.exists(),
        "owned production fixture directory survived cleanup"
    );
    let receipt_path = PathBuf::from(
        std::env::var_os("OTTPLAY_MULTIVIEW_PRODUCTION_RECEIPT").expect("owned receipt path"),
    );
    let receipt = json!({
        "schema":"ottplay.multiview.production-binary.v1","pass":true,
        "input_adapter":mode.adapter(),"sequential_contexts":sequential_sessions.len(),
        "sequential_output_generations":sequential_outputs.len(),"active_sigterm":true,"hls_source":hls_source_evidence,
        "tls":tls.receipt(),
        "https_control_requests":https_control_requests,"https_media_requests":https_media_requests,
        "scope":if mode == ProductionMode::RawMpegTs { "Normal production HTTPS control/HLS with a private RFC1918 HTTP MPEG-TS adapter, all nine advertised slots and private HLS output; no native-player or physical-television qualification" } else { "Normal production HTTPS control/HLS with a private RFC1918 live HLS MPEG-TS adapter, one master level and sliding media windows; no native-player or physical-television qualification" },
        "elapsed_seconds":started.elapsed().as_secs(),"sustained_seconds":sustained_started.elapsed().as_secs(),
        "credential_expiry_rejected":expired,
        "expiry_output_was_active":true,"expired_source_connections_closed":true,
        "expired_output_cleanup":true,"graceful_shutdown":true,"shutdown_signal":"SIGTERM",
        "output_directory_empty":true,
        "owned_fixture_root_removed":true,
        "source_replacement":true,"nine_slot_grid_rgb":true,"selected_audio_tone":true,
        "replacement_audio_tone":replacement_output_checked,"sibling_frames_before_replace":replacement_b_baseline,
        "sibling_frames_after_replace":slot(&after_replace,1)["frames"],"hls_segment_audio_video":true,
        "replacement_late_segment":true,"sibling_frames_recent_at_end":true,
        "upstream_request_counts":counts,
        "inputs":inputs.iter().map(|(id,_,sha,bytes)|json!({"id":id,"sha256":sha,"bytes":bytes})).collect::<Vec<_>>(),
    });
    let raw = serde_json::to_vec(&receipt).unwrap();
    let mut receipt_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&receipt_path)
        .expect("fresh fixture receipt");
    receipt_file.write_all(&raw).unwrap();
    receipt_file.sync_all().unwrap();
}
