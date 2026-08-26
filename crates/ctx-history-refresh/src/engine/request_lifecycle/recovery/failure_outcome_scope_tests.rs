use super::*;

fn route(byte: char) -> SourceRouteIdentity {
    SourceRouteIdentity::from_sha256(byte.to_string().repeat(64)).unwrap()
}

fn exact_scope(route: &SourceRouteIdentity) -> SourceBackedRefreshScope {
    SourceBackedRefreshScope::Exact(BTreeSet::from([route.clone()]))
}

#[test]
fn exact_recovery_rejects_out_of_scope_retryable_disposition() {
    let admitted = route('a');
    let peer = route('b');
    let job = json!({
        "structured_outcome": {
            "code": "source_changed",
            "class": "source_changed",
            "retryable": true,
            "affected_routes": [peer.as_str()],
            "retryable_routes": [peer.as_str()],
            "blocked_routes": [],
            "retry_advice": "retry_automatically"
        }
    });

    let error = recover_failure_outcome(&job, &exact_scope(&admitted), None)
        .expect_err("out-of-scope retryable route must fail closed");

    assert!(format!("{error:#}").contains("exceeds its exact scope"));
}

#[test]
fn exact_recovery_rejects_out_of_scope_blocked_disposition() {
    let admitted = route('a');
    let peer = route('b');
    let job = json!({
        "structured_outcome": {
            "code": "malformed_source",
            "class": "unreadable",
            "retryable": false,
            "affected_routes": [peer.as_str()],
            "retryable_routes": [],
            "blocked_routes": [peer.as_str()],
            "retry_advice": "repair_source"
        }
    });

    let error = recover_failure_outcome(&job, &exact_scope(&admitted), None)
        .expect_err("out-of-scope blocked route must fail closed");

    assert!(format!("{error:#}").contains("exceeds its exact scope"));
}

#[test]
fn exact_recovery_preserves_mixed_source_unclaimed_dispositions() {
    let culprit = route('a');
    let peer = route('b');
    let scope = SourceBackedRefreshScope::Exact(BTreeSet::from([culprit.clone(), peer.clone()]));
    let job = json!({
        "structured_outcome": {
            "code": "source_unclaimed",
            "class": "coverage",
            "retryable": true,
            "affected_routes": [culprit.as_str(), peer.as_str()],
            "retryable_routes": [peer.as_str()],
            "blocked_routes": [culprit.as_str()],
            "retry_advice": "retry_retryable_routes_and_inspect_blocked"
        }
    });

    let outcome = recover_failure_outcome(&job, &scope, None)
        .unwrap()
        .expect("typed source-unclaimed outcome");

    assert_eq!(outcome.code, RefreshOutcomeCode::SourceUnclaimed);
    assert_eq!(outcome.class, RefreshOutcomeClass::Coverage);
    assert!(outcome.retryable);
    assert_eq!(outcome.retryable_routes, BTreeSet::from([peer]));
    assert_eq!(outcome.blocked_routes, BTreeSet::from([culprit]));
    assert_eq!(
        outcome.retry_advice,
        Some(RefreshRetryAdvice::RetryRetryableRoutesAndInspectBlocked)
    );
}

#[test]
fn recovery_rejects_source_unclaimed_without_its_blocked_culprit() {
    let peer = route('b');
    let job = json!({
        "structured_outcome": {
            "code": "source_unclaimed",
            "class": "coverage",
            "retryable": true,
            "affected_routes": [peer.as_str()],
            "retryable_routes": [peer.as_str()],
            "blocked_routes": [],
            "retry_advice": "retry_retryable_routes_and_inspect_blocked"
        }
    });

    let error = recover_failure_outcome(&job, &exact_scope(&peer), None)
        .expect_err("source-unclaimed recovery requires a blocked culprit");

    assert!(format!("{error:#}").contains("source-unclaimed outcome is inconsistent"));
}
