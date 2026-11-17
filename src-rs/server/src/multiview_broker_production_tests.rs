//! Non-media production-boundary tests: no child process, listener or filesystem.
use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

struct TestClock(AtomicU64);
impl Clock for TestClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }
}
fn operator() -> OperatorConfig {
    OperatorConfig {
        server_id: "production-test".into(),
        server_epoch: "epoch-1".into(),
        origin: "http://127.0.0.1:19090".into(),
        listener: ListenerPolicy::default(),
        allowed_origins: vec![],
        signing_key: vec![b's'; 32],
        credentials: vec![CredentialConfig {
            enrollment: Enrollment {
                server_id: "production-test".into(),
                origin: "http://127.0.0.1:19090".into(),
                audience: wire::AUDIENCE.into(),
                installation_id: "installation-1".into(),
                control_bearer: "b".repeat(40),
            },
            subject: "owner-1".into(),
            permissions: BTreeSet::from([Permission::MultiviewControl]),
            revision: 1,
            expires_at_ms: 600_000,
        }],
    }
}
fn binding() -> Binding {
    Binding {
        server_epoch: "epoch-1".into(),
        session_id: "session-1".into(),
        backend_expected: wire::BackendExpected {
            runtime: 1,
            scene: "scene-1".into(),
            geometry_revision: 1,
            focus_revision: 1,
            audio_revision: 1,
        },
        native_expected: wire::NativeExpected {
            runtime: "native-1".into(),
            expected: wire::NativeRevisions {
                scene: "native-scene".into(),
                layout_revision: "layout-1".into(),
                focus_revision: "focus-1".into(),
                audio_revision: "audio-1".into(),
            },
            slot_id: 0,
            slot_generation: "slot-1".into(),
            source_revision: "source-1".into(),
        },
        backend_slot_revision: 0,
    }
}
fn backend() -> BackendConfig {
    let mut media = ottplay_core::multiview::persistent::Config::new(
        "/not-executed/ffmpeg".into(),
        "/not-executed/ffprobe".into(),
        "/not-created/output".into(),
    );
    media.max_duration = Duration::from_secs(3600);
    BackendConfig {
        media,
        profile: wire::OutputProfile {
            profile_id: "mpegts-unix".into(),
            canvas: wire::Canvas {
                width: 320,
                height: 180,
                fps: 5,
            },
            max_inputs: 2,
            geometry_modes: vec![wire::GeometryMode::Disjoint],
        },
        input_adapters: vec!["http-mpegts-v1".into()],
    }
}
fn renew(broker: &Broker, owner: &Authenticated, binding: &Binding) -> Result<(), BoundaryError> {
    let deadline = broker
        .0
        .mutable
        .lock()
        .unwrap()
        .outputs
        .values()
        .next()
        .unwrap()
        .live_lease
        .as_ref()
        .unwrap()
        .owner_end
        .clone();
    broker.renew_live_output(
        owner,
        binding,
        &deadline,
        Instant::now() + Duration::from_secs(180),
    )
}
fn lease_fixture() -> (
    Broker,
    Arc<TestClock>,
    Authenticated,
    Binding,
    Arc<OutputRecord>,
) {
    let clock = Arc::new(TestClock(AtomicU64::new(1000)));
    let registry = Arc::new(Registry::new("production-test".into(), "epoch-1".into()).unwrap());
    let broker = Broker::new(operator(), Some(registry.clone()), clock.clone()).unwrap();
    let owner = Authenticated {
        principal: broker.0.mutable.lock().unwrap().credentials["owner-1"]
            .principal
            .clone(),
        credential_revision: 1,
    };
    let binding = binding();
    registry.create_session(&owner.principal, &binding).unwrap();
    let mut record = OutputRecord {
        authenticated: owner.clone(),
        binding: binding.clone(),
        generation: "generation-1".into(),
        nonce: "a".repeat(32),
        expires: 180_000,
        files: BTreeMap::new(),
        token: String::new(),
        live_lease: Some(LiveLease {
            valid_until: AtomicU64::new(91_000),
            owner_end: Arc::new(Mutex::new(Instant::now() + Duration::from_secs(90))),
        }),
        live: None,
        live_watermark: None,
    };
    record.token = broker.sign(&record).unwrap();
    let record = Arc::new(record);
    broker
        .0
        .mutable
        .lock()
        .unwrap()
        .outputs
        .insert(record.nonce.clone(), record.clone());
    (broker, clock, owner, binding, record)
}

