use super::*;

#[test]
fn source_unclaimed_failure_writes_the_singleton_and_mixed_contracts() {
    for retryable in [false, true] {
        let (status, output) =
            run_terminal_failure(source_unclaimed_terminal_failure(retryable).into());
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
fn paused_failure_raw_wire_matches_progress_and_final_response() {
    for (wire_code, code, wire_class, class) in [
        (
            "source_refresh_failed",
            RefreshOutcomeCode::SourceRefreshFailed,
            "internal",
            RefreshOutcomeClass::Internal,
        ),
        (
            "all_provider_terminal_coverage_unavailable",
            RefreshOutcomeCode::AllProviderTerminalCoverageUnavailable,
            "coverage",
            RefreshOutcomeClass::Coverage,
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let request = json!({
            "data_root": root.path(),
            "operation": "RefreshAndWait",
            "options": {"progress": "events"},
            "protocol_version": CORE_PRO_PROTOCOL_VERSION.get(),
            "schema_version": 1,
        });
        let mut input = canonical(&request).unwrap();
        input.push(b'\n');
        let route_text = "ab".repeat(32);
        let retained = "cd".repeat(32);
        let physical_attempt_id = "01234567-89ab-cdef-0123-456789abcdef";
        let private_detail = format!("private paused detail for {wire_code}");
        let status = crate::semantic::RefreshStatus::parse_schema_v1(json!({
            "request_id": "logical-request",
            "request_state": "failed",
            "logical_request_id": "logical-request",
            "logical_phase": "terminal",
            "physical_attempt_id": physical_attempt_id,
            "physical_attempt_state": "failed",
            "progress_owner_request_id": physical_attempt_id,
            "progress_owner_attempt_state": "failed",
            "progress": {
                "phase": "failed",
                "completed_sources": 0,
                "total_sources": 1,
                "total_sources_known": true,
                "whole_run_stage": "failed"
            },
            "structured_outcome": {
                "code": wire_code,
                "class": wire_class,
                "retryable": false,
                "affected_routes": [route_text],
                "retryable_routes": [],
                "blocked_routes": [route_text],
                "physical_attempt_id": physical_attempt_id,
                "retained_generation": retained,
                "published_generation": null,
                "retry_advice": "inspect_sources",
                "detail": private_detail
            }
        }))
        .unwrap();
        let route = SourceRouteIdentity::from_sha256(route_text).unwrap();
        let terminal: anyhow::Error =
            crate::semantic::SourceBackedRefreshTerminalError::from(RefreshTerminalOutcome {
                code,
                class,
                retryable: false,
                affected_routes: BTreeSet::from([route.clone()]),
                retryable_routes: BTreeSet::new(),
                blocked_routes: BTreeSet::from([route]),
                physical_attempt_id: physical_attempt_id.to_owned(),
                retained_generation: Some(retained),
                published_generation: None,
                retry_advice: Some(RefreshRetryAdvice::InspectSources),
                detail: Some(private_detail.clone()),
            })
            .into();
        let mut output = Vec::new();

        let exit = capability_exit_code(run_with_protocol_io(
            std::io::Cursor::new(input),
            &mut output,
            |_request, events| {
                events.refresh(&status)?;
                Err(terminal)
            },
        ));

        assert_eq!(exit, ExitCode::FAILURE, "{wire_code}");
        let frames = output
            .split(|byte| *byte == b'\n')
            .filter(|frame| !frame.is_empty())
            .map(|frame| serde_json::from_slice::<Value>(frame).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(frames.len(), 2, "{wire_code}");
        assert_eq!(
            frames[0]["refresh"]["terminal_state"]["error_code"],
            wire_code
        );
        assert_eq!(frames[0]["refresh"]["terminal_state"]["retryable"], false);
        assert_eq!(
            frames[0]["refresh"]["terminal_state"]["details"],
            frames[1]["details"]
        );
        assert_eq!(frames[1]["error_code"], wire_code);
        assert_eq!(frames[1]["retryable"], false);
        assert!(!String::from_utf8(output).unwrap().contains(&private_detail));
    }
}

#[test]
fn maximum_valid_failure_frame_writes_and_route_cap_fails_closed() {
    let (status, output) = run_terminal_failure(
        terminal_failure_with_blocked_routes(failure::MAX_FAILURE_ROUTES).into(),
    );
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

    let (status, output) = run_terminal_failure(
        terminal_failure_with_blocked_routes(failure::MAX_FAILURE_ROUTES + 1).into(),
    );
    assert_eq!(status, ExitCode::FAILURE);
    assert!(output.is_empty());
}
