use super::*;
use std::cell::Cell;

fn recovery(allow_daemon_autostart: bool) -> WaitRefreshRecovery {
    let root = tempfile::tempdir().unwrap();
    WaitRefreshRecovery::new(
        root.path(),
        &RefreshRequest::automatic("original-request".to_owned(), RefreshRequestTrigger::Search),
        allow_daemon_autostart,
    )
    .unwrap()
}

fn running_status() -> Value {
    json!({"ok":true,"schema_version":1,"owner":"daemon",
        "request_id":"original-request","request_state":"running"})
}

fn forgotten_status() -> Value {
    json!({"ok":false,"schema_version":1,"owner":"daemon",
        "request_id":"original-request","request_state":"request_unknown",
        "error_code":"source_refresh_request_unknown",
        "reason":"request_not_retained_after_restart","retryable":false})
}

fn lost_response() -> anyhow::Error {
    std::io::Error::new(std::io::ErrorKind::ConnectionReset, "lost response").into()
}

#[test]
fn live_owner_admission_and_status_outages_survive_retry_and_time_limits() {
    for admission in [true, false] {
        let mut recovery = recovery(true);
        let elapsed = Cell::new(0);
        let start = StdInstant::now();
        let mut requests = Vec::new();
        let mut pauses = Vec::new();
        let response = recovery
            .recover_response(
                admission,
                |pause| {
                    pauses.push(pause);
                    elapsed.set(elapsed.get() + 31);
                    Ok(())
                },
                || start + StdDuration::from_secs(elapsed.get()),
                || Ok(()),
                |request| {
                    requests.push(request.clone());
                    if requests.len() <= 6 {
                        Err(lost_response())
                    } else {
                        Ok(Some(running_status()))
                    }
                },
                |restore| {
                    assert!(!restore);
                    Ok(RefreshOwnerObservation::Live)
                },
            )
            .unwrap();
        assert_eq!(response, running_status());
        assert!(elapsed.get() > 30);
        assert_eq!(requests.len(), 7);
        assert!(requests.windows(2).all(|pair| pair[0] == pair[1]));
        assert_eq!(
            pauses,
            [25, 50, 100, 100, 100, 100].map(StdDuration::from_millis)
        );
        assert!(!recovery.owner_restored && !recovery.forgotten_replayed);
    }
}

#[test]
fn uncertain_owner_remains_bounded_without_replay_or_restoration() {
    let mut recovery = recovery(true);
    let elapsed = Cell::new(0);
    let start = StdInstant::now();
    let mut calls = 0;
    let error = recovery
        .recover_response(
            true,
            |_| {
                elapsed.set(elapsed.get() + 15);
                Ok(())
            },
            || start + StdDuration::from_secs(elapsed.get()),
            || Ok(()),
            |_| {
                calls += 1;
                Err(lost_response())
            },
            |restore| {
                assert!(!restore);
                Ok(RefreshOwnerObservation::Unknown)
            },
        )
        .unwrap_err();
    assert_eq!(calls, 3);
    let typed = error
        .downcast_ref::<SourceRefreshObservationRecoveryFailed>()
        .unwrap();
    assert_eq!(typed.request_id, "original-request");
    assert!(!recovery.owner_restored && !recovery.forgotten_replayed);
}

#[test]
fn restoration_queries_original_before_replay_and_is_shared_across_ack() {
    let mut recovery = recovery(true);
    let mut requests = Vec::new();
    let mut restorations = 0;
    recovery
        .recover_response(
            true,
            |_| Ok(()),
            StdInstant::now,
            || Ok(()),
            |request| {
                requests.push(request.clone());
                match requests.len() {
                    1 => Err(lost_response()),
                    2 => Ok(Some(forgotten_status())),
                    3 => Ok(Some(running_status())),
                    _ => panic!("unexpected replay"),
                }
            },
            |restore| {
                if restore {
                    restorations += 1;
                }
                Ok(RefreshOwnerObservation::Lost)
            },
        )
        .unwrap();
    assert_eq!(restorations, 1);
    assert_eq!(requests[0], requests[2]);
    assert_eq!(requests[1]["op"], SOURCE_REFRESH_STATUS_OP);
    assert!(requests
        .iter()
        .all(|request| request["request_id"] == "original-request"));
    let error = recovery
        .recover_response(
            false,
            |_| panic!("second owner loss must fail"),
            StdInstant::now,
            || Ok(()),
            |_| Err(lost_response()),
            |restore| {
                assert!(!restore);
                Ok(RefreshOwnerObservation::Lost)
            },
        )
        .unwrap_err();
    assert!(error.is::<SourceRefreshObservationRecoveryFailed>());
    assert!(recovery.owner_restored && recovery.forgotten_replayed);
}