#[test]
fn renew_keeps_signed_path_and_hard_ceiling_but_advances_only_live_deadline() {
    let (broker, clock, owner, binding, record) = lease_fixture();
    let token = record.token.clone();
    clock.0.store(80_000, Ordering::Release);
    renew(&broker, &owner, &binding).unwrap();
    assert_eq!(record.effective_expiry(), 170_000);
    assert_eq!(record.expires, 180_000);
    assert_eq!(record.token, token);
    assert!(broker.verify(&record, &token));
    clock.0.store(160_000, Ordering::Release);
    renew(&broker, &owner, &binding).unwrap();
    assert_eq!(record.effective_expiry(), 180_000);
    clock.0.store(180_000, Ordering::Release);
    assert_eq!(
        renew(&broker, &owner, &binding),
        Err(BoundaryError::Revoked)
    );
}

// The callback refusal is covered end-to-end here with the real Registry.
// Core's refused_renew_commit_does_not_extend_registry_lease additionally
// inspects the private Registry deadline and proves error means no extension.
fn assert_contended_renew_refuses_without_lease_mutation(hold_soft_deadline: bool) {
    let (broker, _clock, owner, binding, record) = lease_fixture();
    let deadline = record.live_lease.as_ref().unwrap().owner_end.clone();
    let original = *deadline.lock().unwrap();
    let state_guard = (!hold_soft_deadline).then(|| broker.0.mutable.lock().unwrap());
    let soft_guard = hold_soft_deadline.then(|| deadline.lock().unwrap());
    let waiting_broker = broker.clone();
    let waiting_deadline = deadline.clone();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        let result = waiting_broker.renew_live_output(
            &owner,
            &binding,
            &waiting_deadline,
            Instant::now() + Duration::from_secs(180),
        );
        done_tx.send(result).unwrap();
    });
    let started = started_rx.recv_timeout(Duration::from_secs(1));
    let result_while_locked = done_rx.recv_timeout(Duration::from_secs(1));
    // Release both guards before assertions/join so the old blocking callback
    // fails this regression without leaving a hung test worker behind.
    drop(soft_guard);
    drop(state_guard);
    worker.join().unwrap();
    assert!(started.is_ok());
    assert_eq!(result_while_locked, Ok(Err(BoundaryError::Unavailable)));
    assert_eq!(
        record
            .live_lease
            .as_ref()
            .unwrap()
            .valid_until
            .load(Ordering::Acquire),
        91_000
    );
    assert_eq!(record.effective_expiry(), 91_000);
    assert_eq!(*deadline.lock().unwrap(), original);
}

#[test]
fn busy_host_state_refuses_renew_without_waiting_or_mutating_lease() {
    assert_contended_renew_refuses_without_lease_mutation(false);
}

#[test]
fn busy_monotonic_deadline_refuses_renew_without_waiting_or_mutating_lease() {
    assert_contended_renew_refuses_without_lease_mutation(true);
}

#[test]
fn credential_expiring_between_admission_and_commit_cannot_renew() {
    struct CommitExpiryClock {
        armed: std::sync::atomic::AtomicBool,
        samples: std::sync::atomic::AtomicUsize,
    }
    impl Clock for CommitExpiryClock {
        fn now_ms(&self) -> u64 {
            if !self.armed.load(Ordering::Acquire) {
                return 1000;
            }
            // Real boundary: admission still authorized; commit is exactly at
            // expiry. No wall-clock sleep or production clock bypass is needed.
            if self.samples.fetch_add(1, Ordering::AcqRel) == 0 {
                599_999
            } else {
                600_000
            }
        }
    }
    let clock = Arc::new(CommitExpiryClock {
        armed: std::sync::atomic::AtomicBool::new(false),
        samples: std::sync::atomic::AtomicUsize::new(0),
    });
    let registry = Arc::new(Registry::new("production-test".into(), "epoch-1".into()).unwrap());
    let broker = Broker::new(operator(), Some(registry.clone()), clock.clone()).unwrap();
    let owner = Authenticated {
        principal: broker.0.mutable.lock().unwrap().credentials["owner-1"]
            .principal
            .clone(),
        credential_revision: 1,
    };
    let binding = binding();
    registry.create_session(&owner.principal, &binding).unwrap();
    // This is a valid session before first output, or between output epochs.
    // No current output record supplies an incidental second credential check.
    assert!(broker.0.mutable.lock().unwrap().outputs.is_empty());
    let original = Instant::now() + Duration::from_secs(90);
    let deadline = Mutex::new(original);
    clock.armed.store(true, Ordering::Release);
    assert_eq!(
        broker.renew_live_output(
            &owner,
            &binding,
            &deadline,
            Instant::now() + Duration::from_secs(180),
        ),
        Err(BoundaryError::Unauthorized)
    );
    assert_eq!(clock.samples.load(Ordering::Acquire), 2);
    assert_eq!(*deadline.lock().unwrap(), original);
    assert!(broker.0.mutable.lock().unwrap().outputs.is_empty());
}

