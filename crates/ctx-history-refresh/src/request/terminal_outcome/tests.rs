use super::*;

fn nonretryable_outcome(
    code: RefreshOutcomeCode,
    class: RefreshOutcomeClass,
) -> RefreshTerminalOutcome {
    let route = SourceRouteIdentity::from_sha256("ab".repeat(32)).unwrap();
    RefreshTerminalOutcome {
        code,
        class,
        retryable: false,
        affected_routes: BTreeSet::from([route.clone()]),
        retryable_routes: BTreeSet::new(),
        blocked_routes: BTreeSet::from([route]),
        physical_attempt_id: "physical-attempt".to_owned(),
        retained_generation: None,
        published_generation: None,
        retry_advice: Some(RefreshRetryAdvice::InspectSources),
        detail: None,
    }
}

#[test]
fn automatic_retry_pause_contract_is_closed() {
    for (code, class) in [
        (
            RefreshOutcomeCode::SourceRefreshFailed,
            RefreshOutcomeClass::Internal,
        ),
        (
            RefreshOutcomeCode::AllProviderTerminalCoverageUnavailable,
            RefreshOutcomeClass::Coverage,
        ),
    ] {
        assert_eq!(
            RefreshTerminalOutcome::automatic_retry_disposition(code, class, false),
            Some((false, RefreshRetryAdvice::InspectSources)),
        );
        assert_eq!(
            RefreshTerminalOutcome::automatic_retry_disposition(code, class, true),
            Some((true, RefreshRetryAdvice::RetryAffectedRoutes)),
        );
        assert!(nonretryable_outcome(code, class).validate().is_ok());
    }

    assert_eq!(
        RefreshTerminalOutcome::automatic_retry_disposition(
            RefreshOutcomeCode::SourceRefreshInternal,
            RefreshOutcomeClass::Internal,
            false,
        ),
        None,
    );
    assert!(nonretryable_outcome(
        RefreshOutcomeCode::SourceRefreshInternal,
        RefreshOutcomeClass::Internal,
    )
    .validate()
    .is_err());
}

#[test]
fn unsupported_schema_accepts_direct_and_aggregate_advice() {
    let mut outcome = nonretryable_outcome(
        RefreshOutcomeCode::UnsupportedSchema,
        RefreshOutcomeClass::Incompatible,
    );
    for advice in [
        RefreshRetryAdvice::UpgradeOrReconfigure,
        RefreshRetryAdvice::InspectSources,
    ] {
        outcome.retry_advice = Some(advice);
        assert!(outcome.validate().is_ok(), "{advice:?}");
    }
}