#[test]
fn forgotten_replay_allowance_is_shared_between_admission_and_observation() {
    let mut recovery = recovery(false);
    let mut calls = 0;
    recovery
        .recover_response(
            true,
            |_| Ok(()),
            StdInstant::now,
            || Ok(()),
            |_| {
                calls += 1;
                Ok(Some(if calls == 1 {
                    forgotten_status()
                } else {
                    running_status()
                }))
            },
            |_| panic!("responsive owner needs no restoration"),
        )
        .unwrap();
    let error = recovery
        .recover_response(
            false,
            |_| Ok(()),
            StdInstant::now,
            || Ok(()),
            |_| Ok(Some(forgotten_status())),
            |_| panic!("forgotten replay does not restore an owner"),
        )
        .unwrap_err();
    assert!(error.is::<SourceRefreshObservationRecoveryFailed>());
    assert_eq!(calls, 2);
}

#[test]
fn no_start_owner_loss_does_not_restore_or_submit_again() {
    let mut recovery = recovery(false);
    let mut calls = 0;
    let error = recovery
        .recover_response(
            true,
            |_| panic!("confirmed loss must not sleep"),
            StdInstant::now,
            || Ok(()),
            |_| {
                calls += 1;
                Err(lost_response())
            },
            |restore| {
                assert!(!restore);
                Ok(RefreshOwnerObservation::Lost)
            },
        )
        .unwrap_err();
    assert_eq!(calls, 1);
    assert!(error.is::<SourceRefreshObservationRecoveryFailed>());
    assert!(!recovery.owner_restored);
}

#[test]
fn no_start_without_an_owner_preserves_definite_unavailability() {
    let root = tempfile::tempdir().unwrap();
    let mut recovery = WaitRefreshRecovery::new(
        root.path(),
        &RefreshRequest::automatic("never-submitted".to_owned(), RefreshRequestTrigger::Search),
        false,
    )
    .unwrap();
    let error = recovery
        .request(&crate::test_support::AVAILABILITY, root.path(), true)
        .unwrap_err();
    assert!(error.is::<SourceBackedRefreshDaemonUnavailable>());
    assert!(!recovery.owner_restored && !recovery.forgotten_replayed);
}

#[test]
fn cancellation_before_io_during_backoff_and_before_restoration_is_preserved() {
    for boundary in ["before-io", "backoff", "after-io"] {
        let mut recovery = recovery(true);
        let cancelled = Cell::new(boundary == "before-io");
        let mut calls = 0;
        let error = recovery
            .recover_response(
                true,
                |_| Err(anyhow!("cancelled")),
                StdInstant::now,
                || {
                    if cancelled.get() {
                        Err(anyhow!("cancelled"))
                    } else {
                        Ok(())
                    }
                },
                |_| {
                    calls += 1;
                    cancelled.set(boundary == "after-io");
                    Err(lost_response())
                },
                |restore| {
                    assert!(!restore);
                    Ok(RefreshOwnerObservation::Live)
                },
            )
            .unwrap_err();
        assert_eq!(error.to_string(), "cancelled");
        assert_eq!(calls, usize::from(boundary != "before-io"));
        assert!(!recovery.owner_restored);
    }
}

#[test]
fn wrong_id_and_malformed_state_fail_without_retry() {
    for response in [
        json!({"ok":true,"schema_version":1,"owner":"daemon","request_id":"wrong","request_state":"running"}),
        json!({"ok":true,"schema_version":1,"owner":"daemon","request_id":"original-request","request_state":"unknown-state"}),
        json!({"ok":true,"schema_version":2,"owner":"daemon","request_id":"original-request","request_state":"running"}),
    ] {
        let mut recovery = recovery(true);
        let error = recovery
            .recover_response(
                true,
                |_| panic!("protocol error must not retry"),
                StdInstant::now,
                || Ok(()),
                |_| Ok(Some(response.clone())),
                |_| panic!("protocol error must not recover"),
            )
            .unwrap_err();
        assert!(!error.is::<SourceRefreshObservationRecoveryFailed>());
    }
}

