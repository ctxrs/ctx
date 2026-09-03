//! Progress and publication-authority coverage for the refresh client.

use std::collections::BTreeSet;

use super::*;
use ctx_history_core::{
    CertifiedSource, ScannedSourceCounts, SourceAnchor, SourceKey, SourceObservation,
};
use ctx_history_index::{
    GenerationWriter, SourceRouteIdentity, SourceRouteSnapshot, VerifiedIndex, WriterOptions,
};
use ctx_history_refresh::{RefreshOutcomeCode, RefreshRetryAdvice};

fn source_count_route(byte: u8) -> SourceRouteIdentity {
    SourceRouteIdentity::from_sha256(format!("{byte:02x}").repeat(32)).unwrap()
}

fn verified_source_count_routes(route_bytes: &[u8]) -> (tempfile::TempDir, VerifiedIndex) {
    let temp = tempfile::tempdir().unwrap();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let mut routes = Vec::new();
    for byte in route_bytes {
        let route = source_count_route(*byte);
        let source = SourceKey::derive(
            "codex",
            "codex_session_jsonl",
            "session",
            1,
            SourceAnchor::CatalogLineage([*byte; 32]),
        )
        .unwrap();
        let observation =
            SourceObservation::new(source.clone(), "source-count-test-v1", vec![*byte]).unwrap();
        writer.begin_source(source.clone()).unwrap();
        writer
            .certify_source(
                CertifiedSource::certify(
                    observation.clone(),
                    observation,
                    "source-count-test-v1",
                    [*byte; 32],
                    ScannedSourceCounts::default(),
                )
                .unwrap(),
            )
            .unwrap();
        routes.push(SourceRouteSnapshot::present(route, vec![source]).unwrap());
    }
    writer.set_present_source_routes(routes).unwrap();
    writer.commit(|_| true).unwrap();
    let verified = VerifiedIndex::open_pinned(temp.path()).unwrap();
    (temp, verified)
}

fn typed_terminal_status() -> Value {
    let request_id = Uuid::from_u128(0x294_0100).to_string();
    let physical_attempt_id = Uuid::from_u128(0x294_0101).to_string();
    let retryable_route = "a1".repeat(32);
    let blocked_route = "a2".repeat(32);
    json!({
        "ok": true,
        "schema_version": 1,
        "owner": "daemon",
        "request_id": request_id,
        "request_state": "failed",
        "logical_request_id": request_id,
        "logical_phase": "terminal",
        "physical_attempt_id": physical_attempt_id,
        "physical_attempt_state": "failed",
        "progress_owner_request_id": physical_attempt_id,
        "progress_owner_attempt_state": "failed",
        "structured_outcome": {
            "code": "source_failures",
            "class": "mixed",
            "retryable": true,
            "affected_routes": [retryable_route.clone(), blocked_route.clone()],
            "retryable_routes": [retryable_route],
            "blocked_routes": [blocked_route],
            "physical_attempt_id": physical_attempt_id,
            "retained_generation": "b1".repeat(32),
            "retry_advice": "retry_affected_routes",
            "detail": "typed mixed route outcome",
        },
    })
}

fn set_terminal_state(response: &mut Value, state: &str) {
    response["request_state"] = json!(state);
    response["physical_attempt_state"] = json!(state);
    response["progress_owner_attempt_state"] = json!(state);
}

fn typed_published_status() -> Value {
    let generation = "c1".repeat(32);
    let mut response = typed_terminal_status();
    set_terminal_state(&mut response, "published");
    response["published_generation"] = json!(generation);
    response["structured_outcome"] = json!({
        "code": "completed",
        "class": "completed",
        "retryable": false,
        "affected_routes": [],
        "retryable_routes": [],
        "blocked_routes": [],
        "physical_attempt_id": Uuid::from_u128(0x294_0101).to_string(),
        "published_generation": generation,
    });
    response
}

fn assert_protocol_error(response: &Value, expected: &str) {
    let error = source_refresh_protocol_status(response).unwrap_err();
    assert_eq!(error.root_cause().to_string(), expected);
}

#[test]
fn live_progress_heartbeat_is_ten_hertz() {
    assert_eq!(
        SOURCE_REFRESH_PROGRESS_HEARTBEAT,
        StdDuration::from_millis(100)
    );
}

