use super::*;
use std::collections::VecDeque;

fn running_status(request_id: &str) -> Value {
    compact_json(json!({
        "ok": true,
        "schema_version": 1,
        "owner": "daemon",
        "request_id": request_id,
        "request_state": "running",
    }))
}

#[test]
fn transient_status_timeout_recovers_the_same_durable_request() {
    let request_id = "019fcaaa-0000-7000-8000-000000000301";
    let expected = running_status(request_id);
    let mut responses = VecDeque::from([
        Err(anyhow!("daemon query response read timed out")),
        Ok(Some(expected.clone())),
    ]);
    let mut observed_backoffs = Vec::new();
    let mut observed_request_ids = Vec::new();

    let recovered = request_bound_status_with_recovery(
        request_id,
        |backoff| observed_backoffs.push(backoff),
        || {
            observed_request_ids.push(request_id);
            responses.pop_front().expect("bounded status recovery")
        },
    )
    .unwrap()
    .unwrap();

    assert_eq!(observed_backoffs, [StdDuration::from_millis(25)]);
    assert_eq!(observed_request_ids, [request_id, request_id]);
    assert_eq!(recovered, expected);
}

#[test]
fn cancellation_before_status_io_performs_no_roundtrip() {
    let mut roundtrips = 0;
    let error = request_bound_status_with_outage_budget_cancellable(
        "cancel-before-status-io",
        |_| panic!("pre-I/O cancellation must not sleep"),
        StdInstant::now,
        || Err(anyhow!("cancelled before status I/O")),
        || {
            roundtrips += 1;
            Ok(None)
        },
    )
    .unwrap_err();

    assert_eq!(error.to_string(), "cancelled before status I/O");
    assert_eq!(roundtrips, 0);
}

#[test]
fn cancellation_during_status_retry_backoff_prevents_another_roundtrip() {
    let mut roundtrips = 0;
    let error = request_bound_status_with_recovery_cancellable(
        "cancel-status-backoff",
        |backoff| {
            assert_eq!(backoff, StdDuration::from_millis(25));
            Err(anyhow!("cancelled during status backoff"))
        },
        || Ok(()),
        || {
            roundtrips += 1;
            Err(anyhow!("status transport unavailable"))
        },
    )
    .unwrap_err();

    assert_eq!(error.to_string(), "cancelled during status backoff");
    assert_eq!(roundtrips, 1);
}

#[test]
fn cancellation_between_outage_bursts_stops_before_the_next_burst() {
    let request_id = "cancel-between-outage-bursts";
    let started = StdInstant::now();
    let mut times = VecDeque::from([started, started + StdDuration::from_secs(1)]);
    let mut roundtrips = 0;
    let mut retry_backoffs = Vec::new();
    let mut pauses = 0;

    let error = request_bound_status_with_outage_budget_cancellable(
        request_id,
        |backoff| {
            pauses += 1;
            if pauses == 4 {
                assert_eq!(backoff, SOURCE_REFRESH_POLL_INTERVAL);
                return Err(anyhow!("cancelled between outage bursts"));
            }
            retry_backoffs.push(backoff);
            Ok(())
        },
        || times.pop_front().expect("bounded outage clock"),
        || Ok(()),
        || {
            roundtrips += 1;
            Err(anyhow!("status transport unavailable"))
        },
    )
    .unwrap_err();

    assert_eq!(error.to_string(), "cancelled between outage bursts");
    assert_eq!(roundtrips, 4);
    assert_eq!(
        retry_backoffs,
        [
            StdDuration::from_millis(25),
            StdDuration::from_millis(50),
            StdDuration::from_millis(100),
        ]
    );
    assert!(times.is_empty());
}

#[test]
fn one_status_outage_burst_is_typed_and_bounded() {
    let request_id = "019fcaaa-0000-7000-8000-000000000302";
    let error = request_bound_status_with_recovery(
        request_id,
        |_| {},
        || Err(anyhow!("daemon query response read timed out")),
    )
    .unwrap_err();

    let recovery = error
        .downcast_ref::<SourceRefreshObservationRecoveryFailed>()
        .expect("typed request-bound observation outcome");
    assert_eq!(recovery.request_id, request_id);
    assert_eq!(
        recovery.recovery_attempts,
        REQUEST_BOUND_STATUS_RECOVERY_ATTEMPT_LIMIT
    );
    assert_eq!(recovery.disconnect_policy, DISCONNECT_POLICY);
    assert!(error.to_string().contains("durably admitted request"));
    assert!(error.to_string().contains("outcome is unknown"));
    assert!(!error.to_string().contains("timed out"));
}

