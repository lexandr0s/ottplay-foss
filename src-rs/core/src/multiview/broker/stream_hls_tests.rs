//! Synthetic HTTP/TS control fixtures only; no real media or decoder execution.
use super::super::{
    network::{CheckedTransport, Limits, NetworkPolicy},
    registry::{EnrolledInput, Enrollment, ItemEnrollment, Registry, SourcePrincipal},
    resolvers::ServerSource,
    runtime::{SourceConfig, SourceService},
    wire::{
        BackendExpected, Binding, Descriptor, GrantRequest, NativeExpected, NativeRevisions,
        Permission, Principal, Protection, SourcesRequest,
    },
};
use super::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::{AtomicUsize, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};
use uuid::Uuid;
pub(in crate::multiview::broker) fn player() -> Principal {
    Principal {
        subject: "stream_owner".into(),
        installation_id: "stream_native".into(),
        permissions: BTreeSet::from([Permission::MultiviewControl]),
    }
}
pub(in crate::multiview::broker) fn operator() -> Principal {
    Principal {
        subject: "stream_operator".into(),
        installation_id: "stream_server".into(),
        permissions: BTreeSet::from([Permission::SourcesManage]),
    }
}
pub(in crate::multiview::broker) fn binding() -> Binding {
    Binding {
        server_epoch: "stream_epoch".into(),
        session_id: "stream_session".into(),
        backend_expected: BackendExpected {
            runtime: 1,
            scene: "backend_scene".into(),
            geometry_revision: 1,
            focus_revision: 1,
            audio_revision: 1,
        },
        native_expected: NativeExpected {
            runtime: "native_runtime".into(),
            expected: NativeRevisions {
                scene: "native_scene".into(),
                layout_revision: "layout".into(),
                focus_revision: "focus".into(),
                audio_revision: "audio".into(),
            },
            slot_id: 0,
            slot_generation: "slot".into(),
            source_revision: "source".into(),
        },
        backend_slot_revision: 1,
    }
}
pub(in crate::multiview::broker) fn packets(count: usize, marker: u8) -> Vec<u8> {
    let mut bytes = vec![marker; count * 188];
    for (index, packet) in bytes.chunks_exact_mut(188).enumerate() {
        packet[0] = 0x47;
        packet[1] = 0x01;
        packet[2] = 0x00;
        packet[3] = 0x10 | (index as u8 & 15);
    }
    bytes
}
async fn listener() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    (listener, base)
}
async fn request(socket: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 2048];
    loop {
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = std::str::from_utf8(&bytes[..end + 4]).unwrap();
            let length = headers
                .lines()
                .find_map(|line| {
                    line.split_once(':')
                        .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            assert!(length < 16_384);
            if bytes.len() >= end + 4 + length {
                return String::from_utf8(bytes).unwrap();
            }
        }
        let count = tokio::time::timeout(Duration::from_secs(2), socket.read(&mut buffer))
            .await
            .unwrap()
            .unwrap();
        assert_ne!(count, 0);
        bytes.extend_from_slice(&buffer[..count]);
        assert!(bytes.len() < 32_768);
    }
}
async fn response(socket: &mut TcpStream, content_type: &str, body: &[u8]) {
    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await.unwrap();
    socket.write_all(body).await.unwrap();
    socket.shutdown().await.unwrap();
}
pub(in crate::multiview::broker) async fn make_source(
    base: &str,
    source: ServerSource,
) -> (AuthorizedSource, Arc<Registry>) {
    make_source_with_transport(
        source,
        CheckedTransport::new(
            NetworkPolicy::fixture(&[base]),
            Limits {
                timeout: Duration::from_secs(2),
                ..Limits::default()
            },
        )
        .unwrap(),
    )
    .await
}
async fn make_source_with_transport(
    source: ServerSource,
    transport: CheckedTransport,
) -> (AuthorizedSource, Arc<Registry>) {
    let registry = Arc::new(Registry::new("stream_server".into(), "stream_epoch".into()).unwrap());
    let service = SourceService::new(registry.clone());
    service
        .enroll(
            &operator(),
            Enrollment {
                source_ref: "owned_stream".into(),
                profile_uuid: Uuid::from_u128(7),
                config_revision: "configuration".into(),
                policy_revision: "policy".into(),
                allowed: BTreeSet::from([SourcePrincipal::from(&player())]),
                items: vec![ItemEnrollment {
                    item_ref: "item".into(),
                    identity_version: 1,
                    favorite_sha256: "a".repeat(64),
                    kind: "live".into(),
                    input_adapter: "http-hls-ts-v1".into(),
                    protection: Protection {
                        known: true,
                        restricted: false,
                    },
                    descriptor: Descriptor {
                        title: "Synthetic TS fixture".into(),
                        provider: "m3u".into(),
                    },
                    input: EnrolledInput { config_index: 0 },
                }],
            },
            BTreeMap::from([("item".into(), SourceConfig { source, transport })]),
            None,
        )
        .unwrap();
    registry.create_session(&player(), &binding()).unwrap();
    let catalog = registry
        .sources(
            &player(),
            &SourcesRequest {
                version: 1,
                context: binding(),
                cursor: None,
            },
        )
        .unwrap();
    let item = &catalog.items[0];
    let grant = registry
        .mint(
            &player(),
            &GrantRequest {
                version: 1,
                context: binding(),
                next_native: None,
                source_ref: item.source_ref.clone(),
                item_ref: item.item_ref.clone(),
                item_revision: item.item_revision.clone(),
                identity_version: 1,
                favorite_sha256: item.favorite_sha256.clone(),
                catalog_revision: catalog.catalog_revision,
                remaining_budget_ms: 30_000,
            },
        )
        .unwrap();
    let resolved = service
        .resolve(
            &player(),
            &binding(),
            &grant.input_grant,
            &CancellationToken::new(),
        )
        .await
        .ok()
        .expect("server-owned source resolves");
    (resolved, registry)
}

