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

#[test]
fn crash_image_failed_exhaustive_attempt_rearms_retryable_route_ownership() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let route = route('a');
    let first = CoreRefreshEngine::new();
    first.initialize_watch_route_authority([route.clone()]);
    first.record_watch_routes_requiring_exhaustive_reconciliation(
        [(route.clone(), EventWatermark::new(1, 1))],
        source_route_ledger_now_ms().saturating_sub(1_000),
    );
    assert!(first
        .enqueue_next_dirty_route(&data_root, source_route_ledger_now_ms())
        .unwrap());
    let request_id = first.lock_state().active_request_id.clone().unwrap();
    assert!(first.prepare_next_pending_admission(&data_root).unwrap());
    assert_eq!(
        first.reconciliation_demand(&request_id),
        Some(SourceBackedReconciliationDemand::Exhaustive)
    );

    let status_path = daemon_source_backed_refresh_job_path(&data_root);
    let crash = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = first.run_next_with(
            |active_request_id, engine| {
                engine.admit_refresh_scope_for_test(
                    active_request_id,
                    &SourceBackedRefreshScope::Exact(BTreeSet::from([route.clone()])),
                )?;
                Err(anyhow!(
                    "injected failed terminal before route finalization"
                ))
            },
            || Ok(None),
            |job| {
                write_daemon_job_status(&status_path, job)?;
                panic!("injected crash after durable failed terminal");
            },
            |_| Ok(()),
        );
    }));
    assert!(crash.is_err());
    assert_eq!(
        read_daemon_job_status(&status_path).unwrap()["request_state"],
        "failed"
    );
    drop(first);

    let recovered = CoreRefreshEngine::new();
    assert!(!recovered
        .recover_interrupted_publication(&data_root)
        .unwrap());
    assert!(recovered
        .enqueue_next_dirty_route(&data_root, u64::MAX)
        .unwrap());
    assert_eq!(
        recovered.active_reconciliation_demand_for_test(),
        Some(SourceBackedReconciliationDemand::Exhaustive)
    );
}
