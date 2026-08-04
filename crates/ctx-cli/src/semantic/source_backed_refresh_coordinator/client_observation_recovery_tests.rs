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
fn exhausted_status_recovery_reports_retained_request_not_transport_failure() {
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
    assert!(error
        .to_string()
        .contains("continues under daemon ownership"));
    assert!(!error.to_string().contains("timed out"));
}

#[test]
fn typed_service_unavailability_still_enters_daemon_recovery_immediately() {
    let request_id = "019fcaaa-0000-7000-8000-000000000303";
    let mut roundtrips = 0;
    let error = request_bound_status_with_recovery(
        request_id,
        |_| panic!("typed unavailability must not use transport retry backoff"),
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