#[test]
fn temporary_continuous_outage_reobserves_the_same_request() {
    let request_id = "019fcaaa-0000-7000-8000-000000000304";
    let expected = running_status(request_id);
    let mut responses = VecDeque::from([
        Err(anyhow!("daemon query response read timed out")),
        Err(anyhow!("daemon query response read timed out")),
        Err(anyhow!("daemon query response read timed out")),
        Err(anyhow!("daemon query response read timed out")),
        Ok(Some(expected.clone())),
    ]);
    let mut observed_backoffs = Vec::new();
    let mut observed_request_ids = Vec::new();
    let started = StdInstant::now();
    let mut times = VecDeque::from([
        started,
        started + StdDuration::from_secs(8),
        started + StdDuration::from_secs(9),
    ]);

    let recovered = request_bound_status_with_outage_budget(
        request_id,
        |backoff| observed_backoffs.push(backoff),
        || times.pop_front().expect("bounded observation clock"),
        || {
            observed_request_ids.push(request_id);
            responses.pop_front().expect("continued status observation")
        },
    )
    .unwrap()
    .unwrap();

    assert_eq!(
        observed_backoffs,
        [
            StdDuration::from_millis(25),
            StdDuration::from_millis(50),
            StdDuration::from_millis(100),
            SOURCE_REFRESH_POLL_INTERVAL,
        ]
    );
    assert_eq!(observed_request_ids, [request_id; 5]);
    assert_eq!(recovered, expected);
    assert!(times.is_empty());
}

#[test]
fn permanent_continuous_outage_returns_typed_error_at_its_budget() {
    let request_id = "019fcaaa-0000-7000-8000-000000000305";
    let mut observed_backoffs = Vec::new();
    let mut observed_request_ids = Vec::new();
    let started = StdInstant::now();
    let mut times = VecDeque::from([
        started,
        started + StdDuration::from_secs(8),
        started + StdDuration::from_secs(9),
        started + StdDuration::from_secs(17),
        started + StdDuration::from_secs(18),
        started + StdDuration::from_secs(26),
        started + StdDuration::from_secs(27),
        started + StdDuration::from_secs(35),
    ]);

    let error = request_bound_status_with_outage_budget(
        request_id,
        |backoff| observed_backoffs.push(backoff),
        || times.pop_front().expect("bounded observation clock"),
        || {
            observed_request_ids.push(request_id);
            Err(anyhow!("daemon query response read timed out"))
        },
    )
    .unwrap_err();

    let retained = error
        .downcast_ref::<SourceRefreshObservationRecoveryFailed>()
        .expect("continuous outage remains a typed retained request");
    assert_eq!(retained.request_id, request_id);
    assert_eq!(observed_request_ids, [request_id; 16]);
    assert_eq!(
        observed_backoffs,
        [
            StdDuration::from_millis(25),
            StdDuration::from_millis(50),
            StdDuration::from_millis(100),
            SOURCE_REFRESH_POLL_INTERVAL,
            StdDuration::from_millis(25),
            StdDuration::from_millis(50),
            StdDuration::from_millis(100),
            SOURCE_REFRESH_POLL_INTERVAL,
            StdDuration::from_millis(25),
            StdDuration::from_millis(50),
            StdDuration::from_millis(100),
            SOURCE_REFRESH_POLL_INTERVAL,
            StdDuration::from_millis(25),
            StdDuration::from_millis(50),
            StdDuration::from_millis(100),
        ]
    );
    assert!(times.is_empty());
}

#[test]
fn typed_service_unavailability_still_enters_daemon_recovery_immediately() {
    let request_id = "019fcaaa-0000-7000-8000-000000000303";
    let mut roundtrips = 0;
    let error = request_bound_status_with_outage_budget(
        request_id,
        |_| panic!("typed unavailability must not use transport retry backoff"),
        StdInstant::now,
        || {
            roundtrips += 1;
            Err(DaemonSourceRefreshServiceUnavailable.into())
        },
    )
    .unwrap_err();

    assert_eq!(roundtrips, 1);
    assert!(error
        .downcast_ref::<DaemonSourceRefreshServiceUnavailable>()
        .is_some());
}
