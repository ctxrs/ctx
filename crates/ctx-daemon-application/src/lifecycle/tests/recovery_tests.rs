use std::cell::RefCell;

use super::*;

#[test]
fn recovery_never_terminates_live_owner_with_fresh_heartbeat_or_pending_startup() -> Result<()> {
    let owner = test_daemon_owner("healthy-loaded-owner", 37_687);
    let expected = test_config();
    let now_ms = 200_000;

    // Fresh heartbeat preserves owner.
    let fresh = running_status(&owner, &expected, now_ms - 5_000);
    assert!(owner_has_fresh_heartbeat_or_is_starting(
        Some(&fresh),
        &owner,
        now_ms
    ));
    let terminated = recover_unusable_daemon_owner_guarded(
        &owner,
        |_| Ok(false),
        || Ok(Some(owner.clone())),
        || owner_has_fresh_heartbeat_or_is_starting(Some(&fresh), &owner, now_ms),
        |_| panic!("must not terminate an owner with a fresh heartbeat"),
        || Ok(()),
    )?;
    assert!(!terminated);

    // Pending config reload preserves owner even if heartbeat aged during long Core recovery.
    let mut pending = running_status(
        &owner,
        &expected,
        now_ms - DAEMON_SETUP_HANDOFF_MAX_HEARTBEAT_AGE_MS - 10_000,
    );
    pending["config_reload"]["status"] = json!("pending");
    assert!(owner_has_fresh_heartbeat_or_is_starting(
        Some(&pending),
        &owner,
        now_ms
    ));

    // Newly started owner without status.json yet is preserved.
    let mut just_started = owner.clone();
    just_started.started_at_ms = now_ms - 1_000;
    assert!(owner_has_fresh_heartbeat_or_is_starting(
        None,
        &just_started,
        now_ms
    ));

    // Stale heartbeat with applied config and old started_at_ms allows termination.
    let stale = running_status(
        &owner,
        &expected,
        now_ms - DAEMON_SETUP_HANDOFF_MAX_HEARTBEAT_AGE_MS - 1,
    );
    assert!(!owner_has_fresh_heartbeat_or_is_starting(
        Some(&stale),
        &owner,
        now_ms
    ));
    let mut terminated_ids = Vec::new();
    let terminated = recover_unusable_daemon_owner_guarded(
        &owner,
        |_| Ok(false),
        || Ok(Some(owner.clone())),
        || owner_has_fresh_heartbeat_or_is_starting(Some(&stale), &owner, now_ms),
        |owner_id| {
            terminated_ids.push(owner_id.to_owned());
            Ok(())
        },
        || Ok(()),
    )?;
    assert!(terminated);
    assert_eq!(terminated_ids, vec![owner.owner_id.clone()]);

    // Explicitly failed status is never treated as healthy/starting.
    let failed = json!({
        "status": "failed",
        "pid": owner.pid,
        "started_at_ms": owner.started_at_ms,
        "heartbeat_at_ms": now_ms,
    });
    assert!(!owner_has_fresh_heartbeat_or_is_starting(
        Some(&failed),
        &owner,
        now_ms
    ));
    Ok(())
}

#[test]
fn recovery_endpoint_probe_retries_transient_unavailable_and_honors_cancellation() -> Result<()> {
    let mut responses = [
        DaemonLifecycleEndpointObservation::Unavailable,
        DaemonLifecycleEndpointObservation::Starting,
    ]
    .into_iter();
    let pauses = Cell::new(0);
    assert!(probe_daemon_lifecycle_endpoint_with_retries(
        3,
        || responses.next().unwrap(),
        || pauses.set(pauses.get() + 1),
        &mut || Ok(()),
    )?);
    assert_eq!(pauses.get(), 1);

    let probes = Cell::new(0);
    let pauses = Cell::new(0);
    assert!(!probe_daemon_lifecycle_endpoint_with_retries(
        3,
        || {
            probes.set(probes.get() + 1);
            DaemonLifecycleEndpointObservation::Unavailable
        },
        || pauses.set(pauses.get() + 1),
        &mut || Ok(()),
    )?);
    assert_eq!(probes.get(), 3);
    assert_eq!(pauses.get(), 2);

    let checkpoints = Cell::new(0);
    let events = RefCell::new(Vec::new());
    let error = probe_daemon_lifecycle_endpoint_with_retries(
        3,
        || {
            events.borrow_mut().push("probe");
            DaemonLifecycleEndpointObservation::Unavailable
        },
        || events.borrow_mut().push("pause"),
        &mut || {
            checkpoints.set(checkpoints.get() + 1);
            if checkpoints.get() == 2 {
                Err(anyhow!("cancelled between retries"))
            } else {
                Ok(())
            }
        },
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "cancelled between retries");
    assert_eq!(events.borrow().as_slice(), &["probe"]);
    Ok(())
}

#[test]
fn handoff_health_probe_timeout_adapts_within_remaining_stall_budget() {
    assert_eq!(
        handoff_health_probe_timeout(Duration::from_secs(5), 0),
        DAEMON_HEALTH_TIMEOUT
    );
    assert_eq!(
        handoff_health_probe_timeout(Duration::from_secs(5), 1),
        Duration::from_millis(1_500)
    );
    assert_eq!(
        handoff_health_probe_timeout(Duration::from_secs(5), 2),
        DAEMON_RECOVERY_HEALTH_TIMEOUT
    );
    assert_eq!(
        handoff_health_probe_timeout(Duration::from_millis(300), 2),
        Duration::from_millis(300)
    );
    assert_eq!(
        handoff_health_probe_timeout(Duration::ZERO, 2),
        owner_observation::DAEMON_HANDOFF_MIN_HEALTH_PROBE_TIMEOUT
    );
    assert!(handoff_health_probe_timeout(Duration::from_millis(1), 0) > Duration::ZERO);
    assert_eq!(
        handoff_poll_pause(Duration::from_secs(5)),
        DAEMON_UPGRADE_POLL_INTERVAL
    );
    assert_eq!(
        handoff_poll_pause(Duration::ZERO),
        DAEMON_UPGRADE_POLL_INTERVAL
    );
    assert!(handoff_poll_pause(Duration::from_millis(1)) > Duration::ZERO);
}