#[test]
fn expired_soft_lease_cannot_be_resurrected_by_registry_renewal() {
    let (broker, clock, owner, binding, record) = lease_fixture();
    clock.0.store(91_000, Ordering::Release);
    broker
        .0
        .registry
        .as_ref()
        .unwrap()
        .renew(&owner.principal, &binding)
        .unwrap();
    assert_eq!(
        renew(&broker, &owner, &binding),
        Err(BoundaryError::Revoked)
    );
    assert_eq!(record.effective_expiry(), 91_000);
    assert!(broker.check_live_output(&record, &record.token).is_err());
}

#[test]
fn media_checks_and_reporting_never_renew_and_held_body_loses_authority() {
    let mut headers = HeaderMap::new();
    assert!(media_attempt_marker(&headers).is_none());
    headers.insert(
        crate::media_connection::attempt::HEADER,
        "41".parse().unwrap(),
    );
    assert_eq!(media_attempt_marker(&headers).unwrap().get(), 41);
    headers.append(
        crate::media_connection::attempt::HEADER,
        "42".parse().unwrap(),
    );
    assert!(media_attempt_marker(&headers).is_none());
    headers.remove(crate::media_connection::attempt::HEADER);
    headers.insert(
        crate::media_connection::attempt::HEADER,
        "0".parse().unwrap(),
    );
    assert!(media_attempt_marker(&headers).is_none());

    let (broker, clock, _owner, _binding, record) = lease_fixture();
    let custody = Arc::new(MediaCustody {
        broker: broker.clone(),
        record: record.clone(),
        token: record.token.clone(),
        failure: ConnectionControl::new().failure_recorder(),
        attempt: None,
        _permit: broker.0.streams.clone().try_acquire_owned().unwrap(),
    });
    let output = Output {
        session_id: record.binding.session_id.clone(),
        server_epoch: record.binding.server_epoch.clone(),
        output_generation: record.generation.clone(),
        playlist_path: format!("{PREFIX}/media/{}/index.m3u8", record.token),
        lease_expires_in_ms: 90_000,
    };
    let mut body = GuardedBody {
        custody: custody.clone(),
        file: "index.m3u8".into(),
        offset: 0,
        failed: false,
        live_bytes: Some(Bytes::from(vec![0u8; CHUNK_BYTES + 1])),
    };
    let mut context = Context::from_waker(futures_util::task::noop_waker_ref());
    clock.0.store(90_999, Ordering::Release);
    assert!(matches!(
        Pin::new(&mut body).poll_next(&mut context),
        Poll::Ready(Some(Ok(_)))
    ));
    assert!(custody.authorized());
    assert_eq!(broker.live_output_remaining(&output), 1);
    assert_eq!(record.effective_expiry(), 91_000);
    clock.0.store(91_000, Ordering::Release);
    assert!(!custody.authorized()); // The connection's final-flush authority too.
    assert!(matches!(
        Pin::new(&mut body).poll_next(&mut context),
        Poll::Ready(Some(Err(_)))
    ));
    assert_eq!(broker.live_output_remaining(&output), 0);
}

#[test]
fn revoked_output_and_credential_cannot_extend_lease() {
    let (broker, _clock, owner, binding, record) = lease_fixture();
    broker.revoke_output(&binding.session_id).unwrap();
    renew(&broker, &owner, &binding).unwrap();
    assert!(broker.check_live_output(&record, &record.token).is_err());
    assert_eq!(record.effective_expiry(), 91_000);
    let (broker, _clock, owner, binding, record) = lease_fixture();
    broker.revoke_credential(&owner.principal.subject).unwrap();
    assert!(renew(&broker, &owner, &binding).is_err());
    assert!(broker.check_live_output(&record, &record.token).is_err());
    assert_eq!(record.effective_expiry(), 91_000);
}

