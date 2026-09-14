use super::*;

fn route(byte: char) -> SourceRouteIdentity {
    SourceRouteIdentity::from_sha256(byte.to_string().repeat(64)).unwrap()
}

#[test]
fn recovery_preserves_valid_mixed_dispositions_and_rejects_scope_escape() {
    let culprit = route('a');
    let peer = route('b');
    let attempt_id = uuid::Uuid::nil().to_string();
    let job = json!({
        "structured_outcome": {
            "code": "source_unclaimed",
            "class": "coverage",
            "retryable": true,
            "affected_routes": [culprit.as_str(), peer.as_str()],
            "retryable_routes": [peer.as_str()],
            "blocked_routes": [culprit.as_str()],
            "physical_attempt_id": attempt_id,
            "retry_advice": "retry_retryable_routes_and_inspect_blocked"
        }
    });
    let scope = SourceBackedRefreshScope::Exact(BTreeSet::from([culprit.clone(), peer.clone()]));
    let outcome = recover_failure_outcome(&job, &scope, None, &attempt_id, None, None)
        .unwrap()
        .expect("typed terminal outcome");
    assert_eq!(outcome.code(), RefreshOutcomeCode::SourceUnclaimed);
    assert_eq!(outcome.retryable_routes(), &BTreeSet::from([peer]));
    assert_eq!(outcome.blocked_routes(), &BTreeSet::from([culprit]));

    let out_of_scope = route('c');
    let mut escaped = job;
    escaped["structured_outcome"]["affected_routes"] = json!([out_of_scope.as_str()]);
    escaped["structured_outcome"]["retryable_routes"] = json!([out_of_scope.as_str()]);
    escaped["structured_outcome"]["blocked_routes"] = json!([]);
    assert!(recover_failure_outcome(&escaped, &scope, None, &attempt_id, None, None).is_err());
}