#[test]
fn published_source_count_uses_request_routes_not_global_or_diagnostic_counts() {
    let (_temp, verified) = verified_source_count_routes(&[1, 2, 3, 4]);
    for (name, scanned_routes, unsupported_routes, route_results, global_sources, expected) in [
        ("unsupported only", 0, 1, vec![], 4, 0),
        (
            "mixed executable and unsupported",
            1,
            1,
            vec![SourceBackedRefreshRouteResult::succeeded(
                source_count_route(1).as_str().to_owned(),
                false,
            )],
            4,
            1,
        ),
        (
            "covered executable route",
            0,
            3,
            vec![SourceBackedRefreshRouteResult::succeeded(
                source_count_route(2).as_str().to_owned(),
                false,
            )],
            4,
            1,
        ),
        (
            "failed carried source remains global only",
            1,
            3,
            vec![SourceBackedRefreshRouteResult::failed(
                source_count_route(2).as_str().to_owned(),
                "unavailable".to_owned(),
                true,
            )],
            4,
            0,
        ),
        (
            "global publication contains unrelated sources",
            38,
            37,
            vec![
                SourceBackedRefreshRouteResult::succeeded(
                    source_count_route(3).as_str().to_owned(),
                    true,
                ),
                SourceBackedRefreshRouteResult::failed(
                    source_count_route(30).as_str().to_owned(),
                    "unavailable".to_owned(),
                    false,
                ),
            ],
            4,
            1,
        ),
    ] {
        let receipt = SourceBackedRefreshReceipt {
            previous_generation: None,
            published_generation: verified.generation_id().to_owned(),
            generation_changed: true,
            published_explicit_source_catalog: None,
            current: SourceBackedRefreshCurrent {
                source_count: global_sources,
                ..SourceBackedRefreshCurrent::default()
            },
            route_results,
            zero_source_authority: Vec::new(),
            catalog_route_bindings: Vec::new(),
        };
        let response = json!({
            "scanned_routes": scanned_routes,
            "unsupported_routes": unsupported_routes,
        });
        assert_eq!(
            published_source_count(&response, &receipt, &verified).unwrap(),
            expected,
            "{name}"
        );
    }

    let receipt = SourceBackedRefreshReceipt {
        previous_generation: None,
        published_generation: verified.generation_id().to_owned(),
        generation_changed: true,
        published_explicit_source_catalog: None,
        current: SourceBackedRefreshCurrent::default(),
        route_results: vec![
            SourceBackedRefreshRouteResult::succeeded(
                source_count_route(4).as_str().to_owned(),
                false,
            ),
            SourceBackedRefreshRouteResult::failed(
                source_count_route(5).as_str().to_owned(),
                "incompatible".to_owned(),
                false,
            ),
        ],
        zero_source_authority: Vec::new(),
        catalog_route_bindings: Vec::new(),
    };
    assert_eq!(
        published_source_count(
            &json!({"scanned_routes": 2, "unsupported_routes": 1}),
            &receipt,
            &verified,
        )
        .unwrap(),
        1,
        "an exact incompatible route outcome is not a published source route"
    );
}

#[test]
fn identical_poll_is_suppressed_until_heartbeat_or_terminal_state() {
    let status = RefreshStatus::parse_schema_v1(json!({
        "request_id": "request",
        "request_state": "running",
        "progress": {
            "phase": "refreshing",
            "completed_sources": 0,
            "total_sources": 0,
            "total_sources_known": false
        }
    }))
    .unwrap();
    let now = StdInstant::now();
    assert!(!should_report_progress(
        Some(&status),
        Some(now),
        &status,
        RefreshRequestState::Running,
        now,
    ));
    assert!(should_report_progress(
        Some(&status),
        Some(now),
        &status,
        RefreshRequestState::Running,
        now + SOURCE_REFRESH_PROGRESS_HEARTBEAT,
    ));
    assert!(should_report_progress(
        Some(&status),
        Some(now),
        &status,
        RefreshRequestState::Published,
        now,
    ));
}

#[test]
fn logical_transition_with_unchanged_counters_is_reported() {
    let status = |logical_phase: &str| {
        RefreshStatus::parse_schema_v1(json!({
            "request_id": "logical-request",
            "request_state": "running",
            "logical_request_id": "logical-request",
            "logical_phase": logical_phase,
            "physical_attempt_id": "physical-attempt",
            "physical_attempt_state": "running",
            "progress_owner_request_id": "physical-attempt",
            "progress_owner_attempt_state": "running",
            "progress": {
                "phase": "refreshing",
                "completed_sources": 1,
                "total_sources": 2,
                "total_sources_known": true
            }
        }))
        .unwrap()
    };
    let attached = status("attached");
    let coverage = status("coverage_check");
    let now = StdInstant::now();
    assert!(should_report_progress(
        Some(&attached),
        Some(now),
        &coverage,
        RefreshRequestState::Running,
        now,
    ));
}

