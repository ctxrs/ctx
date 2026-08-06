use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use ctx_pro_host_protocol::{
    BlameAttribution, BlameCoverage, BlameCoverageUnit, BlameOutcome, BlameResult,
    CoreMaterializationReceipt, CoreProjectionCurrentness, ErrorClass, MaterializedCoverage,
    ProAccessState, ProAccessStatus, ProOperation, ProStorageEvidence, ProtocolError,
    QuerySnapshotExpectation, RepositoryCoverage, ResolvedBlameTarget, ResourceKind, ResourceRef,
};

fn receipt(generation: char) -> CoreMaterializationReceipt {
    CoreMaterializationReceipt {
        core_generation_id: generation.to_string().repeat(64),
        core_record_contract_fingerprint: "b".repeat(64),
        source_snapshot_sha256: "c".repeat(64),
        materializer_revision: "materializer-v1".to_owned(),
        source_count: 1,
        event_count: 1,
    }
}

fn status(coverage: MaterializedCoverage) -> StatusResult {
    let supported_operations = BTreeSet::from([
        ProOperation::FileBlame,
        ProOperation::CommitBlame,
        ProOperation::PullRequestBlame,
    ]);
    let mut core_receipt = receipt('a');
    if coverage == MaterializedCoverage::Empty {
        core_receipt.event_count = 0;
    }
    StatusResult {
        currentness: CoreProjectionCurrentness::Current,
        requested_core_generation_id: Some("a".repeat(64)),
        core_receipt: Some(core_receipt),
        coverage,
        repository_coverage: if coverage == MaterializedCoverage::Complete {
            RepositoryCoverage {
                repository_candidate_events: 1,
                logical_binding_events: 1,
                certified_live_root_access_events: 1,
                file_evidence_events: 1,
                exact_commit_evidence_events: 1,
                exact_pull_request_evidence_events: 1,
            }
        } else {
            RepositoryCoverage::default()
        },
        core_preparation_peak_workers: 4,
        storage_evidence: Some(ProStorageEvidence {
            graph_manifest_schema: 3,
            flat_format_version: 2,
            materializer_checkpoint_version: 4,
            journal_pack_format_version: 3,
            legacy_journals_written: 0,
            journal_pages_written: 2,
            journal_packs_written: 1,
            journal_finish_activity: ctx_pro_host_protocol::JournalFinishActivity {
                worker_limit: 1,
                peak_workers: 1,
                started_after_preparation: true,
            },
        }),
        access: ProAccessStatus {
            entitlement: ProAccessState::Available,
            graph_key: ProAccessState::Available,
            local_repository: ProAccessState::Available,
        },
        available_operations: if coverage == MaterializedCoverage::Complete {
            supported_operations.clone()
        } else {
            BTreeSet::new()
        },
        supported_operations,
    }
}

fn empty_commit_result(snapshot: QuerySnapshotExpectation) -> BlameResult {
    BlameResult {
        snapshot,
        target: ResolvedBlameTarget::Commit {
            commit: ResourceRef {
                id: "commit:0123456789abcdef".to_owned(),
                kind: ResourceKind::Commit,
                display: "0123456789abcdef".to_owned(),
            },
            repository: ResourceRef {
                id: "repository:fixture".to_owned(),
                kind: ResourceKind::Repository,
                display: "fixture/repository".to_owned(),
            },
        },
        git_snapshot: None,
        outcome: BlameOutcome {
            attribution: BlameAttribution::None,
            coverage: BlameCoverage {
                unit: BlameCoverageUnit::CommitFact,
                evaluated: 0,
                proven: 0,
                possible: 0,
                conflicting: 0,
                none: 0,
            },
        },
        matches: Vec::new(),
        evidence: Vec::new(),
        next: None,
        lineage: None,
    }
}

#[test]
fn blame_capabilities_require_git_only_for_file_targets() {
    assert_eq!(
        required_blame_capabilities(&BlameTarget::File {
            path: "src/lib.rs".to_owned(),
            repository: None,
            lines: None,
        }),
        BTreeSet::from([Capability::Status, Capability::Query, Capability::GitRead])
    );
    assert_eq!(
        required_blame_capabilities(&BlameTarget::Commit {
            oid: "0123456789abcdef".to_owned(),
            repository: None,
        }),
        BTreeSet::from([Capability::Status, Capability::Query])
    );
}

