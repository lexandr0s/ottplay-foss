//! Owned synthetic live-HLS source router for the normal-binary integration test.
//! No socket, media generation, production adapter or cleanup claim lives here.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    net::TcpStream,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

const TARGET: Duration = Duration::from_secs(12);
const BASE_SEQUENCE: usize = 1000;
const SOURCES: usize = 3;
const SEGMENTS: usize = 50;
const WINDOW: usize = 6;
const BOOTSTRAP: usize = 3;
const MAX_SEGMENT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: String,
    seconds: u64,
    target_seconds: u64,
    sources: Vec<ManifestSource>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestSource {
    id: usize,
    original_sha256: String,
    original_path: String,
    original_bytes: usize,
    original_packet_timeline_sha256: String,
    segmented_packet_timeline_sha256: String,
    segments: Vec<ManifestSegment>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestSegment {
    index: usize,
    path: String,
    bytes: usize,
    sha256: String,
    duration_ns: u64,
}
struct Segment {
    bytes: Arc<Vec<u8>>,
    duration_ns: u64,
    sha256: String,
}
struct Source {
    original_path: PathBuf,
    original_bytes: u64,
    original_sha256: String,
    packet_timeline_sha256: String,
    segments: Vec<Segment>,
}
#[derive(Clone, PartialEq, Eq, Serialize)]
pub(super) struct Audit {
    pub master: [u64; SOURCES],
    pub media: [u64; SOURCES],
    pub segments: [Vec<u64>; SOURCES],
    pub future_denied: u64,
}
impl Default for Audit {
    fn default() -> Self {
        Self {
            master: [0; SOURCES],
            media: [0; SOURCES],
            segments: std::array::from_fn(|_| vec![0; SEGMENTS]),
            future_denied: 0,
        }
    }
}
struct Publisher {
    began: Instant,
    generation: u64,
    audit: Audit,
}
pub(super) struct HlsSources {
    sources: Vec<Source>,
    publisher: Mutex<Publisher>,
}
pub(super) struct Reply {
    pub status: u16,
    pub content_type: &'static str,
    pub bytes: Arc<Vec<u8>>,
}
pub(super) struct RetirementObservation {
    began: Instant,
    generation: u64,
    audit: Audit,
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn sealed(path: &Path, root: &Path, limit: usize) -> Vec<u8> {
    assert!(
        path.is_absolute(),
        "owned HLS fixture path must be absolute"
    );
    let metadata = fs::symlink_metadata(path).expect("sealed HLS fixture metadata");
    assert!(metadata.is_file() && !metadata.file_type().is_symlink());
    assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
    assert_eq!(metadata.nlink(), 1);
    assert_eq!(metadata.mode() & 0o222, 0);
    assert!(metadata.len() > 0 && metadata.len() <= limit as u64);
    let actual = fs::canonicalize(path).expect("canonical sealed HLS fixture");
    assert!(actual.starts_with(root));
    let bytes = fs::read(path).expect("read sealed HLS fixture");
    let after = fs::symlink_metadata(path).expect("sealed HLS fixture after read");
    assert_eq!(
        (
            metadata.dev(),
            metadata.ino(),
            metadata.mode(),
            metadata.nlink(),
            metadata.len(),
            metadata.mtime(),
            metadata.mtime_nsec()
        ),
        (
            after.dev(),
            after.ino(),
            after.mode(),
            after.nlink(),
            after.len(),
            after.mtime(),
            after.mtime_nsec()
        )
    );
    assert_eq!(bytes.len() as u64, metadata.len());
    bytes
}
fn window(elapsed: Duration) -> (usize, usize) {
    let available = BOOTSTRAP
        .saturating_add((elapsed.as_secs() / TARGET.as_secs()) as usize)
        .min(SEGMENTS);
    (available.saturating_sub(WINDOW), available)
}
fn response(content_type: &'static str, bytes: Vec<u8>) -> Reply {
    Reply {
        status: 200,
        content_type,
        bytes: Arc::new(bytes),
    }
}
fn denied() -> Reply {
    Reply {
        status: 404,
        content_type: "text/plain",
        bytes: Arc::new(Vec::new()),
    }
}
impl HlsSources {
    pub fn load(path: &Path) -> Self {
        let root = fs::canonicalize(path.parent().expect("manifest parent"))
            .expect("owned HLS input root");
        let raw = sealed(path, &root, 128 * 1024);
        let manifest: Manifest = serde_json::from_slice(&raw).expect("sealed HLS input manifest");
        assert_eq!(
            manifest.schema,
            "ottplay.multiview.production-hls-inputs.v1"
        );
        assert_eq!((manifest.seconds, manifest.target_seconds), (600, 12));
        assert_eq!(manifest.sources.len(), SOURCES);
        let mut sources = Vec::new();
        let mut total_bytes = 0usize;
        for (source_index, row) in manifest.sources.into_iter().enumerate() {
            assert_eq!(row.id, source_index + 1);
            assert!(hex(&row.original_sha256) && hex(&row.original_packet_timeline_sha256));
            assert_eq!(
                row.original_packet_timeline_sha256, row.segmented_packet_timeline_sha256,
                "copy-only segmentation must preserve original packet timestamps"
            );
            let original_path = PathBuf::from(&row.original_path);
            let original = sealed(&original_path, &root, 64 * 1024 * 1024);
            assert_eq!(original.len(), row.original_bytes);
            assert_eq!(digest(&original), row.original_sha256);
            total_bytes = total_bytes.checked_add(original.len()).unwrap();
            assert_eq!(row.segments.len(), SEGMENTS);
            let mut segments = Vec::new();
            for (index, row) in row.segments.into_iter().enumerate() {
                assert_eq!(row.index, index);
                assert_eq!(row.duration_ns, 12_000_000_000);
                let bytes = sealed(Path::new(&row.path), &root, MAX_SEGMENT_BYTES);
                assert_eq!(bytes.len(), row.bytes);
                total_bytes = total_bytes.checked_add(bytes.len()).unwrap();
                assert!(
                    total_bytes <= 256 * 1024 * 1024,
                    "bounded original+HLS asset sum"
                );
                assert!(bytes.len() >= 188 * 3 && bytes.len() % 188 == 0);
                assert!([0, 188, 376]
                    .into_iter()
                    .all(|offset| bytes[offset] == 0x47));
                assert_eq!(digest(&bytes), row.sha256);
                segments.push(Segment {
                    bytes: Arc::new(bytes),
                    duration_ns: row.duration_ns,
                    sha256: row.sha256,
                });
            }
            sources.push(Source {
                original_path,
                original_bytes: row.original_bytes as u64,
                original_sha256: row.original_sha256,
                packet_timeline_sha256: row.original_packet_timeline_sha256,
                segments,
            });
        }
        Self {
            sources,
            publisher: Mutex::new(Publisher {
                began: Instant::now(),
                generation: 1,
                audit: Audit::default(),
            }),
        }
    }
    pub fn original_inputs(&self) -> Vec<(u8, PathBuf, String, u64)> {
        self.sources
            .iter()
            .enumerate()
            .map(|(index, source)| {
                (
                    index as u8 + 1,
                    source.original_path.clone(),
                    source.original_sha256.clone(),
                    source.original_bytes,
                )
            })
            .collect()
    }
    pub fn serve(&self, mut stream: TcpStream, stop: &AtomicBool) -> std::io::Result<()> {
        stream.set_read_timeout(Some(Duration::from_secs(3)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let mut raw = Vec::with_capacity(1024);
        let mut byte = [0u8; 1];
        while raw.len() < 16 * 1024 && !raw.ends_with(b"\r\n\r\n") {
            if stop.load(Ordering::Acquire) || stream.read(&mut byte)? == 0 {
                return Ok(());
            }
            raw.push(byte[0]);
        }
        if !raw.ends_with(b"\r\n\r\n") {
            return Ok(());
        }
        let first = raw.split(|b| *b == b'\n').next().unwrap_or_default();
        let fields = first.split(|b| *b == b' ').collect::<Vec<_>>();
        let reply = if fields.len() == 3 && fields[0] == b"GET" {
            self.route(std::str::from_utf8(fields[1]).unwrap_or(""))
        } else {
            denied()
        };
        write!(
            stream,
            "HTTP/1.1 {} {}\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            reply.status,
            if reply.status == 200 {
                "OK"
            } else {
                "Not Found"
            },
            reply.content_type,
            reply.bytes.len()
        )?;
        for chunk in reply.bytes.chunks(16 * 1024) {
            if stop.load(Ordering::Acquire) {
                break;
            }
            stream.write_all(chunk)?;
        }
        Ok(())
    }
    pub fn route(&self, path: &str) -> Reply {
        let mut publisher = self.publisher.lock().expect("fixture publisher");
        let elapsed = publisher.began.elapsed();
        self.route_at(path, elapsed, &mut publisher.audit)
    }
    fn route_at(&self, path: &str, elapsed: Duration, audit: &mut Audit) -> Reply {
        // Exact synthetic paths only. Caller must not URL-decode or normalize.
        let Some(path) = path.strip_prefix("/source-") else {
            return denied();
        };
        let Some((id, tail)) = path.split_once('/') else {
            return denied();
        };
        let Ok(id) = id.parse::<usize>() else {
            return denied();
        };
        if !(1..=SOURCES).contains(&id) || id.to_string() != path.split('/').next().unwrap() {
            return denied();
        }
        let source = &self.sources[id - 1];
        if tail == "master.m3u8" {
            audit.master[id - 1] = audit.master[id - 1].checked_add(1).expect("fixture count");
            return response("application/vnd.apple.mpegurl", b"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-STREAM-INF:BANDWIDTH=320000,RESOLUTION=96x64,FRAME-RATE=10.000\nmedia.m3u8\n".to_vec());
        }
        let (first, available) = window(elapsed);
        if tail == "media.m3u8" {
            audit.media[id - 1] = audit.media[id - 1].checked_add(1).expect("fixture count");
            let mut text = format!(
                "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:12\n#EXT-X-MEDIA-SEQUENCE:{}\n",
                BASE_SEQUENCE + first
            );
            for index in first..available {
                let nanos = source.segments[index].duration_ns;
                text.push_str(&format!(
                    "#EXTINF:{}.{:09},\nsegment-{}.ts\n",
                    nanos / 1_000_000_000,
                    nanos % 1_000_000_000,
                    BASE_SEQUENCE + index
                ));
            }
            return response("application/vnd.apple.mpegurl", text.into_bytes());
        }
        let Some(sequence) = tail
            .strip_prefix("segment-")
            .and_then(|s| s.strip_suffix(".ts"))
        else {
            return denied();
        };
        let Ok(sequence) = sequence.parse::<usize>() else {
            return denied();
        };
        let Some(index) = sequence.checked_sub(BASE_SEQUENCE) else {
            return denied();
        };
        if index >= available
            || index < first
            || index >= SEGMENTS
            || tail != format!("segment-{sequence}.ts")
        {
            audit.future_denied = audit.future_denied.checked_add(1).expect("fixture count");
            return denied();
        }
        audit.segments[id - 1][index] = audit.segments[id - 1][index]
            .checked_add(1)
            .expect("fixture count");
        Reply {
            status: 200,
            content_type: "video/mp2t",
            bytes: source.segments[index].bytes.clone(),
        }
    }
    pub fn audit(&self) -> Audit {
        self.publisher
            .lock()
            .expect("fixture publisher")
            .audit
            .clone()
    }
    /// Call only AFTER the scenario asserted an accepted Off or expired-session
    /// rejection. This observation does not itself manufacture that authority.
    pub fn observe_retirement(&self) -> RetirementObservation {
        let publisher = self.publisher.lock().expect("fixture publisher");
        RetirementObservation {
            began: Instant::now(),
            generation: publisher.generation,
            audit: publisher.audit.clone(),
        }
    }
    /// The caller supplies its real upstream worker/socket count. No existing
    /// input file changes; the finite publisher never resets or loops.
    pub fn confirm_retired_scene(
        &self,
        observation: RetirementObservation,
        output: &Path,
        active_bodies: usize,
    ) {
        assert!(
            observation.began.elapsed() >= TARGET,
            "observe a full target interval after retirement"
        );
        assert_eq!(active_bodies, 0, "actual upstream bodies must drain");
        assert!(
            fs::read_dir(output)
                .expect("owned output root")
                .next()
                .is_none(),
            "output cleanup required"
        );
        let publisher = self.publisher.lock().expect("fixture publisher");
        assert_eq!(publisher.generation, observation.generation);
        assert!(
            publisher.audit == observation.audit,
            "retired source still fetches"
        );
        // The finite600-second source publisher never resets or loops.
    }
    pub fn input_receipt(&self) -> serde_json::Value {
        serde_json::json!(self.sources.iter().enumerate().map(|(index, source)| serde_json::json!({
            "id":index+1,"original_sha256":source.original_sha256,"packet_timeline_sha256":source.packet_timeline_sha256,
            "segments":source.segments.iter().enumerate().map(|(index, segment)|serde_json::json!({
                "index":index,"sha256":segment.sha256,"bytes":segment.bytes.len(),"duration_ns":segment.duration_ns
            })).collect::<Vec<_>>()
        })).collect::<Vec<_>>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> HlsSources {
        HlsSources {
            sources: (0..SOURCES)
                .map(|_| Source {
                    original_path: PathBuf::from("/synthetic-unit-only"),
                    original_bytes: 1,
                    original_sha256: "a".repeat(64),
                    packet_timeline_sha256: "b".repeat(64),
                    segments: (0..SEGMENTS)
                        .map(|index| Segment {
                            bytes: Arc::new(vec![index as u8; 188 * 3]),
                            duration_ns: 12_000_000_000,
                            sha256: "c".repeat(64),
                        })
                        .collect(),
                })
                .collect(),
            publisher: Mutex::new(Publisher {
                began: Instant::now(),
                generation: 1,
                audit: Audit::default(),
            }),
        }
    }
    #[test]
    fn starts_with_three_complete_and_slides_six_without_exposing_future() {
        let source = fixture();
        let mut audit = Audit::default();
        let first = source.route_at("/source-1/media.m3u8", Duration::ZERO, &mut audit);
        let text = std::str::from_utf8(&first.bytes).unwrap();
        assert!(text.contains("#EXT-X-MEDIA-SEQUENCE:1000\n"));
        assert_eq!(text.matches("#EXTINF:").count(), 3);
        assert!(!text.contains("ENDLIST"));
        assert_eq!(
            source
                .route_at("/source-1/segment-1003.ts", Duration::ZERO, &mut audit)
                .status,
            404
        );
        assert_eq!(
            source
                .route_at(
                    "/source-1/segment-1003.ts",
                    Duration::from_secs(12),
                    &mut audit
                )
                .status,
            200
        );
        let late = source.route_at("/source-1/media.m3u8", Duration::from_secs(60), &mut audit);
        let text = std::str::from_utf8(&late.bytes).unwrap();
        assert!(text.contains("#EXT-X-MEDIA-SEQUENCE:1002\n"));
        assert_eq!(text.matches("#EXTINF:").count(), 6);
        assert_eq!(
            source
                .route_at(
                    "/source-1/segment-1000.ts",
                    Duration::from_secs(60),
                    &mut audit
                )
                .status,
            404
        );
    }
    #[test]
    fn never_loops_or_serves_fifty_first_segment_and_keeps_original_duration() {
        let source = fixture();
        let mut audit = Audit::default();
        let late = source.route_at(
            "/source-3/media.m3u8",
            Duration::from_secs(3600),
            &mut audit,
        );
        let text = std::str::from_utf8(&late.bytes).unwrap();
        assert!(text.contains("#EXT-X-MEDIA-SEQUENCE:1044\n"));
        assert!(text.contains("#EXTINF:12.000000000,"));
        assert!(!text.contains("segment-1050.ts"));
        assert_eq!(
            source
                .route_at(
                    "/source-3/segment-1050.ts",
                    Duration::from_secs(3600),
                    &mut audit
                )
                .status,
            404
        );
    }
    #[test]
    fn exact_path_fence_and_master_child_are_self_contained() {
        let source = fixture();
        let mut audit = Audit::default();
        for path in [
            "/source-0/master.m3u8",
            "/source-01/master.m3u8",
            "/source-4/master.m3u8",
            "/source-1/../segment-1000.ts",
            "/source-1/segment-01000.ts",
            "/source-1/segment-1000.ts?token=x",
        ] {
            assert_eq!(
                source.route_at(path, Duration::ZERO, &mut audit).status,
                404
            );
        }
        let master = source.route_at("/source-1/master.m3u8", Duration::ZERO, &mut audit);
        let text = std::str::from_utf8(&master.bytes).unwrap();
        assert_eq!(
            text.lines()
                .filter(|l| !l.starts_with('#'))
                .collect::<Vec<_>>(),
            ["media.m3u8"]
        );
        assert!(!text.contains("http:"));
        assert!(!text.contains("AUDIO="));
    }
}
