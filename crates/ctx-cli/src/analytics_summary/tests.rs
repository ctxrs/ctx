use super::*;
use ctx_client_observability::analytics::*;
use std::time::Duration;
fn sample(input: u64, output: u64) -> SiftSummaryV1 {
    let mut v = SiftSummaryV1::new(
        SiftOperation::Compact,
        SiftMode::Presentation,
        Duration::ZERO,
    );
    v.observed = 1;
    v.complete = 1;
    v.bytes = Some(PresentedTotals {
        samples: 1,
        input,
        output,
    });
    v.tokens = Some(PresentedTotals {
        samples: 1,
        input: input / 2,
        output: output / 2,
    });
    let mut bins = [0; 14];
    bins[0] = 1;
    v.latency = Some(bins);
    v
}
#[test]
fn weighted_savings_preserve_actual_totals_expansion_and_unknown_denominators() {
    let mut window = Window::default();
    window.record(Summary::Sift(sample(1000, 100)), 100);
    window.record(Summary::Sift(sample(10, 20)), 101);
    let mut missing = sample(0, 0);
    missing.complete = 0;
    missing.unmeasured = 1;
    missing.bytes = None;
    missing.tokens = None;
    window.record(Summary::Sift(missing), 102);
    let events = window.take(110);
    let [PublicEventV1::SiftSummary(v)] = events.as_slice() else {
        panic!("one cohort")
    };
    assert_eq!((v.observed, v.complete, v.unmeasured), (3, 2, 1));
    assert_eq!(
        v.bytes,
        Some(PresentedTotals {
            samples: 2,
            input: 1010,
            output: 120
        })
    );
    assert_eq!(
        v.tokens,
        Some(PresentedTotals {
            samples: 2,
            input: 505,
            output: 60
        })
    );
    assert_eq!(v.window, Duration::from_secs(10));
    assert!(
        window.take(111).is_empty(),
        "taken counters are not a retry queue"
    );
}
#[test]
fn output_failure_and_child_failure_are_independent_cohorts_without_false_savings() {
    let mut failed = sample(100, 20);
    failed.output_failed = 1;
    failed.complete = 0;
    failed.unmeasured = 1;
    failed.bytes = None;
    failed.tokens = None;
    failed.cohort.delivery = Some(SiftDelivery::Failed);
    let mut child = sample(100, 20);
    child.execution_failed = 1;
    child.cohort.child = Some(SiftChildOutcome::ExitedNonzero);
    let mut window = Window::default();
    window.record(Summary::Sift(failed), 1);
    window.record(Summary::Sift(child), 1);
    let events = window.take(2);
    assert_eq!(events.len(), 2);
    let PublicEventV1::SiftSummary(failed) = events[0] else {
        panic!()
    };
    assert_eq!((failed.execution_failed, failed.output_failed), (0, 1));
    assert!(failed.bytes.is_none());
    let PublicEventV1::SiftSummary(child) = events[1] else {
        panic!()
    };
    assert_eq!((child.execution_failed, child.output_failed), (1, 0));
    assert_eq!(child.bytes.unwrap().output, 20);
}
#[test]
fn counter_overflow_drops_one_whole_sample_and_marks_the_retained_window() {
    let mut v = sample(100, 20);
    v.observed = u64::MAX;
    v.complete = u64::MAX;
    v.bytes = None;
    v.tokens = None;
    v.latency = None;
    let mut window = Window::default();
    window.record(Summary::Sift(v), 1);
    window.record(Summary::Sift(sample(100, 20)), 2);
    let events = window.take(3);
    let [PublicEventV1::SiftSummary(v)] = events.as_slice() else {
        panic!()
    };
    assert_eq!((v.observed, v.complete), (u64::MAX, u64::MAX));
    assert!(v.bytes.is_none());
    assert!(v.collection_limited);
}
#[test]
fn correlated_server_reads_keep_empty_results_missingness_and_response_handoff() {
    let make = |body, returned: u64, complete: Option<bool>| {
        let mut v = ServerSummaryV1::new(
            ServerOperation::Search,
            Duration::ZERO,
            ServerSummaryFacts::Request {
                handoff: HandoffCounts {
                    complete: u64::from(body == ServerBodyOutcome::Complete),
                    unknown: u64::from(body != ServerBodyOutcome::Complete),
                    failed: 0,
                },
                body: Some(body),
                response_bytes: Some(MeasuredTotal {
                    samples: 1,
                    total: 100,
                }),
                execution: Some(WindowCounts {
                    observed: 1,
                    failed: 0,
                    latency: None,
                }),
                read: Some(ServerReadTotals {
                    observed: 1,
                    returned,
                    nonempty: u64::from(returned > 0),
                    complete: complete.map(|v| MeasuredTotal {
                        samples: 1,
                        total: u64::from(v),
                    }),
                    ..Default::default()
                }),
            },
        );
        v.counts = WindowCounts {
            observed: 1,
            failed: 0,
            latency: None,
        };
        v.response_class = Some(ResponseClass::Success);
        v
    };
    let mut window = Window::default();
    window.record(
        Summary::Server(make(ServerBodyOutcome::Complete, 0, None)),
        1,
    );
    window.record(
        Summary::Server(make(ServerBodyOutcome::Complete, 5, Some(true))),
        2,
    );
    window.record(
        Summary::Server(make(ServerBodyOutcome::Dropped, 5, Some(true))),
        3,
    );
    let events = window.take(4);
    assert_eq!(events.len(), 2);
    let PublicEventV1::ServerSummary(v) = events[0] else {
        panic!()
    };
    let ServerSummaryFacts::Request {
        handoff,
        read: Some(read),
        response_bytes,
        ..
    } = v.facts
    else {
        panic!()
    };
    assert_eq!(handoff.complete, 2);
    assert_eq!((read.observed, read.nonempty, read.returned), (2, 1, 5));
    assert_eq!(
        read.complete,
        Some(MeasuredTotal {
            samples: 1,
            total: 1
        })
    );
    assert_eq!(response_bytes.unwrap().total, 200);
    let PublicEventV1::ServerSummary(v) = events[1] else {
        panic!()
    };
    let ServerSummaryFacts::Request { handoff, .. } = v.facts else {
        panic!()
    };
    assert_eq!((handoff.unknown, handoff.complete), (1, 0));
}
#[test]
fn cohort_capacity_reports_collection_loss_without_fabricating_observation_counts() {
    let mut window = Window::default();
    let operations = [
        SiftOperation::Compact,
        SiftOperation::Restore,
        SiftOperation::Run,
        SiftOperation::Proxy,
        SiftOperation::Filter,
        SiftOperation::Read,
        SiftOperation::Json,
        SiftOperation::Summary,
        SiftOperation::Err,
        SiftOperation::Test,
        SiftOperation::Gain,
        SiftOperation::Config,
        SiftOperation::Semantic,
        SiftOperation::Discover,
        SiftOperation::Recall,
        SiftOperation::Hook,
        SiftOperation::Rewrite,
    ];
    for op in operations {
        for host in [SiftHost::Claude, SiftHost::Codex] {
            let mut v = sample(100, 20);
            v.operation = op;
            v.host = host;
            window.record(Summary::Sift(v), 1);
        }
    }
    let events = window.take(2);
    assert_eq!(events.len(), 32);
    for event in events {
        let PublicEventV1::SiftSummary(v) = event else {
            panic!()
        };
        assert!(v.collection_limited);
        assert_eq!(v.observed, 1);
    }
}
#[test]
fn private_sift_window_is_taken_once_and_never_initializes_history_stores() {
    let _lock = ctx_app_config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let _env =
        crate::observability_composition::consent_tests::isolate_analytics_environment(temp.path());
    let root = temp.path().join("history-owner");
    crate::observability_composition::consent_tests::configure(
        &root,
        true,
        "https://telemetry.example/v1",
    );
    record_sift(&root, sample(1000, 100));
    record_sift(&root, sample(10, 20));
    let owner = crate::identity::existing_installation_id(&root)
        .unwrap()
        .unwrap();
    assert!(!state_path(&root, &owner).unwrap().starts_with(&root));
    let events = take_saved(&root, &owner);
    let [PublicEventV1::SiftSummary(v)] = events.as_slice() else {
        panic!()
    };
    assert_eq!(v.observed, 2);
    assert_eq!(v.bytes.unwrap().input, 1010);
    assert!(take_saved(&root, &owner).is_empty());
    for path in ["core", "usage.sqlite", "sharing"] {
        assert!(!root.join(path).exists());
    }
}
#[test]
fn opt_out_endpoint_and_version_changes_drop_private_counters_without_retagging() {
    let _lock = ctx_app_config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let _env =
        crate::observability_composition::consent_tests::isolate_analytics_environment(temp.path());
    let root = temp.path().join("history-owner");
    let configure = |enabled, endpoint| {
        crate::observability_composition::consent_tests::configure(&root, enabled, endpoint)
    };
    configure(true, "https://one.example/v1");
    record_sift(&root, sample(100, 20));
    let owner = crate::identity::existing_installation_id(&root)
        .unwrap()
        .unwrap();
    configure(true, "https://two.example/v1");
    assert!(take_saved(&root, &owner).is_empty());
    record_sift(&root, sample(100, 20));
    configure(false, "https://two.example/v1");
    assert!(take_saved(&root, &owner).is_empty());
    configure(true, "https://two.example/v1");
    assert!(take_saved(&root, &owner).is_empty());
    let path = state_path(&root, &owner).unwrap();
    let mut file = crate::analytics_state::StateFile::try_open(&path)
        .unwrap()
        .unwrap();
    let mut old = SavedWindow::new(&owner, "https://two.example/v1", 100);
    old.producer_version = "0.0.1".into();
    old.window.record(Summary::Sift(sample(100, 20)), 100);
    file.write(&old).unwrap();
    drop(file);
    assert!(take_saved(&root, &owner).is_empty());
    record_sift(&root, sample(100, 20));
    assert_eq!(take_saved(&root, &owner).len(), 1);
}

