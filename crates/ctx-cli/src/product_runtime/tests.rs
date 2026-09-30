use super::*;
use ctx_history_server as server_source;
use ctx_history_sharing as sharing_source;

mod consent;
mod delivery;
mod projection;

fn collector(root: &Path) -> Arc<Collector> {
    Arc::new(Collector {
        root: root.to_owned(),
        owner: "synthetic-owner".into(),
        endpoint: "https://example.invalid/telemetry".into(),
        started: Instant::now(),
        memory: Mutex::new(Accumulator::default()),
        flushing: Mutex::new(()),
        active: AtomicBool::new(true),
        limited: AtomicBool::new(false),
    })
}

#[test]
fn hot_callbacks_never_wait_or_access_the_root_and_report_lost_samples() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("must-not-be-created");
    let collector = collector(&root);
    let held = collector.memory.lock().unwrap();
    // A blocking lock would deadlock this test; these callbacks must just drop.
    collector.sharing(sharing_source::SharingObservation::Transfer { bytes: 500 });
    collector.server(server_source::ServerObservation::Execution {
        operation: server_source::ServerOperation::Search,
        duration: Duration::from_millis(10),
        failure: None,
    });
    drop(held);
    collector.sharing(sharing_source::SharingObservation::Transfer { bytes: 3 });
    let (events, terminals) = collector.take().unwrap();
    let [PublicEventV1::SharingSummary(v)] = events.as_slice() else {
        panic!("one observed transfer");
    };
    assert_eq!(v.bytes, Some(measured(3)));
    assert_eq!(v.counts.observed, 1);
    assert!(v.collection_limited);
    assert!(terminals.is_empty());
    assert!(!root.exists());
}

#[test]
fn shared_window_merges_only_correlated_requests_and_retires_once() {
    let mut memory = Accumulator::default();
    let make = |returned| server_source::ServerObservation::Request {
        operation: server_source::ServerOperation::Search,
        duration: Duration::from_millis(20),
        failure: None,
        body_handed_off: Some(true),
        response_class: server_source::ServerResponseClass::Success,
        body_outcome: server_source::ServerBodyOutcome::Complete,
        response_bytes: 40,
        execution: Some(server_source::ServerExecutionFacts {
            duration: Duration::from_millis(5),
            failure: None,
        }),
        read: Some(server_source::ServerReadFacts {
            returned,
            ..Default::default()
        }),
    };
    server::record(&mut memory, make(3), Duration::ZERO, 100);
    // Standalone read observations are a separate population, never joined to
    // whichever request happens to be in flight in this collector.
    server::record(
        &mut memory,
        server_source::ServerObservation::Read {
            operation: server_source::ServerOperation::Search,
            facts: server_source::ServerReadFacts {
                returned: 999,
                ..Default::default()
            },
        },
        Duration::ZERO,
        101,
    );
    server::record(&mut memory, make(0), Duration::ZERO, 102);
    let events = memory.window.take(110);
    assert_eq!(events.len(), 2);
    let PublicEventV1::ServerSummary(v) = &events[0] else {
        panic!("request summary")
    };
    assert_eq!(v.window, Duration::from_secs(10));
    assert_eq!(v.counts.observed, 2);
    let wire::ServerSummaryFacts::Request {
        handoff,
        response_bytes,
        execution,
        read,
        ..
    } = v.facts
    else {
        panic!("request")
    };
    assert_eq!(handoff.complete, 2);
    assert_eq!(
        response_bytes,
        Some(wire::MeasuredTotal {
            samples: 2,
            total: 80
        })
    );
    assert_eq!(execution.unwrap().observed, 2);
    assert_eq!(read.unwrap().returned, 3);
    assert!(memory.window.take(111).is_empty());
}

#[test]
fn one_summary_batch_preserves_saved_counters_and_marks_combined_overflow() {
    let sift = || {
        let mut value = wire::SiftSummaryV1::new(
            wire::SiftOperation::Compact,
            wire::SiftMode::Lossless,
            Duration::ZERO,
        );
        value.observed = 1;
        value.unmeasured = 1;
        value.into_event().unwrap()
    };
    let sharing = || {
        sharing::project(sharing_source::SharingObservation::WorkerStopped)
            .into_event()
            .unwrap()
    };
    let events = combine_summaries(
        (0..32).map(|_| sift()).collect(),
        (0..32).map(|_| sharing()).collect(),
    );
    assert_eq!(events.len(), 50);
    assert!(events[..32]
        .iter()
        .all(|v| matches!(v, PublicEventV1::SiftSummary(v) if v.collection_limited)));
    assert!(events[32..]
        .iter()
        .all(|v| matches!(v, PublicEventV1::SharingSummary(v) if v.collection_limited)));
}

