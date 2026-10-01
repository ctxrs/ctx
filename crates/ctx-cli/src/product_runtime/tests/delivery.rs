use super::*;
use crate::observability_composition::consent_tests::{configure, isolate_analytics_environment};
use fs2::FileExt as _;

const ENDPOINT: &str = "https://telemetry.example/v1";

fn outbox(collector: &Collector) -> crate::analytics_outbox::AnalyticsOutbox {
    let path =
        crate::identity::device_state_path("analytics-outbox-v1.json", &collector.root).unwrap();
    crate::analytics_outbox::AnalyticsOutbox::open(path, &collector.owner).unwrap()
}

fn ready(collector: &Collector) {
    collector.server(server_source::ServerObservation::Lifecycle {
        kind: server_source::ServerLifecycle::Ready,
        stage: server_source::ServerStage::Serve,
        duration: Duration::from_millis(25),
        failure: None,
        backlog: None,
    });
}

#[test]
fn busy_identity_defers_without_waiting_then_admits_fresh_observations() {
    let _lock = ctx_app_config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let _env = isolate_analytics_environment(temp.path());
    let root = temp.path().join("owner");
    configure(&root, true, ENDPOINT);
    let collector = Collector::capture(&root).unwrap();
    let original_owner = collector.owner.clone();
    let identity = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(crate::identity::install_path(&root))
        .unwrap();
    identity.lock_exclusive().unwrap();
    collector.sharing(sharing_source::SharingObservation::Transfer { bytes: 7 });
    // A blocking lookup would deadlock while this same thread owns the lock.
    collector.flush(false);
    assert!(collector.active.load(Ordering::Relaxed));
    assert_eq!(collector.binding(), Binding::Deferred);
    fs2::FileExt::unlock(&identity).unwrap();
    collector.sharing(sharing_source::SharingObservation::Transfer { bytes: 5 });
    collector.flush(false);
    assert_eq!(collector.owner, original_owner);
    assert_eq!(collector.binding(), Binding::Current);
    let snapshot = outbox(&collector).snapshot(ENDPOINT).unwrap();
    assert_eq!(snapshot.len(), 1);
    let payload: serde_json::Value = serde_json::from_slice(snapshot[0].payload()).unwrap();
    assert_eq!(
        payload["events"][0]["properties"]["observed_count_bucket"],
        "2-5"
    );
}

#[test]
fn finite_finish_never_contacts_stalled_transport_and_retains_output_failure() {
    let _lock = ctx_app_config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let _env = isolate_analytics_environment(temp.path());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/telemetry", listener.local_addr().unwrap());
    let root = temp.path().join("finite-owner");
    configure(&root, true, &endpoint);
    let preload = Collector::capture(&root).unwrap();
    ready(&preload);
    preload.flush(false);
    // Defer the already tested child launcher so this test isolates HTTP on
    // the foreground path, without spawning a unit-test executable as a child.
    let claim = crate::identity::device_state_path("analytics-launch-v1.json", &root).unwrap();
    crate::analytics_state::StateFile::try_open(&claim)
        .unwrap()
        .unwrap()
        .write(&serde_json::json!({"schema_version": 1, "next_allowed_at": now() + 60}))
        .unwrap();
    let observers = hosted(Some(&root));
    observers.completion.as_ref().unwrap()(crate::hosted::HostedCompletion {
        operation: crate::hosted::HostedOperation::ArchiveVerify,
        output: wire::OutputKind::Human,
        duration: Duration::from_millis(1),
        result: Err(wire::HostedFailureV1::Output),
    });
    finish(&observers);
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "a foreground drain would have connected to this deliberately unanswered listener"
    );
    let snapshot = outbox(&preload).snapshot(&endpoint).unwrap();
    assert_eq!(
        snapshot.len(),
        2,
        "finite finish admits locally without draining older events"
    );
    let payload: serde_json::Value = serde_json::from_slice(snapshot[1].payload()).unwrap();
    assert_eq!(payload["events"][0]["outcome"], "failure");
    assert_eq!(
        payload["events"][0]["properties"]["hosted_failure_stage"],
        "output"
    );
}

