use super::*;
use crate::observability_composition::consent_tests::{configure, isolate_analytics_environment};

const ENDPOINT: &str = "https://telemetry.example/v1";

fn hosted_command(arguments: &[&str]) -> crate::hosted::HostedCommand {
    use clap::Parser;
    let cli = crate::Cli::try_parse_from(std::iter::once("ctx").chain(arguments.iter().copied()))
        .unwrap();
    let crate::cli::CommandRoot::Hosted(command) = cli.command else {
        panic!("hosted command expected");
    };
    command
}

#[test]
fn only_server_run_and_remote_sync_require_live_observers() {
    for (arguments, live) in [
        (vec!["server", "run"], true),
        (vec!["remote", "sync", "team"], true),
        (
            vec![
                "archive", "export", "--origin", "team", "--output", "archive",
            ],
            false,
        ),
        (vec!["archive", "verify", "archive"], false),
        (vec!["archive", "restore", "archive"], false),
        (vec!["server", "init"], false),
        (vec!["server", "backup", "--output", "checkpoint"], false),
        (vec!["server", "restore", "checkpoint"], false),
        (vec!["server", "status"], false),
        (vec!["server", "collection", "create", "team"], false),
        (vec!["server", "user", "create", "member"], false),
        (vec!["server", "user", "list"], false),
        (vec!["server", "user", "credentials", "member"], false),
        (
            vec![
                "server",
                "user",
                "credential",
                "member",
                "--read",
                "--output",
                "token",
            ],
            false,
        ),
        (vec!["server", "invite", "member"], false),
        (vec!["server", "grant", "--user", "member", "--read"], false),
        (vec!["server", "revoke", "--user", "member"], false),
        (
            vec!["server", "withdraw", "--publication", "publication"],
            false,
        ),
        (vec!["server", "publications"], false),
        (vec!["remote", "connect", "https://example.invalid"], false),
        (
            vec![
                "remote",
                "share",
                "team",
                "--profile-root",
                "/synthetic",
                "--whole-source",
            ],
            false,
        ),
        (vec!["remote", "pause", "team"], false),
        (vec!["remote", "pause", "team", "--resume"], false),
        (vec!["remote", "status", "team", "--online"], false),
        (vec!["remote", "remove", "team"], false),
    ] {
        assert_eq!(
            hosted_command(&arguments).needs_live_observers(),
            live,
            "{arguments:?}"
        );
    }
}

#[test]
fn disabled_factory_does_not_create_identity_and_suppresses_legacy_completion() {
    let _lock = ctx_app_config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let _env = isolate_analytics_environment(temp.path());
    let root = temp.path().join("disabled-owner");
    configure(&root, false, ENDPOINT);
    let observers = hosted(Some(&root));
    assert!(observers.server.is_none());
    assert!(observers.sharing.is_none());
    assert!(observers.runtime.is_none());
    // Some(no-op) prevents the hosted producer's legacy append fallback.
    observers.completion.unwrap()(crate::hosted::HostedCompletion {
        operation: crate::hosted::HostedOperation::ArchiveVerify,
        output: wire::OutputKind::Human,
        duration: Duration::from_millis(1),
        result: Ok(()),
    });
    assert!(!crate::identity::install_path(&root).exists());
}

#[test]
fn changed_consent_endpoint_or_owner_retires_collector_without_retagging() {
    let _lock = ctx_app_config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let _env = isolate_analytics_environment(temp.path());
    for change in ["opt-out", "endpoint", "owner"] {
        let root = temp.path().join(change);
        configure(&root, true, ENDPOINT);
        let collector = Collector::capture(&root).unwrap();
        let path = crate::identity::device_state_path("analytics-outbox-v1.json", &root).unwrap();
        collector.sharing(sharing_source::SharingObservation::Transfer { bytes: 100 });
        match change {
            "opt-out" => configure(&root, false, ENDPOINT),
            "endpoint" => configure(&root, true, "https://replacement.example/v1"),
            "owner" => {
                std::fs::rename(&root, temp.path().join("old-owner")).unwrap();
                configure(&root, true, ENDPOINT);
                let owner = crate::identity::installation_id(&root).unwrap();
                assert_ne!(collector.owner, owner);
            }
            _ => unreachable!(),
        }
        collector.flush(false);
        assert!(!collector.active.load(Ordering::Relaxed));
        assert!(collector.take().unwrap().0.is_empty());
        // Re-enable the old configuration; the retired observer stays retired.
        configure(&root, true, ENDPOINT);
        collector.sharing(sharing_source::SharingObservation::Transfer { bytes: 200 });
        collector.flush(false);
        assert!(collector.take().unwrap().0.is_empty());
        assert!(!path.exists(), "retired observations must not be admitted");
    }
}