pub(in crate::multiview::broker) fn playlist(first: u64, count: usize, ended: bool) -> Vec<u8> {
    let mut value = format!("#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:{first}\n");
    for index in 0..count {
        value.push_str(&format!("#EXTINF:0.02,\ns{}.ts\n", first + index as u64));
    }
    if ended {
        value.push_str("#EXT-X-ENDLIST\n");
    }
    value.into_bytes()
}
pub(in crate::multiview::broker) struct Reply {
    pub path: String,
    pub body: Vec<u8>,
    pub status: u16,
    pub headers: &'static str,
}
impl Reply {
    pub(in crate::multiview::broker) fn ok(path: &str, body: Vec<u8>) -> Self {
        Self {
            path: path.into(),
            body,
            status: 200,
            headers: "",
        }
    }
}
pub(in crate::multiview::broker) struct Fixture {
    pub base: String,
    pub requests: Arc<AtomicUsize>,
    task: Option<JoinHandle<()>>,
}
impl Fixture {
    pub(in crate::multiview::broker) async fn start(replies: Vec<Reply>) -> Self {
        let (listener, base) = listener().await;
        let requests = Arc::new(AtomicUsize::new(0));
        let seen = requests.clone();
        let task = tokio::spawn(async move {
            for reply in replies {
                let (mut socket, _) =
                    tokio::time::timeout(Duration::from_secs(3), listener.accept())
                        .await
                        .unwrap()
                        .unwrap();
                let req = request(&mut socket).await;
                assert_eq!(
                    req.lines().next().unwrap(),
                    format!("GET {} HTTP/1.1", reply.path)
                );
                seen.fetch_add(1, Ordering::SeqCst);
                if reply.status == 200 && reply.headers.is_empty() {
                    response(&mut socket, "application/octet-stream", &reply.body).await;
                } else {
                    socket.write_all(format!("HTTP/1.1 {} Fixture\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n",reply.status,reply.body.len(),reply.headers).as_bytes()).await.unwrap();
                    socket.write_all(&reply.body).await.unwrap();
                }
            }
        });
        Self {
            base,
            requests,
            task: Some(task),
        }
    }
    pub(in crate::multiview::broker) async fn finish(mut self) {
        tokio::time::timeout(Duration::from_secs(3), self.task.take().unwrap())
            .await
            .unwrap()
            .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
fn limits() -> StreamLimits {
    StreamLimits {
        first_response: Duration::from_secs(2),
        idle: Duration::from_secs(2),
        hard_end: Instant::now() + Duration::from_secs(5),
    }
}
async fn source(fixture: &Fixture) -> (Arc<AuthorizedSource>, Arc<Registry>) {
    let (source, registry) = make_source(
        &fixture.base,
        ServerSource::hls_ts(&format!("{}/root", fixture.base), None).unwrap(),
    )
    .await;
    (Arc::new(source), registry)
}
async fn finish(source: Arc<AuthorizedSource>) {
    match Arc::try_unwrap(source) {
        Ok(source) => source.finish().await.unwrap(),
        Err(_) => panic!("source custody not released"),
    }
}
fn media(first: u64, paths: &[&str], ended: bool) -> MediaPlaylist {
    MediaPlaylist {
        media_sequence: first,
        discontinuity_sequence: 0,
        target_duration: Duration::from_secs(1),
        end_list: ended,
        segments: paths
            .iter()
            .enumerate()
            .map(|(index, path)| MediaSegment {
                sequence: first + index as u64,
                duration: Duration::from_millis(20),
                resource: HlsResource {
                    url: reqwest::Url::parse(&format!("https://fixture.invalid/{path}")).unwrap(),
                    kind: ResourceKind::Segment,
                    provenance: Some(Arc::new(())),
                },
            })
            .collect(),
    }
}

#[test]
fn cursor_rejects_rollback_overlap_rewrite_and_window_loss() {
    for invalid in [
        media(9, &["a", "b"], false),
        media(10, &["changed", "b"], false),
        media(13, &["d"], false),
        media(10, &["a"], false),
    ] {
        let mut cursor = Cursor::new(media(10, &["a", "b"], false)).unwrap();
        assert_eq!(cursor.take().unwrap().unwrap().sequence, 10);
        assert_eq!(cursor.take().unwrap().unwrap().sequence, 11);
        assert!(cursor.reload(invalid).is_err());
    }
    let mut cursor = Cursor::new(media(10, &["a", "b"], false)).unwrap();
    cursor.take().unwrap();
    cursor.take().unwrap();
    let mut changed = media(11, &["b", "c"], false);
    changed.segments[0].duration = Duration::from_millis(21);
    assert!(cursor.reload(changed).is_err());
}
#[test]
fn cursor_rolls_window_once_and_never_reemits_old_sequence() {
    let mut cursor = Cursor::new(media(10, &["a", "b"], false)).unwrap();
    cursor.take().unwrap();
    cursor.take().unwrap();
    assert!(cursor.reload(media(11, &["b", "c"], false)).unwrap());
    assert_eq!(cursor.take().unwrap().unwrap().sequence, 12);
    assert!(cursor.take().unwrap().is_none());
    assert!(!cursor.reload(media(11, &["b", "c"], false)).unwrap());
    assert!(cursor.take().unwrap().is_none());
    assert!(cursor.reload(media(12, &["c", "a"], true)).unwrap());
    assert_eq!(cursor.take().unwrap().unwrap().sequence, 13);
    assert!(cursor.ended);
    assert!(cursor.reload(media(12, &["c", "a"], false)).is_err());
}
#[test]
fn initial_live_window_is_bounded_but_endlist_keeps_first_segment() {
    let paths = ["a", "b", "c", "d", "e"];
    assert_eq!(
        Cursor::new(media(5, &paths, false))
            .unwrap()
            .take()
            .unwrap()
            .unwrap()
            .sequence,
        7
    );
    assert_eq!(
        Cursor::new(media(5, &paths, true))
            .unwrap()
            .take()
            .unwrap()
            .unwrap()
            .sequence,
        5
    );
    assert_eq!(
        progress_wait(Duration::from_secs(12), Duration::from_secs(8)),
        Duration::from_secs(36)
    );
    assert_eq!(
        progress_wait(Duration::from_secs(30), Duration::from_secs(8)),
        Duration::from_secs(90)
    );
}
#[test]
fn whole_segment_packet_validation_rejects_late_corruption_and_non_ts() {
    let valid = packets(7, 0x55);
    assert!(validate_ts(&valid).is_ok());
    let mut broken = valid.clone();
    broken[6 * 188] = 0;
    assert_eq!(validate_ts(&broken), Err(Error::Unsupported));
    let mut broken = valid.clone();
    broken[6 * 188 + 3] = 0x30;
    broken[6 * 188 + 4] = 183;
    assert_eq!(validate_ts(&broken), Err(Error::Unsupported));
    assert_eq!(
        validate_ts(&valid[..valid.len() - 1]),
        Err(Error::Unsupported)
    );
    assert_eq!(validate_ts(b"#EXTM3U\nnot-ts"), Err(Error::Unsupported));
}

#[tokio::test]
async fn two_sliding_windows_are_continuous_and_endlist_is_finite() {
    let fixture = Fixture::start(vec![
        Reply::ok("/root", playlist(10, 2, false)),
        Reply::ok("/s10.ts", packets(7, 0x51)),
        Reply::ok("/s11.ts", packets(7, 0x52)),
        Reply::ok("/root", playlist(11, 2, false)),
        Reply::ok("/s12.ts", packets(7, 0x53)),
        Reply::ok("/root", playlist(12, 2, true)),
        Reply::ok("/s13.ts", packets(7, 0x54)),
    ])
    .await;
    let (source, _) = source(&fixture).await;
    let mut stream = source.open_input(limits()).await.unwrap();
    let mut actual = Vec::new();
    while let Some(chunk) = stream.next_chunk().await.unwrap() {
        assert!(chunk.len() <= CHUNK_BYTES);
        actual.extend_from_slice(&chunk);
    }
    let expected: Vec<u8> = (0x51..=0x54)
        .flat_map(|marker| packets(7, marker))
        .collect();
    assert_eq!(actual, expected);
    assert_eq!(fixture.requests.load(Ordering::SeqCst), 7);
    drop(stream);
    finish(source).await;
    fixture.finish().await;
}
#[tokio::test]
async fn retained_chunk_blocks_fetch_and_reopen_after_stream_drop() {
    let fixture = Fixture::start(vec![
        Reply::ok("/root", playlist(1, 1, true)),
        Reply::ok("/s1.ts", packets(400, 0x51)),
        Reply::ok("/root", playlist(1, 1, true)),
        Reply::ok("/s1.ts", packets(7, 0x52)),
    ])
    .await;
    let (source, _) = source(&fixture).await;
    let mut stream = source.open_input(limits()).await.unwrap();
    let chunk = stream.next_chunk().await.unwrap().unwrap();
    let held = chunk.slice(1..20);
    drop(chunk);
    assert!(matches!(stream.next_chunk().await, Err(Error::Busy)));
    assert_eq!(fixture.requests.load(Ordering::SeqCst), 2);
    drop(stream);
    assert!(matches!(
        source.open_input(limits()).await,
        Err(RuntimeError::Network(Error::Busy))
    ));
    assert_eq!(fixture.requests.load(Ordering::SeqCst), 2);
    drop(held);
    let mut fresh = source.open_input(limits()).await.unwrap();
    assert_eq!(fresh.next_chunk().await.unwrap().unwrap().len(), 7 * 188);
    drop(fresh);
    finish(source).await;
    fixture.finish().await;
}
#[tokio::test]
async fn existing_registry_revocation_blocks_the_next_segment() {
    let fixture = Fixture::start(vec![
        Reply::ok("/root", playlist(1, 2, true)),
        Reply::ok("/s1.ts", packets(7, 0x51)),
    ])
    .await;
    let (source, registry) = source(&fixture).await;
    let mut stream = source.open_input(limits()).await.unwrap();
    drop(stream.next_chunk().await.unwrap().unwrap());
    registry
        .revoke_session(&player(), &binding().session_id)
        .unwrap();
    assert!(matches!(stream.next_chunk().await, Err(Error::Cancelled)));
    assert_eq!(fixture.requests.load(Ordering::SeqCst), 2);
    drop(stream);
    finish(source).await;
    fixture.finish().await;
}
#[tokio::test]
async fn rejected_and_redirected_roots_are_not_retried_or_followed() {
    for (status, headers, expected) in [
        (401, "", Error::Rejected),
        (302, "Location: /other\r\n", Error::RedirectDenied),
    ] {
        let fixture = Fixture::start(vec![Reply {
            path: "/root".into(),
            body: vec![],
            status,
            headers,
        }])
        .await;
        let (source, _) = source(&fixture).await;
        assert!(
            matches!(source.open_input(limits()).await,Err(RuntimeError::Network(error)) if error==expected)
        );
        assert_eq!(fixture.requests.load(Ordering::SeqCst), 1);
        finish(source).await;
        fixture.finish().await;
    }
}
#[tokio::test]
async fn unsupported_playlist_features_fail_before_any_segment_request() {
    for tag in [
        "#EXT-X-KEY:METHOD=AES-128,URI=\"key\"\n",
        "#EXT-X-MAP:URI=\"init.mp4\"\n",
        "#EXT-X-GAP\n",
        "#EXT-X-DISCONTINUITY\n",
        "#EXT-X-BYTERANGE:188@0\n",
    ] {
        let text = format!(
            "#EXTM3U\n#EXT-X-TARGETDURATION:1\n{tag}#EXTINF:0.02,\ns1.ts\n#EXT-X-ENDLIST\n"
        );
        let fixture = Fixture::start(vec![Reply::ok("/root", text.into_bytes())]).await;
        let (source, _) = source(&fixture).await;
        assert!(source.open_input(limits()).await.is_err());
        assert_eq!(fixture.requests.load(Ordering::SeqCst), 1);
        finish(source).await;
        fixture.finish().await;
    }
}
fn master(entries: &[(&str, u64)]) -> Vec<u8> {
    let mut text = String::from("#EXTM3U\n");
    for (uri, bw) in entries {
        text.push_str(&format!("#EXT-X-STREAM-INF:BANDWIDTH={bw}\n{uri}\n"));
    }
    text.into_bytes()
}
#[tokio::test]
async fn master_pin_survives_reordering_new_lower_variant_and_rejects_disappearance() {
    let fixture = Fixture::start(vec![
        Reply::ok("/root", master(&[("a.m3u8", 200), ("b.m3u8", 400)])),
        Reply::ok("/a.m3u8", playlist(1, 1, true)),
        Reply::ok("/s1.ts", packets(7, 0x51)),
        Reply::ok(
            "/root",
            master(&[("c.m3u8", 100), ("b.m3u8", 400), ("a.m3u8", 200)]),
        ),
        Reply::ok("/a.m3u8", playlist(2, 1, true)),
        Reply::ok("/s2.ts", packets(7, 0x52)),
        Reply::ok("/root", master(&[("c.m3u8", 100)])),
    ])
    .await;
    let (source, _) = source(&fixture).await;
    let mut first = source.open_input(limits()).await.unwrap();
    drop(first.next_chunk().await.unwrap().unwrap());
    drop(first);
    let old = match source.hls_selection().unwrap().unwrap() {
        HlsSelection::Variant(p) => p,
        _ => panic!("expected master"),
    };
    let mut second = source.open_input(limits()).await.unwrap();
    drop(second.next_chunk().await.unwrap().unwrap());
    drop(second);
    let new = match source.hls_selection().unwrap().unwrap() {
        HlsSelection::Variant(p) => p,
        _ => panic!("expected master"),
    };
    assert_eq!(old.url, new.url);
    assert!(!Arc::ptr_eq(&old, &new));
    assert!(matches!(
        source.open_input(limits()).await,
        Err(RuntimeError::Network(Error::DestinationDenied))
    ));
    finish(source).await;
    fixture.finish().await;
}

#[tokio::test]
async fn same_origin_media_fence_rejects_even_an_other_enrolled_origin() {
    let foreign = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let foreign_origin = format!("http://{}", foreign.local_addr().unwrap());
    let text = format!(
        "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:0.02,\n{foreign_origin}/s1.ts\n#EXT-X-ENDLIST\n"
    );
    let fixture = Fixture::start(vec![Reply::ok("/root", text.into_bytes())]).await;
    let transport = CheckedTransport::new(
        NetworkPolicy::fixture(&[&fixture.base, &foreign_origin]),
        Limits::default(),
    )
    .unwrap();
    let (source, _) = make_source_with_transport(
        ServerSource::hls_ts(&format!("{}/root", fixture.base), None).unwrap(),
        transport,
    )
    .await;
    let source = Arc::new(source);
    assert!(matches!(
        source.open_input(limits()).await,
        Err(RuntimeError::Network(Error::DestinationDenied))
    ));
    // The foreign listener is owned and never contacted. No external address.
    assert!(
        tokio::time::timeout(Duration::from_millis(20), foreign.accept())
            .await
            .is_err()
    );
    finish(source).await;
    fixture.finish().await;
}

#[tokio::test]
async fn pacing_wait_is_pending_and_subscription_cancel_does_not_cancel_source() {
    use std::{future::Future, task::Poll};
    let fixture = Fixture::start(vec![
        Reply::ok("/root", playlist(1, 1, true)),
        Reply::ok("/s1.ts", packets(7, 0x51)),
    ])
    .await;
    let (source, _) = source(&fixture).await;
    let mut stream = source.open_input(limits()).await.unwrap();
    // Inject the next delivery instant with already-loaded real checked bytes.
    // A missing pacing await would return Ready here; network latency cannot
    // satisfy the negative control because no further fetch is needed.
    let super::super::stream_transport::InputStream::HlsTs(hls) = &mut stream else {
        panic!("HLS kind")
    };
    hls.next_due = Instant::now() + Duration::from_secs(60);
    let cancel = stream.cancellation();
    let mut waiting = Box::pin(stream.next_chunk());
    std::future::poll_fn(|cx| match waiting.as_mut().poll(cx) {
        Poll::Pending => Poll::Ready(()),
        Poll::Ready(_) => panic!("pacing bypassed"),
    })
    .await;
    cancel.cancel();
    assert!(matches!(waiting.await, Err(Error::Cancelled)));
    drop(stream);
    assert!(source.check_authority().is_ok());
    finish(source).await;
    fixture.finish().await;
}

#[tokio::test]
async fn expired_progress_or_hard_end_never_starts_another_fetch() {
    for progress in [false, true] {
        let fixture = Fixture::start(vec![
            Reply::ok("/root", playlist(1, 1, false)),
            Reply::ok("/s1.ts", packets(7, 0x51)),
        ])
        .await;
        let (source, _) = source(&fixture).await;
        let mut stream = source.open_input(limits()).await.unwrap();
        if progress {
            drop(stream.next_chunk().await.unwrap().unwrap());
        }
        let super::super::stream_transport::InputStream::HlsTs(hls) = &mut stream else {
            panic!("HLS kind")
        };
        if progress {
            hls.progress_deadline = Instant::now();
        } else {
            hls.limits.hard_end = Instant::now();
        }
        assert!(matches!(stream.next_chunk().await, Err(Error::Timeout)));
        assert_eq!(fixture.requests.load(Ordering::SeqCst), 2);
        drop(stream);
        finish(source).await;
        fixture.finish().await;
    }
}

#[tokio::test]
async fn cancellation_interrupts_unchanged_playlist_reload_without_source_retirement() {
    let fixture = Fixture::start(vec![
        Reply::ok("/root", playlist(1, 1, false)),
        Reply::ok("/s1.ts", packets(7, 0x51)),
        Reply::ok("/root", playlist(1, 1, false)),
    ])
    .await;
    let (source, _) = source(&fixture).await;
    let mut stream = source.open_input(limits()).await.unwrap();
    drop(stream.next_chunk().await.unwrap().unwrap());
    let cancel = stream.cancellation();
    let task = tokio::spawn(async move {
        let result = stream.next_chunk().await;
        (stream, result)
    });
    fixture.finish().await;
    cancel.cancel();
    let (stream, result) = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(result, Err(Error::Cancelled)));
    drop(stream);
    assert!(source.check_authority().is_ok());
    finish(source).await;
}
