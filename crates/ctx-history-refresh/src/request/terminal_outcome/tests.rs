use super::*;

fn route(byte: &str) -> SourceRouteIdentity {
    SourceRouteIdentity::from_sha256(byte.repeat(32)).unwrap()
}

fn paused(code: RefreshOutcomeCode) -> Result<RefreshTerminalOutcome> {
    RefreshTerminalOutcome::with_uniform_route_disposition(
        code,
        false,
        BTreeSet::from([route("ab")]),
        uuid::Uuid::nil().to_string(),
        None,
        None,
        Some(RefreshRetryAdvice::InspectSources),
        None,
    )
}

#[test]
fn paused_automatic_terminal_outcomes_and_aggregate_schema_advice_are_valid() {
    assert!(paused(RefreshOutcomeCode::SourceRefreshFailed).is_ok());
    assert!(paused(RefreshOutcomeCode::AllProviderTerminalCoverageUnavailable).is_ok());
    for advice in [
        RefreshRetryAdvice::InspectSources,
        RefreshRetryAdvice::UpgradeOrReconfigure,
    ] {
        assert!(RefreshTerminalOutcome::with_uniform_route_disposition(
            RefreshOutcomeCode::UnsupportedSchema,
            false,
            BTreeSet::from([route("ab")]),
            uuid::Uuid::nil().to_string(),
            None,
            None,
            Some(advice),
            None,
        )
        .is_ok());
    }
}

#[test]
fn retry_route_moves_ignore_empty_and_unrelated_sets() {
    let affected = route("ab");
    let unrelated = BTreeSet::from([route("cd")]);
    let mut outcome = RefreshTerminalOutcome::with_uniform_route_disposition(
        RefreshOutcomeCode::SourceRefreshFailed,
        true,
        BTreeSet::from([affected.clone()]),
        uuid::Uuid::nil().to_string(),
        None,
        None,
        Some(RefreshRetryAdvice::RetryAffectedRoutes),
        None,
    )
    .unwrap();

    let retryable = outcome.clone();
    outcome.pause_automatic_retry_routes(&BTreeSet::new());
    outcome.pause_automatic_retry_routes(&unrelated);
    assert_eq!(outcome, retryable);

    outcome.pause_automatic_retry_routes(&BTreeSet::from([affected]));
    let paused = outcome.clone();
    outcome.rearm_automatic_retry_routes(&BTreeSet::new());
    outcome.rearm_automatic_retry_routes(&unrelated);
    assert_eq!(outcome, paused);
}

#[test]
fn completed_outcomes_reject_failure_facts_at_the_boundary() {
    assert!(RefreshTerminalOutcome::new(
        RefreshOutcomeCode::Completed,
        false,
        BTreeSet::from([route("ab")]),
        BTreeSet::new(),
        BTreeSet::from([route("ab")]),
        uuid::Uuid::nil().to_string(),
        None,
        Some("generation".to_owned()),
        Some(RefreshRetryAdvice::InspectSources),
        Some("not a completed outcome".to_owned()),
    )
    .is_err());
}

#[test]
fn successful_outcomes_require_a_published_generation() {
    let error = RefreshTerminalOutcome::with_uniform_route_disposition(
        RefreshOutcomeCode::Completed,
        false,
        BTreeSet::new(),
        uuid::Uuid::nil().to_string(),
        None,
        None,
        None,
        None,
    )
    .unwrap_err();

    assert_eq!(
        error.to_string(),
        "successful source refresh outcome has no published generation"
    );
}

#[test]
fn partial_source_failures_reject_an_affected_route_without_a_disposition() {
    for retryable in [false, true] {
        let assigned = BTreeSet::from([route("ab")]);
        let (retryable_routes, blocked_routes) = if retryable {
            (assigned, BTreeSet::new())
        } else {
            (BTreeSet::new(), assigned)
        };
        let error = RefreshTerminalOutcome::new(
            RefreshOutcomeCode::CompletedWithSourceFailures,
            retryable,
            BTreeSet::from([route("ab"), route("cd")]),
            retryable_routes,
            blocked_routes,
            "attempt".to_owned(),
            Some("generation".to_owned()),
            Some("generation".to_owned()),
            None,
            None,
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "source refresh structured outcome has inconsistent route dispositions"
        );
    }
}

