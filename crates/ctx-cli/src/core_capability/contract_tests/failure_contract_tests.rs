use super::*;

#[test]
fn source_unclaimed_failure_writes_the_singleton_and_mixed_contracts() {
    for retryable in [false, true] {
        let (status, output) = run_terminal_failure(source_unclaimed_terminal_failure(retryable));
        assert_eq!(status, ExitCode::FAILURE);
        let response: Value = serde_json::from_slice(output.strip_suffix(b"\n").unwrap()).unwrap();
        assert_eq!(response["error_code"], "source_unclaimed");
        assert_eq!(response["details"]["class"], "coverage");
        assert_eq!(response["retryable"], retryable);
        assert_eq!(
            response["details"]["retry_advice"],
            if retryable {
                json!("retry_retryable_routes_and_inspect_blocked")
            } else {
                json!("inspect_sources")
            }
        );
        assert_eq!(
            response["details"]["retryable_routes"]
                .as_array()
                .unwrap()
                .len(),
            usize::from(retryable)
        );
        assert_eq!(
            response["details"]["blocked_routes"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}

#[test]
fn maximum_valid_failure_frame_writes_and_route_cap_fails_closed() {
    let (status, output) = run_terminal_failure(terminal_failure_with_blocked_routes(
        failure::MAX_FAILURE_ROUTES,
    ));
    assert_eq!(status, ExitCode::FAILURE);
    assert_eq!(output.last(), Some(&b'\n'));
    assert_eq!(output.iter().filter(|byte| **byte == b'\n').count(), 1);
    let frame = &output[..output.len() - 1];
    assert!(frame.len() <= MAX_RESPONSE_BYTES);
    let response: Value = serde_json::from_slice(frame).unwrap();
    assert_eq!(
        response["details"]["affected_routes"]
            .as_array()
            .unwrap()
            .len(),
        failure::MAX_FAILURE_ROUTES
    );
    assert_eq!(
        response["details"]["blocked_routes"]
            .as_array()
            .unwrap()
            .len(),
        failure::MAX_FAILURE_ROUTES
    );

    let (status, output) = run_terminal_failure(terminal_failure_with_blocked_routes(
        failure::MAX_FAILURE_ROUTES + 1,
    ));
    assert_eq!(status, ExitCode::FAILURE);
    assert!(output.is_empty());
}