#[test]
fn saved_sift_and_live_sharing_share_one_admission_and_do_not_open_history() {
    let _lock = ctx_app_config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let _env = isolate_analytics_environment(temp.path());
    let root = temp.path().join("private-root-marker");
    configure(&root, true, ENDPOINT);
    let collector = Collector::capture(&root).unwrap();
    let mut sift = wire::SiftSummaryV1::new(
        wire::SiftOperation::Compact,
        wire::SiftMode::Lossless,
        Duration::ZERO,
    );
    sift.observed = 1;
    sift.unmeasured = 1;
    crate::analytics_summary::record_sift(&root, sift);
    collector.sharing(sharing_source::SharingObservation::Transfer { bytes: 33 });
    collector.flush(false); // Admission only; no network, daemon or delivery service.
    let path = crate::identity::device_state_path("analytics-outbox-v1.json", &root).unwrap();
    let outbox = crate::analytics_outbox::AnalyticsOutbox::open(path, &collector.owner).unwrap();
    let snapshot = outbox.snapshot(ENDPOINT).unwrap();
    assert_eq!(snapshot.len(), 1);
    let payload: serde_json::Value = serde_json::from_slice(snapshot[0].payload()).unwrap();
    let events = payload["events"].as_array().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["operation"], "sift_summary");
    assert_eq!(events[1]["operation"], "sharing_summary");
    assert!(!String::from_utf8_lossy(snapshot[0].payload()).contains("private-root-marker"));
    collector.flush(false);
    assert_eq!(
        outbox.snapshot(ENDPOINT).unwrap().len(),
        1,
        "retired windows cannot replay"
    );
    assert!(crate::analytics_summary::take_saved(&root, &collector.owner).is_empty());
    for name in ["core", "search", "usage.sqlite", "sharing"] {
        assert!(!root.join(name).exists());
    }
}

#[test]
fn daemon_destinations_share_one_root_collector_and_empty_upload_flushes_it() {
    let _lock = ctx_app_config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let _env = isolate_analytics_environment(temp.path());
    let root = temp.path().join("daemon-owner");
    configure(&root, true, ENDPOINT);
    let daemon = DaemonCollector::new();
    let a = daemon.sharing_observer(&root).unwrap();
    let b = daemon.sharing_observer(&root).unwrap();
    a(sharing_source::SharingObservation::WorkerStarted);
    b(sharing_source::SharingObservation::WorkerStarted);
    assert!(daemon
        .sharing_observer(&temp.path().join("other-root"))
        .is_none());
    assert!(!temp.path().join("other-root").exists());
    // The periodic uploader has no requirement for an ordinary event to flush.
    crate::analytics::flush_daemon_summaries(&root, &daemon);
    let owner = crate::identity::existing_installation_id(&root)
        .unwrap()
        .unwrap();
    let path = crate::identity::device_state_path("analytics-outbox-v1.json", &root).unwrap();
    let outbox = crate::analytics_outbox::AnalyticsOutbox::open(path, &owner).unwrap();
    let snapshot = outbox.snapshot(ENDPOINT).unwrap();
    assert_eq!(snapshot.len(), 1);
    let payload: serde_json::Value = serde_json::from_slice(snapshot[0].payload()).unwrap();
    assert_eq!(
        payload["events"][0]["properties"]["observed_count_bucket"],
        "2-5"
    );
    // Opt out before terminal flush: this exercise must never call transport.
    configure(&root, false, ENDPOINT);
    a(sharing_source::SharingObservation::WorkerStopped);
    b(sharing_source::SharingObservation::WorkerStopped);
    daemon.finish(&root);
    assert!(daemon.sharing_observer(&root).is_none());
}

#[test]
fn daemon_without_sharing_materializes_standalone_sift_once() {
    let _lock = ctx_app_config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let _env = isolate_analytics_environment(temp.path());
    let root = temp.path().join("sift-only-owner");
    configure(&root, true, ENDPOINT);
    let mut sift = wire::SiftSummaryV1::new(
        wire::SiftOperation::Compact,
        wire::SiftMode::Lossless,
        Duration::ZERO,
    );
    sift.observed = 1;
    sift.unmeasured = 1;
    crate::analytics_summary::record_sift(&root, sift);
    let daemon = DaemonCollector::new();
    assert!(!daemon.flush(&root));
    crate::analytics::flush_daemon_summaries(&root, &daemon);
    crate::analytics::flush_daemon_summaries(&root, &daemon);
    let owner = crate::identity::existing_installation_id(&root)
        .unwrap()
        .unwrap();
    let path = crate::identity::device_state_path("analytics-outbox-v1.json", &root).unwrap();
    let outbox = crate::analytics_outbox::AnalyticsOutbox::open(path, &owner).unwrap();
    let snapshot = outbox.snapshot(ENDPOINT).unwrap();
    assert_eq!(snapshot.len(), 1);
    let payload: serde_json::Value = serde_json::from_slice(snapshot[0].payload()).unwrap();
    assert_eq!(payload["events"].as_array().unwrap().len(), 1);
    assert_eq!(payload["events"][0]["operation"], "sift_summary");
    assert!(crate::analytics_summary::take_saved(&root, &owner).is_empty());
    assert!(!root.join("sharing").exists());
    crate::analytics_summary::record_sift(&root, sift);
    configure(&root, false, ENDPOINT);
    daemon.finish(&root); // Retires saved counters on opt-out, without transport.
    configure(&root, true, ENDPOINT);
    assert!(crate::analytics_summary::take_saved(&root, &owner).is_empty());
}