#[test]
fn every_helper_session_requires_the_current_protocol_fingerprint() {
    let core = BTreeSet::from([Capability::Status, Capability::CoreMaterialization]);
    for fingerprint in [
        "0a5db03b653a8effa18ff3ef6a275b085f3e68eedf7d3f982ac18d4a6c38b642",
        "dd902ae952032e66fcdaabcdc84522bde4350dace2bca28b9b16e67ffe6ef2fd",
    ] {
        let error = super::transport::validate_protocol_session(&core, fingerprint).unwrap_err();
        assert!(error
            .to_string()
            .contains("requires the current Protocol V1 fingerprint"));
    }
    super::transport::validate_protocol_session(&core, PROTOCOL_FINGERPRINT).unwrap();

    let query = BTreeSet::from([Capability::Status, Capability::Query]);
    assert!(super::transport::validate_protocol_session(
        &query,
        "0a5db03b653a8effa18ff3ef6a275b085f3e68eedf7d3f982ac18d4a6c38b642",
    )
    .is_err());
}

#[test]
fn blame_request_is_bound_to_the_exact_core_receipt() {
    let status = status(MaterializedCoverage::Complete);
    let request = support::current_blame_request(
        BlameTarget::Commit {
            oid: "0123456789abcdef".to_owned(),
            repository: None,
        },
        10,
        None,
        &status,
        &"a".repeat(64),
    )
    .unwrap();
    let ctx_pro_host_protocol::QuerySnapshotExpectation::Core { receipt } =
        request.expected_snapshot;
    assert_eq!(receipt.core_generation_id, "a".repeat(64));
}

#[test]
fn client_rejects_an_empty_result_from_another_materializer_revision_before_rendering() {
    let status = status(MaterializedCoverage::Complete);
    let request = support::current_blame_request(
        BlameTarget::Commit {
            oid: "0123456789abcdef".to_owned(),
            repository: None,
        },
        10,
        None,
        &status,
        &"a".repeat(64),
    )
    .unwrap();
    let matching = empty_commit_result(request.expected_snapshot.clone());
    validate_blame_response(&request, &matching).unwrap();

    let mut mismatched = matching;
    let QuerySnapshotExpectation::Core { receipt } = &mut mismatched.snapshot;
    receipt.materializer_revision = "materializer-v2".to_owned();
    let error = validate_blame_response(&request, &mismatched).unwrap_err();
    assert_eq!(stable_error_code(&error), Some("invalid_response"));
    assert!(error
        .to_string()
        .contains("blame result snapshot does not match the requested Core snapshot"));
}

#[test]
fn terminal_quiet_status_is_current_materialized_and_not_blame_ready() {
    for coverage in [MaterializedCoverage::Empty, MaterializedCoverage::Abstained] {
        let status = status(coverage);
        status.validate().unwrap();
        assert_eq!(
            super::client_status::status_outcome(&status, None, &"a".repeat(64)),
            (false, true, None),
            "{coverage:?}"
        );
        let error = support::current_blame_request(
            BlameTarget::Commit {
                oid: "0123456789abcdef".to_owned(),
                repository: None,
            },
            10,
            None,
            &status,
            &"a".repeat(64),
        )
        .unwrap_err();
        assert_eq!(
            stable_error_code(&error),
            Some("not_materialized"),
            "{coverage:?}"
        );
    }
}

#[test]
fn current_helper_receipt_is_ready_for_the_active_core_generation() {
    let current = status(MaterializedCoverage::Complete);

    assert_eq!(
        super::client_status::status_outcome(&current, None, &"a".repeat(64)),
        (true, true, None)
    );
}

#[test]
fn invalid_status_contradiction_fails_closed() {
    let mut contradictory = status(MaterializedCoverage::Complete);
    contradictory.core_receipt = None;
    assert!(contradictory.validate().is_err());

    assert_eq!(
        super::client_status::status_outcome(&contradictory, None, &"a".repeat(64)),
        (false, false, Some("protocol_mismatch"))
    );
}

#[test]
fn malformed_current_receipt_fails_closed_as_a_protocol_mismatch() {
    let mut malformed = status(MaterializedCoverage::Complete);
    malformed.requested_core_generation_id = Some("d".repeat(64));
    malformed.core_receipt.as_mut().unwrap().core_generation_id = "malformed".to_owned();

    assert_eq!(
        super::client_status::status_outcome(&malformed, None, &"d".repeat(64)),
        (false, false, Some("protocol_mismatch"))
    );
}

