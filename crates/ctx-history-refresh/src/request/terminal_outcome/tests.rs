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