#[test]
fn structured_terminal_error_preserves_engine_route_dispositions() {
    let response = typed_terminal_status();
    let protocol = source_refresh_protocol_status(&response).unwrap();
    assert_eq!(protocol.request_state(), RefreshRequestState::Failed);
    let error = match failed_refresh_response(&response, protocol.into_terminal_outcome()) {
        Ok(_) => panic!("failed status must return a terminal error"),
        Err(error) => error,
    };
    let terminal = error
        .downcast_ref::<SourceBackedRefreshTerminalError>()
        .expect("typed terminal error");
    let outcome = terminal.outcome();

    assert_eq!(outcome.code(), RefreshOutcomeCode::SourceFailures);
    assert_eq!(outcome.class(), RefreshOutcomeClass::Mixed);
    assert!(outcome.retryable());
    assert_eq!(outcome.affected_routes().len(), 2);
    assert_eq!(
        outcome.retryable_routes(),
        &BTreeSet::from([SourceRouteIdentity::from_sha256("a1".repeat(32)).unwrap()])
    );
    assert_eq!(
        outcome.blocked_routes(),
        &BTreeSet::from([SourceRouteIdentity::from_sha256("a2".repeat(32)).unwrap()])
    );
    assert_eq!(
        outcome.physical_attempt_id(),
        Uuid::from_u128(0x294_0101).to_string()
    );
    assert_eq!(
        outcome.retained_generation(),
        Some("b1".repeat(32).as_str())
    );
    assert_eq!(
        outcome.retry_advice(),
        Some(RefreshRetryAdvice::RetryAffectedRoutes)
    );
    assert_eq!(
        terminal.to_string(),
        format!(
            "daemon-owned source-backed refresh failed (code=source_failures, class=mixed, retryable=true, attempt={}, affected_routes={:?}, retryable_routes={:?}, blocked_routes={:?}, retained_generation={:?}, retry_advice={:?}): typed mixed route outcome",
            Uuid::from_u128(0x294_0101),
            vec!["a1".repeat(32), "a2".repeat(32)],
            vec!["a1".repeat(32)],
            vec!["a2".repeat(32)],
            Some("b1".repeat(32)),
            Some("retry_affected_routes"),
        )
    );
}

#[test]
fn explicit_path_disappearance_code_survives_the_daemon_boundary() {
    let mut response = typed_terminal_status();
    let affected = response["structured_outcome"]["affected_routes"].clone();
    response["structured_outcome"]["code"] = json!("explicit_source_path_missing");
    response["structured_outcome"]["class"] = json!("unavailable");
    response["structured_outcome"]["retryable_routes"] = affected;
    response["structured_outcome"]["blocked_routes"] = json!([]);
    response["structured_outcome"]["retry_advice"] = json!("inspect_sources");
    let protocol = source_refresh_protocol_status(&response).unwrap();
    let error = match failed_refresh_response(&response, protocol.into_terminal_outcome()) {
        Ok(_) => panic!("failed status must return a terminal error"),
        Err(error) => error,
    };
    let terminal = error
        .downcast_ref::<SourceBackedRefreshTerminalError>()
        .expect("typed terminal error");
    let outcome = terminal.outcome();

    assert_eq!(
        outcome.code(),
        RefreshOutcomeCode::ExplicitSourcePathMissing
    );
    assert_eq!(outcome.class(), RefreshOutcomeClass::Unavailable);
    assert!(outcome.retryable());
    assert_eq!(
        outcome.retry_advice(),
        Some(RefreshRetryAdvice::InspectSources)
    );
}

#[test]
fn source_unclaimed_terminal_error_preserves_the_culprit_and_retryable_peer() {
    let mut response = typed_terminal_status();
    response["structured_outcome"]["code"] = json!("source_unclaimed");
    response["structured_outcome"]["class"] = json!("coverage");
    response["structured_outcome"]["retry_advice"] =
        json!("retry_retryable_routes_and_inspect_blocked");
    let protocol = source_refresh_protocol_status(&response).unwrap();
    let error = match failed_refresh_response(&response, protocol.into_terminal_outcome()) {
        Ok(_) => panic!("failed status must return a terminal error"),
        Err(error) => error,
    };
    let terminal = error
        .downcast_ref::<SourceBackedRefreshTerminalError>()
        .expect("typed terminal error");
    let outcome = terminal.outcome();

    assert_eq!(outcome.code(), RefreshOutcomeCode::SourceUnclaimed);
    assert_eq!(outcome.class(), RefreshOutcomeClass::Coverage);
    assert!(outcome.retryable());
    assert_eq!(
        outcome.retryable_routes(),
        &BTreeSet::from([SourceRouteIdentity::from_sha256("a1".repeat(32)).unwrap()])
    );
    assert_eq!(
        outcome.blocked_routes(),
        &BTreeSet::from([SourceRouteIdentity::from_sha256("a2".repeat(32)).unwrap()])
    );
    assert_eq!(
        outcome.retry_advice(),
        Some(RefreshRetryAdvice::RetryRetryableRoutesAndInspectBlocked)
    );
}