#[test]
fn status_rejects_a_current_receipt_for_a_stale_core_generation() {
    let mut stale = status(MaterializedCoverage::Complete);
    stale.requested_core_generation_id = Some("d".repeat(64));

    assert_eq!(
        super::client_status::status_outcome(&stale, None, &"d".repeat(64)),
        (false, false, Some("stale_source"))
    );
}

#[test]
fn status_preserves_explicit_stale_currentness_for_the_pinned_generation() {
    let mut stale = status(MaterializedCoverage::Complete);
    stale.currentness = CoreProjectionCurrentness::Stale;
    stale.requested_core_generation_id = Some("d".repeat(64));
    stale.coverage = MaterializedCoverage::Partial;
    stale.repository_coverage = RepositoryCoverage::default();
    stale.available_operations.clear();

    assert_eq!(
        super::client_status::status_outcome(&stale, None, &"d".repeat(64)),
        (false, false, Some("stale_source"))
    );
}

#[test]
fn missing_helper_status_does_not_require_an_active_core_generation() {
    let temp = tempfile::tempdir().unwrap();
    let status = super::client_status::status_with_helper_resolver(temp.path(), |_| {
        bail!("pro_not_installed: helper fixture is absent")
    });

    assert!(!status.installed);
    assert!(!status.ready);
    assert!(!status.materialized);
    assert_eq!(status.error_code.as_deref(), Some("pro_not_installed"));
}

#[test]
fn installed_status_fails_closed_when_active_core_cannot_be_pinned() {
    let temp = tempfile::tempdir().unwrap();
    let status = super::client_status::status_with_helper_resolver(temp.path(), |_| {
        Ok(temp.path().join("ctx-pro"))
    });

    assert!(status.installed);
    assert!(!status.ready);
    assert!(!status.materialized);
    assert_eq!(status.error_code.as_deref(), Some("source_unavailable"));
    assert!(status.projection_currentness.is_none());
    assert!(status.materialized_coverage.is_none());
    assert!(status.supported_operations.is_none());
    assert!(status.available_operations.is_none());
}

#[test]
fn stale_core_receipt_fails_closed() {
    let status = status(MaterializedCoverage::Complete);
    let error = support::current_blame_request(
        BlameTarget::Commit {
            oid: "0123456789abcdef".to_owned(),
            repository: None,
        },
        10,
        None,
        &status,
        &"d".repeat(64),
    )
    .unwrap_err();
    assert_eq!(stable_error_code(&error), Some("stale_source"));
}

