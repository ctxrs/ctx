use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use ctx_pro_host_protocol::{
    CoreMaterializationReceipt, CoreProjectionCurrentness, MaterializedCoverage, ProAccessState,
    ProAccessStatus, ProOperation, RepositoryCoverage,
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
    StatusResult {
        currentness: CoreProjectionCurrentness::Current,
        requested_core_generation_id: Some("a".repeat(64)),
        core_receipt: Some(receipt('a')),
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
fn terminal_empty_status_is_quiet_and_not_blame_ready() {
    let status = status(MaterializedCoverage::Empty);
    status.validate().unwrap();
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
    assert_eq!(stable_error_code(&error), Some("not_materialized"));
}

#[test]
fn invalid_status_contradiction_fails_closed() {
    let mut contradictory = status(MaterializedCoverage::Complete);
    contradictory.core_receipt = None;
    assert!(contradictory.validate().is_err());

    assert_eq!(
        super::client_status::status_outcome(&contradictory, None),
        (false, false, Some("protocol_mismatch"))
    );
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
    let latest_calls = AtomicUsize::new(0);
    let generation = prepare_blame_generation_with(
        BlameFreshnessPolicy::LatestCommitted,
        || {
            latest_calls.fetch_add(1, Ordering::SeqCst);
            Ok("a".repeat(64))
        },
        || panic!("ordinary blame must not synchronously wait"),
    )
    .unwrap();
    assert_eq!(generation, "a".repeat(64));
    assert_eq!(latest_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn explicit_wait_policy_uses_the_wait_path() {
    let generation = prepare_blame_generation_with(
        BlameFreshnessPolicy::WaitForCurrent,
        || panic!("wait policy must not read latest-only state"),
        || Ok("b".repeat(64)),
    )
    .unwrap();
    assert_eq!(generation, "b".repeat(64));
}

#[test]
fn core_advance_during_blame_returns_typed_stale_source() {
    let error = ensure_blame_generation_is_current(&"a".repeat(64), &"b".repeat(64)).unwrap_err();
    assert_eq!(stable_error_code(&error), Some("stale_source"));
}
