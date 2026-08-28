use super::*;

#[test]
fn bounded_failure_diagnostics_do_not_bound_affected_routes_or_retryability() {
    let attempted_routes = (0..70)
        .map(|index| SourceRouteIdentity::from_sha256(format!("{index:064x}")).unwrap())
        .collect::<BTreeSet<_>>();
    let failures = attempted_routes.iter().enumerate().map(|(index, route)| {
        SourceBackedFailedRoute::new(
            route.clone(),
            format!("{index:064x}"),
            CaptureProvider::Codex,
            if index == 69 {
                SourceBackedSourceFailureClass::Unavailable
            } else {
                SourceBackedSourceFailureClass::Incompatible
            },
            false,
            "fixture source",
            "fixture failure",
        )
    });
    let failed_routes = SourceBackedSourceFailures::from_failures(failures);
    assert_eq!(failed_routes.failures().len(), 64);
    assert_eq!(failed_routes.omitted(), 6);
    let error: anyhow::Error =
        SourceBackedCoordinatorError::NoUsableSourceRoutes { failed_routes }.into();

    let outcome = source_backed_refresh_failure_outcome(&error, &attempted_routes);

    assert_eq!(outcome.affected_routes, attempted_routes);
    assert!(outcome.retryable);
    assert_eq!(outcome.blocked_routes.len(), 64);
    assert_eq!(outcome.retryable_routes.len(), 6);
}

#[test]
fn unclaimed_base_source_is_terminal_and_not_retryable() {
    let route = SourceRouteIdentity::from_sha256("aa".repeat(32)).unwrap();
    let attempted_routes = BTreeSet::from([route.clone()]);
    let error: anyhow::Error = SourceBackedCoordinatorError::UnclaimedBaseSource {
        source_id: "fixture-source".into(),
        route_identity: route.clone(),
        route_failures: Vec::new(),
        logical_source_failures: Box::default(),
    }
    .into();

    let mut outcome = source_backed_refresh_failure_outcome(&error, &attempted_routes);

    assert_eq!(outcome.code, RefreshOutcomeCode::SourceUnclaimed);
    assert_eq!(outcome.class, RefreshOutcomeClass::Coverage);
    assert!(!outcome.retryable);
    assert_eq!(outcome.blocked_routes, BTreeSet::from([route.clone()]));
    assert!(outcome.retryable_routes.is_empty());
    assert_eq!(
        outcome.retry_advice,
        Some(RefreshRetryAdvice::InspectSources)
    );

    let original = outcome.clone();
    outcome.pause_automatic_retry_routes(&BTreeSet::from([route]));
    assert_eq!(outcome, original);
}

#[test]
fn unclaimed_base_source_preserves_peer_route_dispositions() {
    let culprit = SourceRouteIdentity::from_sha256("aa".repeat(32)).unwrap();
    let healthy = SourceRouteIdentity::from_sha256("bb".repeat(32)).unwrap();
    let incompatible = SourceRouteIdentity::from_sha256("cc".repeat(32)).unwrap();
    let failed_route = SourceBackedFailedRoute::new(
        incompatible.clone(),
        "fixture-failed-source".into(),
        CaptureProvider::Codex,
        SourceBackedSourceFailureClass::Incompatible,
        true,
        "fixture source",
        "fixture failure",
    );
    let attempted_routes = BTreeSet::from([culprit.clone(), healthy.clone(), incompatible.clone()]);
    let error: anyhow::Error = SourceBackedCoordinatorError::UnclaimedBaseSource {
        source_id: "fixture-source".into(),
        route_identity: culprit.clone(),
        route_failures: vec![(&failed_route).into()],
        logical_source_failures: Box::default(),
    }
    .into();

    let outcome = source_backed_refresh_failure_outcome(&error, &attempted_routes);

    assert_eq!(outcome.code, RefreshOutcomeCode::SourceUnclaimed);
    assert_eq!(outcome.class, RefreshOutcomeClass::Coverage);
    assert!(outcome.retryable);
    assert_eq!(outcome.retryable_routes, BTreeSet::from([healthy]));
    assert_eq!(
        outcome.blocked_routes,
        BTreeSet::from([culprit, incompatible])
    );
    assert_eq!(
        outcome.retry_advice,
        Some(RefreshRetryAdvice::RetryRetryableRoutesAndInspectBlocked)
    );
}

#[test]
fn route_index_and_internal_failures_have_stable_retry_classes() {
    let route = SourceRouteIdentity::from_sha256("aa".repeat(32)).unwrap();
    let attempted_routes = BTreeSet::from([route]);
    let cases: Vec<(anyhow::Error, RefreshOutcomeCode, RefreshOutcomeClass, bool)> = vec![
        (
            ZeroSourcePublicationBlocked::new("fixture catalog blocker").into(),
            RefreshOutcomeCode::AllProviderTerminalCoverageUnavailable,
            RefreshOutcomeClass::Coverage,
            true,
        ),
        (
            SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::ResourceUnavailable,
                "fixture resource pressure",
            )
            .into(),
            RefreshOutcomeCode::ResourceUnavailable,
            RefreshOutcomeClass::ResourceUnavailable,
            true,
        ),
        (
            SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::InvalidSource,
                "fixture malformed source",
            )
            .into(),
            RefreshOutcomeCode::MalformedSource,
            RefreshOutcomeClass::Unreadable,
            false,
        ),
        (
            SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Unsupported,
                "fixture incompatible source",
            )
            .into(),
            RefreshOutcomeCode::UnsupportedSchema,
            RefreshOutcomeClass::Incompatible,
            false,
        ),
        (
            IndexError::MissingCommitPayload.into(),
            RefreshOutcomeCode::IndexCorruption,
            RefreshOutcomeClass::Corruption,
            false,
        ),
        (
            IndexError::InvalidSourceRouteIdentity.into(),
            RefreshOutcomeCode::IndexCorruption,
            RefreshOutcomeClass::Corruption,
            false,
        ),
        (
            SourceBackedCoordinatorError::Index(IndexError::InvalidSourceRouteIdentity).into(),
            RefreshOutcomeCode::IndexCorruption,
            RefreshOutcomeClass::Corruption,
            false,
        ),
        (
            IndexError::SchemaMismatch(1).into(),
            RefreshOutcomeCode::IndexIncompatible,
            RefreshOutcomeClass::Incompatible,
            false,
        ),
        (
            anyhow!("fixture internal failure"),
            RefreshOutcomeCode::SourceRefreshFailed,
            RefreshOutcomeClass::Internal,
            true,
        ),
    ];

    for (error, code, class, retryable) in cases {
        let outcome = source_backed_refresh_failure_outcome(&error, &attempted_routes);
        assert_eq!(outcome.code, code);
        assert_eq!(outcome.class, class);
        assert_eq!(outcome.retryable, retryable);
    }
}