#[test]
fn default_blame_policy_reads_latest_committed_without_waiting() {
    let wake_calls = AtomicUsize::new(0);
    let expected_active_generation = prepare_blame_freshness_with(
        BlameFreshnessPolicy::LatestCommitted,
        || {
            wake_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        },
        || panic!("ordinary blame must not synchronously wait"),
    )
    .unwrap();
    assert_eq!(expected_active_generation, None);
    assert_eq!(wake_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn latest_committed_blame_ignores_a_failed_background_wake() {
    let expected_active_generation = prepare_blame_freshness_with(
        BlameFreshnessPolicy::LatestCommitted,
        || bail!("source_unavailable: daemon is unavailable"),
        || panic!("ordinary blame must not synchronously wait"),
    )
    .unwrap();
    assert_eq!(expected_active_generation, None);
}

#[test]
fn latest_committed_blame_uses_the_helpers_trailing_receipt() {
    let status = status(MaterializedCoverage::Complete);
    let generation = blame_core_generation(&status, None).unwrap();
    assert_eq!(generation, "a".repeat(64));
    support::current_blame_request(
        BlameTarget::Commit {
            oid: "0123456789abcdef".to_owned(),
            repository: None,
        },
        10,
        None,
        &status,
        &generation,
    )
    .unwrap();
}

#[test]
fn explicit_wait_policy_uses_the_wait_path() {
    let generation = prepare_blame_freshness_with(
        BlameFreshnessPolicy::WaitForCurrent,
        || panic!("wait policy must not read latest-only state"),
        || Ok("b".repeat(64)),
    )
    .unwrap();
    assert_eq!(generation, Some("b".repeat(64)));
}

#[test]
fn core_advance_during_blame_returns_typed_stale_source() {
    let error =
        ensure_active_core_generation_is_unchanged(&"a".repeat(64), &"b".repeat(64)).unwrap_err();
    assert_eq!(stable_error_code(&error), Some("stale_source"));
}

#[test]
fn blame_freshness_accepts_only_current_or_fully_positive_stale_coverage() {
    let outcome = |attribution, proven, possible, conflicting, none| BlameOutcome {
        attribution,
        coverage: BlameCoverage {
            unit: BlameCoverageUnit::CommitFact,
            evaluated: proven + possible + conflicting + none,
            proven,
            possible,
            conflicting,
            none,
        },
    };
    let current_none = outcome(BlameAttribution::None, 0, 0, 0, 0);
    assert_eq!(
        classify_blame_snapshot_freshness("a", &current_none, "a", "a").unwrap(),
        BlameResultFreshness::Current
    );

    for positive in [
        outcome(BlameAttribution::Proven, 1, 0, 0, 0),
        outcome(BlameAttribution::Possible, 1, 1, 0, 0),
        outcome(BlameAttribution::Conflicting, 1, 0, 1, 0),
    ] {
        assert_eq!(
            classify_blame_snapshot_freshness("a", &positive, "b", "b").unwrap(),
            BlameResultFreshness::StaleCommitted
        );
    }

    for stale_miss in [
        outcome(BlameAttribution::None, 0, 0, 0, 1),
        outcome(BlameAttribution::Possible, 1, 0, 0, 1),
    ] {
        let error = classify_blame_snapshot_freshness("a", &stale_miss, "b", "b").unwrap_err();
        assert_eq!(stable_error_code(&error), Some("stale_source"));
    }
}

#[test]
fn stale_generation_bound_failure_becomes_one_typed_stale_diagnostic() {
    let error = protocol_blame_error(
        ProtocolError::new(ErrorClass::ResourceNotFound, "ignored helper detail")
            .with_blame_details(ctx_pro_host_protocol::BlameDiagnosticDetails {
                reason: ctx_pro_host_protocol::BlameDiagnosticReason::TargetNotIndexed,
                candidates: Vec::new(),
                candidates_truncated: false,
            }),
        &BlameTarget::Commit {
            oid: "a".repeat(40),
            repository: None,
        },
    );
    assert!(is_generation_bound_negative(&error));

    let stale = stale_negative_diagnostic();
    let diagnostic = blame_diagnostic(&stale).unwrap();
    assert_eq!(diagnostic.error_code, "stale_source");
    assert!(diagnostic.retryable);
    assert!(diagnostic.candidates.is_empty());
    assert_ne!(
        diagnostic.next_action.as_ref().map(|action| action.kind),
        Some(crate::pro::diagnostic::BlameNextActionKind::SearchCore)
    );

    let entitlement = protocol_blame_error(
        ProtocolError::new(ErrorClass::EntitlementExpired, "ignored helper detail"),
        &BlameTarget::Commit {
            oid: "a".repeat(40),
            repository: None,
        },
    );
    assert!(!is_generation_bound_negative(&entitlement));
}

#[test]
fn pro_receipt_advance_during_blame_returns_typed_stale_source() {
    let mut advanced = status(MaterializedCoverage::Complete);
    advanced.requested_core_generation_id = None;
    advanced.core_receipt = Some(receipt('b'));
    let error = ensure_committed_pro_receipt_is_unchanged(&receipt('a'), &advanced).unwrap_err();
    assert_eq!(stable_error_code(&error), Some("stale_source"));
}

#[test]
fn unchanged_pro_receipt_after_blame_is_accepted() {
    let status = status(MaterializedCoverage::Complete);
    ensure_committed_pro_receipt_is_unchanged(&receipt('a'), &status).unwrap();
}

#[test]
fn missing_pro_receipt_after_blame_fails_closed() {
    let mut missing = status(MaterializedCoverage::Complete);
    missing.currentness = CoreProjectionCurrentness::NotMaterialized;
    missing.requested_core_generation_id = None;
    missing.core_receipt = None;
    missing.coverage = MaterializedCoverage::NotMaterialized;
    missing.repository_coverage = RepositoryCoverage::default();
    missing.available_operations.clear();
    missing.storage_evidence = None;
    let error = ensure_committed_pro_receipt_is_unchanged(&receipt('a'), &missing).unwrap_err();
    assert_eq!(stable_error_code(&error), Some("stale_source"));
}

#[test]
fn incoherent_pro_receipt_after_blame_fails_closed() {
    let mut incoherent = status(MaterializedCoverage::Complete);
    incoherent.core_receipt = None;
    let error = ensure_committed_pro_receipt_is_unchanged(&receipt('a'), &incoherent).unwrap_err();
    assert_eq!(stable_error_code(&error), Some("invalid_response"));
}