#[test]
fn live_runtime_tick_still_delivers_through_the_existing_bounded_drain() {
    let _lock = ctx_app_config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let _env = isolate_analytics_environment(temp.path());
    let sink = temp.path().join("received.jsonl");
    let endpoint = url::Url::from_file_path(&sink).unwrap().to_string();
    let root = temp.path().join("runtime-owner");
    configure(&root, true, &endpoint);
    let observers = hosted(Some(&root));
    observers.server.as_ref().unwrap()(server_source::ServerObservation::Lifecycle {
        kind: server_source::ServerLifecycle::Ready,
        stage: server_source::ServerStage::Serve,
        duration: Duration::from_millis(25),
        failure: None,
        backlog: None,
    });
    observers.runtime.as_ref().unwrap()(server_source::ServerRuntimeTick::Ready);
    let payload: serde_json::Value =
        serde_json::from_str(std::fs::read_to_string(sink).unwrap().trim()).unwrap();
    assert_eq!(payload["events"][0]["properties"]["runtime_phase"], "ready");
    let owner = crate::identity::try_existing_installation_id(&root)
        .unwrap()
        .unwrap();
    let path = crate::identity::device_state_path("analytics-outbox-v1.json", &root).unwrap();
    assert!(crate::analytics_outbox::AnalyticsOutbox::open(path, &owner)
        .unwrap()
        .snapshot(&endpoint)
        .unwrap()
        .is_empty());
}

#[test]
fn opt_out_purges_captured_owner_after_replacement_and_preserves_replacement_bytes() {
    let _lock = ctx_app_config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let _env = isolate_analytics_environment(temp.path());
    let root = temp.path().join("owner");
    configure(&root, true, ENDPOINT);
    let old = Collector::capture(&root).unwrap();
    ready(&old);
    old.flush(false);
    std::fs::rename(&root, temp.path().join("previous-owner")).unwrap();
    configure(&root, true, ENDPOINT);
    let replacement = Collector::capture(&root).unwrap();
    assert_ne!(old.owner, replacement.owner);
    ready(&replacement);
    replacement.flush(false);
    let replacement_outbox = outbox(&replacement);
    let before = replacement_outbox.snapshot(ENDPOINT).unwrap();
    assert_eq!(before.len(), 1);
    configure(&root, false, ENDPOINT);
    old.flush(false);
    assert!(!old.active.load(Ordering::Relaxed));
    assert!(outbox(&old).snapshot(ENDPOINT).unwrap().is_empty());
    let after = replacement_outbox.snapshot(ENDPOINT).unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].payload(), before[0].payload());
    // A fresh disabled factory now observes the replacement owner's opt-out.
    let observers = hosted(Some(&root));
    assert!(observers.runtime.is_none());
    assert!(replacement_outbox.snapshot(ENDPOINT).unwrap().is_empty());
}

#[test]
fn malformed_config_defers_without_destroying_queued_or_saved_observations() {
    let _lock = ctx_app_config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let _env = isolate_analytics_environment(temp.path());
    let root = temp.path().join("owner");
    configure(&root, true, ENDPOINT);
    let collector = Collector::capture(&root).unwrap();
    ready(&collector);
    collector.flush(false);
    let mut sift = wire::SiftSummaryV1::new(
        wire::SiftOperation::Compact,
        wire::SiftMode::Lossless,
        Duration::ZERO,
    );
    sift.observed = 1;
    sift.unmeasured = 1;
    crate::analytics_summary::record_sift_for_owner(&root, &collector.owner, ENDPOINT, sift);
    let saved = crate::identity::device_state_path(
        &format!("analytics-summary-{}.json", collector.owner),
        &root,
    )
    .unwrap();
    let queue = crate::identity::device_state_path("analytics-outbox-v1.json", &root).unwrap();
    let saved_before = std::fs::read(&saved).unwrap();
    let queued_before = std::fs::read(&queue).unwrap();
    let config_path = ctx_app_config::AppConfig::config_path(&root);
    let config_before = std::fs::read(&config_path).unwrap();
    std::fs::write(&config_path, b"[analytics]\nenabled = [").unwrap();
    collector.sharing(sharing_source::SharingObservation::Transfer { bytes: 9 });
    collector.flush(false);
    assert!(collector.active.load(Ordering::Relaxed));
    assert_eq!(collector.binding(), Binding::Deferred);
    assert_eq!(std::fs::read(&saved).unwrap(), saved_before);
    assert_eq!(std::fs::read(&queue).unwrap(), queued_before);
    assert!(hosted(Some(&root)).runtime.is_none());
    assert_eq!(std::fs::read(&saved).unwrap(), saved_before);
    assert_eq!(std::fs::read(&queue).unwrap(), queued_before);
    // Restore the valid fixture bytes without asking a config writer to parse corruption.
    std::fs::write(&config_path, config_before).unwrap();
    collector.flush(false);
    let snapshot = outbox(&collector).snapshot(ENDPOINT).unwrap();
    assert_eq!(snapshot.len(), 2);
    let payload: serde_json::Value = serde_json::from_slice(snapshot[1].payload()).unwrap();
    let events = payload["events"].as_array().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["operation"], "sift_summary");
    assert_eq!(events[1]["operation"], "sharing_summary");
    assert!(crate::analytics_summary::take_saved(&root, &collector.owner).is_empty());
}