#[test]
fn an_opted_out_unused_root_does_not_get_identity_or_counter_state() {
    let _lock = ctx_app_config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let _env =
        crate::observability_composition::consent_tests::isolate_analytics_environment(temp.path());
    let root = temp.path().join("unused-owner");
    // Explicit global opt-out precedes a missing config or root.
    let previous = std::env::var_os("CTX_ANALYTICS_ENABLED");
    std::env::set_var("CTX_ANALYTICS_ENABLED", "false");
    record_sift(&root, sample(100, 20));
    assert!(!root.exists());
    match previous {
        Some(v) => std::env::set_var("CTX_ANALYTICS_ENABLED", v),
        None => std::env::remove_var("CTX_ANALYTICS_ENABLED"),
    }
}
#[test]
fn a_taken_window_starts_a_clean_new_window_without_a_false_loss_marker() {
    let _lock = ctx_app_config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let _env =
        crate::observability_composition::consent_tests::isolate_analytics_environment(temp.path());
    let root = temp.path().join("history-owner");
    crate::observability_composition::consent_tests::configure(
        &root,
        true,
        "https://telemetry.example/v1",
    );
    record_sift(&root, sample(100, 20));
    let owner = crate::identity::existing_installation_id(&root)
        .unwrap()
        .unwrap();
    assert_eq!(take_saved(&root, &owner).len(), 1);
    record_sift(&root, sample(100, 20));
    let events = take_saved(&root, &owner);
    let [PublicEventV1::SiftSummary(v)] = events.as_slice() else {
        panic!()
    };
    assert!(!v.collection_limited);
    assert_eq!(v.observed, 1);
}
#[test]
fn root_replacement_cannot_publish_the_previous_owners_sift_window() {
    let _lock = ctx_app_config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let _env =
        crate::observability_composition::consent_tests::isolate_analytics_environment(temp.path());
    let root = temp.path().join("history-owner");
    let configure = || {
        crate::observability_composition::consent_tests::configure(
            &root,
            true,
            "https://telemetry.example/v1",
        )
    };
    configure();
    record_sift(&root, sample(100, 20));
    let old = crate::identity::existing_installation_id(&root)
        .unwrap()
        .unwrap();
    std::fs::rename(&root, temp.path().join("previous-owner")).unwrap();
    configure();
    let current = crate::identity::installation_id(&root).unwrap();
    assert_ne!(old, current);
    assert!(take_saved(&root, &old).is_empty());
    assert!(take_saved(&root, &current).is_empty());
    record_sift(&root, sample(20, 10));
    let events = take_saved(&root, &current);
    let [PublicEventV1::SiftSummary(v)] = events.as_slice() else {
        panic!()
    };
    assert_eq!(v.bytes.unwrap().input, 20);
}
