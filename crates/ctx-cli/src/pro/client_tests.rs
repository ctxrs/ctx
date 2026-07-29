use super::*;
use ctx_pro_host_protocol::GraphState;

fn source_receipt(generation: char) -> ctx_pro_host_protocol::SourceManifestReceipt {
    ctx_pro_host_protocol::SourceManifestReceipt {
        core_generation_id: generation.to_string().repeat(64),
        manifest_aggregate_sha256: "b".repeat(64),
        materializer_revision: "materializer-v1".to_owned(),
        progress: Vec::new(),
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
    assert_eq!(
        required_blame_capabilities(&BlameTarget::PullRequest {
            selector: "https://github.com/ctxrs/ctx/pull/42".to_owned(),
            repository: None,
        }),
        BTreeSet::from([Capability::Status, Capability::Query])
    );
}

#[test]
fn blame_request_requires_source_manifest_authority() {
    let receipt = source_receipt('a');
    let status = StatusResult {
        state: GraphState::Ready,
        authority: ctx_pro_host_protocol::MaterializationAuthority::Source,
        source_receipt: Some(receipt.clone()),
    };
    let request = support::current_blame_request(
        BlameTarget::Commit {
            oid: "0123456789abcdef".to_owned(),
            repository: None,
        },
        10,
        None,
        &status,
    )
    .expect("source-backed request");
    let ctx_pro_host_protocol::QuerySnapshotExpectation::Source { receipt: identity } =
        request.expected_snapshot;
    assert_eq!(identity.core_generation_id, receipt.core_generation_id);
    assert_eq!(
        identity.receipt_sha256,
        ctx_pro_host_protocol::source_manifest_receipt_sha256(&receipt).unwrap()
    );
}

#[test]
fn incomplete_source_authority_fails_closed() {
    let status = StatusResult {
        state: GraphState::NeedsResume,
        authority: ctx_pro_host_protocol::MaterializationAuthority::Source,
        source_receipt: None,
    };
    let error = support::current_blame_request(
        BlameTarget::Commit {
            oid: "0123456789abcdef".to_owned(),
            repository: None,
        },
        10,
        None,
        &status,
    )
    .expect_err("incomplete source authority must fail without fallback");
    assert_eq!(stable_error_code(&error), Some("source_unavailable"));
}

#[test]
fn blame_client_binds_responses_to_original_target() {
    let receipt = source_receipt('a');
    let request = ctx_pro_host_protocol::BlameRequest {
        target: BlameTarget::Commit {
            oid: "0123456789abcdef".to_owned(),
            repository: Some("ctxrs/ctx".to_owned()),
        },
        limit: 10,
        cursor: None,
        expected_snapshot: ctx_pro_host_protocol::QuerySnapshotExpectation::Source {
            receipt: ctx_pro_host_protocol::SourceManifestReceiptIdentity::from_receipt(&receipt)
                .unwrap(),
        },
    };
    let repository = ctx_pro_host_protocol::ResourceRef {
        id: "repository:1".to_owned(),
        kind: ctx_pro_host_protocol::ResourceKind::Repository,
        display: "ctxrs/ctx".to_owned(),
    };
    let explicit_absence = BlameResult {
        target: ctx_pro_host_protocol::ResolvedBlameTarget::Commit {
            commit: ctx_pro_host_protocol::ResourceRef {
                id: "commit:1".to_owned(),
                kind: ctx_pro_host_protocol::ResourceKind::Commit,
                display: "0123456789abcdef".to_owned(),
            },
            repository: repository.clone(),
        },
        git_snapshot: None,
        matches: Vec::new(),
        evidence: Vec::new(),
        next: None,
    };
    validate_blame_response(&request, &explicit_absence).unwrap();

    let wrong_variant = BlameResult {
        target: ctx_pro_host_protocol::ResolvedBlameTarget::PullRequest {
            selector: "42".to_owned(),
            pull_request: ctx_pro_host_protocol::ResourceRef {
                id: "pull_request:1".to_owned(),
                kind: ctx_pro_host_protocol::ResourceKind::PullRequest,
                display: "https://github.com/ctxrs/ctx/pull/42".to_owned(),
            },
            repository,
        },
        git_snapshot: None,
        matches: Vec::new(),
        evidence: Vec::new(),
        next: None,
    };
    let error = validate_blame_response(&request, &wrong_variant)
        .expect_err("cross-target response must fail closed");
    assert_eq!(stable_error_code(&error), Some("invalid_response"));
}

#[test]
fn status_binds_installation_identity_and_preserves_locked_state() {
    let status = BTreeSet::from([Capability::Status]);
    assert!(authorization_required(&status, true));
    assert!(!authorization_required(&status, false));
    assert_eq!(
        status_outcome(GraphState::Ready, Some(EntitlementAccessState::Locked)),
        (false, true, Some("entitlement_expired"))
    );
    assert_eq!(
        status_outcome(
            GraphState::Ready,
            Some(EntitlementAccessState::OfflineGrace)
        ),
        (true, true, None)
    );
}

#[test]
fn normal_pro_client_has_no_legacy_history_authority() {
    let materialization = include_str!("client/materialization.rs");
    let support = include_str!("client_support.rs");
    for forbidden in [
        ["ctx_history_", "store"].concat(),
        ["database_", "path"].concat(),
        ["projection_", "journal"].concat(),
        ["work", ".sqlite"].concat(),
    ] {
        assert!(!materialization.contains(&forbidden), "{forbidden}");
        assert!(!support.contains(&forbidden), "{forbidden}");
    }
}