#[test]
fn present_structured_fields_are_strictly_validated() {
    let mut unknown = typed_terminal_status();
    unknown["structured_outcome"]["code"] = json!("invented_code");
    assert!(format!(
        "{:#}",
        source_refresh_protocol_status(&unknown).unwrap_err()
    )
    .contains("unknown code"));

    let mut overlap = typed_terminal_status();
    overlap["structured_outcome"]["blocked_routes"] = json!(["a1".repeat(32)]);
    assert!(format!(
        "{:#}",
        source_refresh_protocol_status(&overlap).unwrap_err()
    )
    .contains("inconsistent route dispositions"));

    let mut wrong_class = typed_terminal_status();
    wrong_class["structured_outcome"]["class"] = json!("internal");
    assert!(format!(
        "{:#}",
        source_refresh_protocol_status(&wrong_class).unwrap_err()
    )
    .contains("inconsistent code and class"));

    let mut failed_with_publication = typed_terminal_status();
    failed_with_publication["structured_outcome"]["published_generation"] = json!("c1".repeat(32));
    assert!(format!(
        "{:#}",
        source_refresh_protocol_status(&failed_with_publication).unwrap_err()
    )
    .contains("failed source refresh outcome has a published generation"));

    let mut partial = typed_terminal_status();
    partial.as_object_mut().unwrap().remove("logical_phase");
    assert!(format!(
        "{:#}",
        source_refresh_protocol_status(&partial).unwrap_err()
    )
    .contains("partial typed logical status"));
}

#[test]
fn published_status_rejects_a_failure_outcome() {
    let mut response = typed_terminal_status();
    set_terminal_state(&mut response, "published");
    response["published_generation"] = json!("c1".repeat(32));

    assert_protocol_error(&response, "published source refresh has a failure outcome");
}

#[test]
fn failed_status_rejects_a_nonfailure_outcome() {
    let mut response = typed_published_status();
    set_terminal_state(&mut response, "failed");

    assert_protocol_error(&response, "failed source refresh has a nonfailure outcome");
}

#[test]
fn published_status_requires_matching_outer_and_outcome_generations() {
    let mut response = typed_published_status();
    response["published_generation"] = json!("d1".repeat(32));

    assert_protocol_error(
        &response,
        "published source refresh generation disagrees with its outcome",
    );
}

#[test]
fn attached_logical_phase_remains_active_until_engine_terminalizes_it() {
    let request_id = Uuid::from_u128(0x294_0200).to_string();
    let physical_attempt_id = Uuid::from_u128(0x294_0201).to_string();
    let response = json!({
        "request_id": request_id,
        "request_state": "queued",
        "logical_request_id": request_id,
        "logical_phase": "attached",
        "physical_attempt_id": physical_attempt_id,
        "physical_attempt_state": "running",
        "progress_owner_request_id": physical_attempt_id,
        "progress_owner_attempt_state": "running",
    });

    let protocol = source_refresh_protocol_status(&response).unwrap();
    assert_eq!(protocol.request_state(), RefreshRequestState::Queued);
    assert!(matches!(
        protocol,
        RefreshStatusKind::Logical(ref status)
            if status.logical_phase == RefreshLogicalPhase::Attached
                && status.structured_outcome.is_none()
    ));
}

#[test]
fn legacy_terminal_record_uses_explicit_failure_type_fallback() {
    let response = json!({
        "request_id": Uuid::from_u128(0x294_0300).to_string(),
        "request_state": "failed",
        "failure_type": "unsupported_schema",
        "last_error": "legacy incompatible source",
        "previous_generation": "c1".repeat(32),
    });
    let protocol = source_refresh_protocol_status(&response).unwrap();
    assert!(matches!(
        protocol,
        RefreshStatusKind::Legacy {
            request_state: RefreshRequestState::Failed
        }
    ));
    let error = match failed_refresh_response(&response, None) {
        Ok(_) => panic!("legacy failed status must return an error"),
        Err(error) => error,
    };
    assert!(error
        .chain()
        .any(|cause| cause.downcast_ref::<CaptureError>().is_some()));
}