fn published_partial_outcome(
    rejections: bool,
    retryable: bool,
    blocked: bool,
) -> RefreshTerminalOutcome {
    let mut results = Vec::new();
    for (identity, present, retryable) in [("ab", retryable, true), ("cd", blocked, false)] {
        if present {
            let mut result = SourceBackedRefreshRouteResult::succeeded(
                route(identity).as_str().to_owned(),
                true,
            );
            result.source_failure_total = 1;
            result.source_retryable_failure_total = usize::from(retryable);
            results.push(result);
        }
    }
    if rejections {
        let mut result =
            SourceBackedRefreshRouteResult::succeeded(route("ef").as_str().to_owned(), true);
        result.rejected_record_total = 1;
        results.push(result);
    }
    let publication = SourceBackedRefreshPublication {
        generation_id: "generation".to_owned(),
        published_explicit_source_catalog: None,
        unsupported_routes: 0,
        certified_source_count: 0,
        certified_source_bytes: 0,
        current: SourceBackedRefreshCurrent {
            rejected_records: u64::from(rejections),
            sources_with_rejections: usize::from(rejections),
            ..SourceBackedRefreshCurrent::default()
        },
        timings: SourceBackedRefreshTimings::default(),
        route_results: results,
        zero_source_authority: Vec::new(),
        catalog_route_bindings: Vec::new(),
        verified_index: None,
    };
    let receipt = SourceBackedRefreshReceipt::from_verified_publication(
        None,
        "generation".to_owned(),
        &publication,
    )
    .unwrap();
    RefreshTerminalOutcome::from_published_receipt(&receipt, "attempt")
}

fn published_status(outcome: &RefreshTerminalOutcome) -> Value {
    json!({
        "request_id": "request",
        "logical_request_id": "request",
        "request_state": "published",
        "logical_phase": "terminal",
        "physical_attempt_id": "attempt",
        "physical_attempt_state": "published",
        "progress_owner_request_id": "request",
        "progress_owner_attempt_state": "published",
        "published_generation": "generation",
        "structured_outcome": outcome.to_json(),
        "progress": {"phase": "published", "completed_sources": 0, "total_sources": 0},
    })
}

#[test]
fn published_partial_outcomes_keep_complete_and_diagnostic_only_routes() {
    use RefreshOutcomeCode::*;
    for (rejections, retryable, blocked, code) in [
        (false, false, false, Completed),
        (false, true, false, CompletedWithSourceFailures),
        (false, false, true, CompletedWithSourceFailures),
        (false, true, true, CompletedWithSourceFailures),
        (true, false, false, CompletedWithRejections),
        (true, true, true, CompletedWithRejectionsAndSourceFailures),
    ] {
        let outcome = published_partial_outcome(rejections, retryable, blocked);
        assert_eq!(outcome.code(), code);
        assert_eq!(outcome.retryable(), retryable);
        assert_eq!(
            outcome.retryable_routes(),
            &if retryable {
                BTreeSet::from([route("ab")])
            } else {
                BTreeSet::new()
            }
        );
        assert_eq!(
            outcome.blocked_routes(),
            &if blocked {
                BTreeSet::from([route("cd")])
            } else {
                BTreeSet::new()
            }
        );
        assert_eq!(outcome.affected_routes().contains(&route("ef")), rejections);
        let status = RefreshStatus::parse_schema_v1(published_status(&outcome)).unwrap();
        let RefreshStatusKind::Logical(logical) = status.kind().unwrap() else {
            panic!("expected logical published status");
        };
        assert_eq!(logical.structured_outcome, Some(outcome));
    }
}

#[test]
fn partial_source_failure_status_rejects_incomplete_dispositions() {
    let outcome = published_partial_outcome(false, true, true);
    let mut fields = published_status(&outcome);
    RefreshStatus::parse_schema_v1(fields.clone()).unwrap();
    fields["structured_outcome"]["blocked_routes"] = json!([]);
    let error = RefreshStatus::parse_schema_v1(fields).unwrap_err();
    assert_eq!(
        error.to_string(),
        "source refresh structured outcome has inconsistent route dispositions"
    );
}