#[test]
fn decode_utf8_oversize_and_other_errors_are_not_transient() {
    for error in [
        anyhow::Error::from(serde_json::from_str::<Value>("{").unwrap_err()),
        String::from_utf8(vec![0xff]).unwrap_err().into(),
        ctx_daemon_runtime::DaemonQueryResponseTooLarge::new(1).into(),
        anyhow!("protocol fingerprint conflict"),
    ] {
        assert!(!retryable_refresh_transport_error(&error));
        let mut recovery = recovery(true);
        let mut error = Some(error);
        recovery
            .recover_response(
                true,
                |_| panic!("decode error must not retry"),
                StdInstant::now,
                || Ok(()),
                |_| Err(error.take().unwrap()),
                |_| panic!("decode error must not recover"),
            )
            .unwrap_err();
    }
}

#[cfg(unix)]
fn publish_endpoint(root: &Path) -> Result<()> {
    crate::query_service::write_daemon_service_endpoint(
        root,
        crate::query_service::DaemonIpcService::SourceRefresh,
        &ctx_daemon_runtime::DaemonQueryEndpoint::Unix {
            path: root.join("not-listening.sock"),
            token: "0123456789abcdef0123456789abcdef".to_owned(),
        },
    )
}

#[cfg(unix)]
#[test]
fn cleanup_guard_and_alive_retained_pid_do_not_authenticate_owner() -> Result<()> {
    let root = tempfile::tempdir()?;
    ctx_history_platform::platform_security::establish_private_data_root(root.path())?;
    let path = daemon_lock_path(root.path());
    std::fs::create_dir_all(path.parent().unwrap())?;
    let payload = ctx_daemon_runtime::pid_lock_payload(json!({"owner_id":"retained-owner"}));
    std::fs::write(&path, serde_json::to_vec(&payload)?)?;
    publish_endpoint(root.path())?;
    let _cleanup = ctx_daemon_runtime::DaemonQuiescenceGuard::acquire(root.path())?
        .context("cleanup guard")?;
    assert_eq!(observe_pid_advisory_guard(&path), Some(true));
    assert_eq!(process_state(std::process::id()), ProcessState::Running);
    let (observation, identity) = observe_refresh_owner(root.path(), None, false)?;
    assert_eq!(observation, RefreshOwnerObservation::Unknown);
    assert!(identity.is_some());
    assert_eq!(read_pid_lock_json(&path), Some(payload));
    Ok(())
}

#[cfg(unix)]
#[test]
fn observer_detects_release_replacement_and_metadata_gap_without_false_liveness() -> Result<()> {
    let root = tempfile::tempdir()?;
    let owner = ctx_daemon_runtime::DaemonLock::acquire(root.path())?.context("owner")?;
    publish_endpoint(root.path())?;
    let (observation, identity) = observe_refresh_owner(root.path(), None, false)?;
    assert_eq!(observation, RefreshOwnerObservation::Unknown);
    let identity = identity.context("captured identity")?;
    assert_eq!(
        observe_refresh_owner(root.path(), Some(&identity), true)?.0,
        RefreshOwnerObservation::Live
    );
    assert_eq!(
        observe_refresh_owner_with_process_state(root.path(), Some(&identity), true, |_| {
            ProcessState::Unknown
        })?
        .0,
        RefreshOwnerObservation::Live,
        "exact child proof survives an unknown redundant PID probe"
    );
    assert_eq!(
        observe_refresh_owner_with_process_state(root.path(), Some(&identity), false, |_| {
            ProcessState::Unknown
        })?
        .0,
        RefreshOwnerObservation::Unknown,
        "unknown PID plus generic guard is not live proof"
    );
    let path = daemon_lock_path(root.path());
    let original = std::fs::read(&path)?;
    std::fs::write(&path, b"{")?;
    assert_eq!(
        observe_refresh_owner(root.path(), Some(&identity), true)?.0,
        RefreshOwnerObservation::Unknown
    );
    std::fs::write(&path, original)?;
    drop(owner);
    assert_eq!(
        observe_refresh_owner(root.path(), Some(&identity), true)?.0,
        RefreshOwnerObservation::Lost
    );
    let _replacement =
        ctx_daemon_runtime::DaemonLock::acquire(root.path())?.context("replacement")?;
    assert_eq!(
        observe_refresh_owner(root.path(), Some(&identity), true)?.0,
        RefreshOwnerObservation::Lost
    );
    Ok(())
}