#[test]
fn stale_binding_or_credential_revision_cannot_renew() {
    let (broker, _clock, mut owner, mut binding, record) = lease_fixture();
    binding.backend_expected.focus_revision += 1;
    assert!(renew(&broker, &owner, &binding).is_err());
    binding.backend_expected.focus_revision -= 1;
    owner.credential_revision += 1;
    assert!(renew(&broker, &owner, &binding).is_err());
    assert_eq!(record.effective_expiry(), 91_000);
}

#[test]
fn ordinary_constructor_remains_zero_capacity() {
    let broker = Broker::new(operator(), None, Arc::new(TestClock(AtomicU64::new(1000)))).unwrap();
    assert!(broker.0.live.is_none());
    assert!(broker.0.registry.is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn production_opt_in_preserves_fixture_limit_and_shutdown_is_terminal() {
    let clock = Arc::new(TestClock(AtomicU64::new(1000)));
    let registry = Arc::new(Registry::new("production-test".into(), "epoch-1".into()).unwrap());
    assert!(Broker::new_fixture(operator(), registry.clone(), clock.clone(), backend()).is_err());
    let (broker, _sources) = Broker::new_live(operator(), registry, clock, backend()).unwrap();
    assert_eq!(
        broker.0.live.as_ref().unwrap().input_adapters(),
        ["http-mpegts-v1"]
    );
    broker.shutdown().await.unwrap();
    broker.shutdown().await.unwrap();
    let owner = Authenticated {
        principal: broker.0.mutable.lock().unwrap().credentials["owner-1"]
            .principal
            .clone(),
        credential_revision: 1,
    };
    let request = wire::SessionCreateRequest {
        version: wire::VERSION,
        server_epoch: "epoch-1".into(),
        output_profile_id: "mpegts-unix".into(),
        native_expected: binding().native_expected,
        geometry: wire::Geometry {
            canvas: wire::Canvas {
                width: 320,
                height: 180,
                fps: 5,
            },
            revision: 1,
            mode: wire::GeometryMode::Disjoint,
            rects: vec![],
        },
    };
    assert!(matches!(
        broker
            .0
            .live
            .as_ref()
            .unwrap()
            .create(
                broker.clone(),
                owner,
                request,
                "create-after-shutdown".into(),
                [1; 32]
            )
            .await,
        Err(BoundaryError::Revoked)
    ));
}

#[cfg(unix)]
#[test]
fn production_rejects_unknown_adapters_and_overlong_custody() {
    for invalid in 0..2 {
        let registry = Arc::new(Registry::new("production-test".into(), "epoch-1".into()).unwrap());
        let mut config = backend();
        if invalid == 0 {
            config.input_adapters = vec!["http_hls".into()];
        } else {
            config.media.max_duration = Duration::from_secs(86_401);
        }
        assert!(matches!(
            Broker::new_live(
                operator(),
                registry,
                Arc::new(TestClock(AtomicU64::new(1000))),
                config
            ),
            Err(BoundaryError::Configuration)
        ));
    }
}

#[cfg(not(unix))]
#[test]
fn production_opt_in_explicitly_rejects_unsupported_platform() {
    let registry = Arc::new(Registry::new("production-test".into(), "epoch-1".into()).unwrap());
    assert!(matches!(
        Broker::new_live(
            operator(),
            registry,
            Arc::new(TestClock(AtomicU64::new(1000))),
            backend()
        ),
        Err(BoundaryError::UnsupportedPlatform)
    ));
}

#[test]
fn expired_monotonic_owner_lease_rejects_renew_even_if_wall_clock_moves_back() {
    let (broker, clock, owner, binding, record) = lease_fixture();
    clock.0.store(1, Ordering::Release);
    assert_eq!(
        broker.renew_live_output(
            &owner,
            &binding,
            &Mutex::new(Instant::now() - Duration::from_secs(1)),
            Instant::now() + Duration::from_secs(180)
        ),
        Err(BoundaryError::Revoked)
    );
    assert_eq!(record.effective_expiry(), 91_000);
}

#[test]
fn monotonic_expiry_revokes_body_even_when_test_wall_clock_moves_back() {
    let (broker, clock, _owner, _binding, record) = lease_fixture();
    *record
        .live_lease
        .as_ref()
        .unwrap()
        .owner_end
        .lock()
        .unwrap() = Instant::now() - Duration::from_secs(1);
    clock.0.store(1, Ordering::Release);
    assert!(broker.check_live_output(&record, &record.token).is_err());
    assert_eq!(record.lease_remaining(clock.now_ms()), 0);
}

pub(super) fn broker_for_host_test() -> Broker {
    lease_fixture().0
}