#[test]
fn deferred_restore_observer_leaves_destination_untouched_until_success() {
    let _lock = ctx_app_config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let _env = isolate_analytics_environment(temp.path());
    let root = temp.path().join("restore-destination");
    let observers = hosted_after_completion(
        Some(&root),
        &hosted_command(&["archive", "restore", "synthetic-archive"]),
    );
    assert!(
        !root.exists(),
        "telemetry must not pre-create an archive destination"
    );
    assert!(
        observers.server.is_none() && observers.sharing.is_none() && observers.runtime.is_none()
    );
    configure(&root, true, ENDPOINT); // Simulate the archive owning its destination first.
    observers.completion.unwrap()(crate::hosted::HostedCompletion {
        operation: crate::hosted::HostedOperation::Existing(
            wire::HostedOperationV1::ArchiveRestore,
        ),
        output: wire::OutputKind::Json,
        duration: Duration::ZERO,
        result: Ok(()),
    });
    let owner = crate::identity::try_existing_installation_id(&root)
        .unwrap()
        .unwrap();
    let path = crate::identity::device_state_path("analytics-outbox-v1.json", &root).unwrap();
    let entries = crate::analytics_outbox::AnalyticsOutbox::open(path, &owner)
        .unwrap()
        .snapshot(ENDPOINT)
        .unwrap();
    assert_eq!(entries.len(), 1);
    let payload: serde_json::Value = serde_json::from_slice(entries[0].payload()).unwrap();
    assert_eq!(payload["events"][0]["operation"], "archive_restore");
    assert_eq!(
        payload["events"][0]["properties"]["output_delivery"],
        "known_complete"
    );
    assert!(!root.join("search").exists() && !root.join("daemon").exists());
}

#[test]
fn failed_restore_and_failed_error_output_do_not_create_identity_in_unowned_targets() {
    let _lock = ctx_app_config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let _env = isolate_analytics_environment(temp.path());
    for operation in [
        wire::HostedOperationV1::ArchiveRestore,
        wire::HostedOperationV1::ServerRestore,
    ] {
        let command = hosted_command(&[
            if operation == wire::HostedOperationV1::ArchiveRestore {
                "archive"
            } else {
                "server"
            },
            "restore",
            "malformed",
        ]);
        for empty in [false, true] {
            let root = temp.path().join(format!("{operation:?}-{empty}"));
            if empty {
                ctx_history_platform::platform_security::create_private_directory_all(&root)
                    .unwrap();
            }
            let observers = hosted_after_completion(Some(&root), &command);
            for failure in [
                wire::HostedFailureV1::Operation(wire::HostedFailureTypeV1::InvalidArchive),
                wire::HostedFailureV1::Output,
            ] {
                observers.completion.as_ref().unwrap()(crate::hosted::HostedCompletion {
                    operation: crate::hosted::HostedOperation::Existing(operation),
                    output: wire::OutputKind::Json,
                    duration: Duration::ZERO,
                    result: Err(failure),
                });
                finish(&observers);
                assert_eq!(root.exists(), empty);
                if empty {
                    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
                }
                assert!(
                    !crate::identity::device_state_path("analytics-outbox-v1.json", &root)
                        .unwrap()
                        .exists()
                );
            }
        }
    }
}

#[test]
fn failed_restore_can_record_for_existing_identity_without_a_restore_marker() {
    let _lock = ctx_app_config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let _env = isolate_analytics_environment(temp.path());
    let root = temp.path().join("existing-owner");
    configure(&root, true, ENDPOINT);
    let owner = crate::identity::installation_id(&root).unwrap();
    let identity = std::fs::read(crate::identity::install_path(&root)).unwrap();
    let observers = hosted_after_completion(
        Some(&root),
        &hosted_command(&["archive", "restore", "malformed"]),
    );
    observers.completion.as_ref().unwrap()(crate::hosted::HostedCompletion {
        operation: crate::hosted::HostedOperation::Existing(
            wire::HostedOperationV1::ArchiveRestore,
        ),
        output: wire::OutputKind::Json,
        duration: Duration::ZERO,
        result: Err(wire::HostedFailureV1::Operation(
            wire::HostedFailureTypeV1::InvalidArchive,
        )),
    });
    finish(&observers);
    assert_eq!(
        std::fs::read(crate::identity::install_path(&root)).unwrap(),
        identity
    );
    let path = crate::identity::device_state_path("analytics-outbox-v1.json", &root).unwrap();
    let entries = crate::analytics_outbox::AnalyticsOutbox::open(path, &owner)
        .unwrap()
        .snapshot(ENDPOINT)
        .unwrap();
    assert_eq!(entries.len(), 1);
    let payload: serde_json::Value = serde_json::from_slice(entries[0].payload()).unwrap();
    assert_eq!(payload["events"].as_array().unwrap().len(), 1);
    assert_eq!(payload["events"][0]["operation"], "archive_restore");
    assert_eq!(payload["events"][0]["outcome"], "failure");
    assert_eq!(
        payload["events"][0]["properties"]["failure_type"],
        "invalid_archive"
    );
}