#[test]
fn lifecycle_uses_producer_elapsed_backlog_and_retains_failed_stop() {
    let mut memory = Accumulator::default();
    server::record(
        &mut memory,
        server_source::ServerObservation::Lifecycle {
            kind: server_source::ServerLifecycle::Ready,
            stage: server_source::ServerStage::Serve,
            duration: Duration::from_millis(23),
            failure: None,
            backlog: Some(server_source::ServerBacklog {
                pending_operations_capped: 1000,
                staged_uploads_capped: 8,
                collections_capped: 3,
            }),
        },
        Duration::from_secs(999),
        100,
    );
    server::record(
        &mut memory,
        server_source::ServerObservation::IndexerHealth {
            failure: Some(server_source::ServerFailure::Catalog),
        },
        Duration::from_secs(35),
        135,
    );
    server::record(
        &mut memory,
        server_source::ServerObservation::IndexerHealth { failure: None },
        Duration::from_secs(41),
        141,
    );
    server::record(
        &mut memory,
        server_source::ServerObservation::Lifecycle {
            kind: server_source::ServerLifecycle::Stopped,
            stage: server_source::ServerStage::Shutdown,
            duration: Duration::from_secs(80),
            failure: Some(server_source::ServerFailure::Io),
            backlog: None,
        },
        Duration::from_secs(999),
        180,
    );
    let values = memory
        .terminals
        .iter()
        .map(|e| match e {
            PublicEventV1::ProductRuntime(v) => *v,
            _ => panic!("runtime"),
        })
        .collect::<Vec<_>>();
    assert_eq!(values.len(), 5);
    assert_eq!(values[0].phase, wire::ProductRuntimePhase::Ready);
    assert_eq!(values[0].uptime, Duration::from_millis(23));
    assert_eq!(values[0].pending_work, Some(1000));
    assert_eq!(values[0].active_requests, None);
    assert_eq!(
        values[1].failure.unwrap().class,
        wire::ProductFailureClass::Store
    );
    assert_eq!(values[2].phase, wire::ProductRuntimePhase::Recovered);
    assert_eq!(values[2].uptime, Duration::from_secs(41));
    assert!(values[2].failure.is_none());
    assert_eq!(values[3].phase, wire::ProductRuntimePhase::Failed);
    assert_eq!(
        values[3].failure.unwrap().class,
        wire::ProductFailureClass::Io
    );
    assert_eq!(values[4].phase, wire::ProductRuntimePhase::Stopped);
    assert_eq!(values[3].uptime, values[4].uptime);
    assert_eq!(values[4].uptime, Duration::from_secs(80));
    assert!(
        memory.window.take(180).is_empty(),
        "health recovery is not an indexing run"
    );
}

#[test]
fn runtime_buffer_is_bounded_and_keeps_the_terminal_transition() {
    let mut memory = Accumulator::default();
    for _ in 0..100 {
        server::record(
            &mut memory,
            server_source::ServerObservation::IndexerHealth { failure: None },
            Duration::ZERO,
            100,
        );
    }
    server::record(
        &mut memory,
        server_source::ServerObservation::Lifecycle {
            kind: server_source::ServerLifecycle::Stopped,
            stage: server_source::ServerStage::Shutdown,
            duration: Duration::from_secs(9),
            failure: None,
            backlog: None,
        },
        Duration::ZERO,
        101,
    );
    assert_eq!(memory.terminals.len(), MAX_TERMINALS);
    assert!(
        matches!(memory.terminals.last(), Some(PublicEventV1::ProductRuntime(v)) if v.phase == wire::ProductRuntimePhase::Stopped)
    );
}

#[test]
fn finish_uses_delivery_hook_without_inventing_lifecycle() {
    let ticks = Arc::new(Mutex::new(Vec::new()));
    let capture = ticks.clone();
    let observers = HostedObservers {
        runtime: Some(Arc::new(move |tick| capture.lock().unwrap().push(tick))),
        ..Default::default()
    };
    finish(&observers);
    assert_eq!(
        *ticks.lock().unwrap(),
        [server_source::ServerRuntimeTick::Stopped]
    );
    finish(&HostedObservers::default());
}
