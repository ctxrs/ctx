use super::*;
use ctx_pro_host_protocol::GraphState;
use std::sync::atomic::{AtomicUsize, Ordering};

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
fn helper_startup_requires_git_only_for_git_bound_sessions() {
    assert!(!requires_git_preflight(&BTreeSet::from([
        Capability::Status,
        Capability::Query,
    ])));
    assert!(requires_git_preflight(&BTreeSet::from([
        Capability::Status,
        Capability::Query,
        Capability::GitRead,
    ])));
    assert!(requires_git_preflight(&BTreeSet::from([
        Capability::Status,
    ])));
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
        &receipt.core_generation_id,
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
        &"a".repeat(64),
    )
    .expect_err("incomplete source authority must fail without fallback");
    assert_eq!(stable_error_code(&error), Some("source_unavailable"));
}

#[test]
fn stale_graph_frontier_is_a_typed_error_instead_of_a_valid_looking_result() {
    let receipt = source_receipt('a');
    let status = StatusResult {
        state: GraphState::Ready,
        authority: ctx_pro_host_protocol::MaterializationAuthority::Source,
        source_receipt: Some(receipt),
    };
    let error = support::current_blame_request(
        BlameTarget::Commit {
            oid: "0123456789abcdef".to_owned(),
            repository: None,
        },
        10,
        Some("g1-cursor".to_owned()),
        &status,
        &"b".repeat(64),
    )
    .expect_err("G1 helper authority and cursor must be rejected after verified Core G2");
    assert_eq!(stable_error_code(&error), Some("stale_source"));
}

#[test]
fn default_blame_policy_never_invokes_the_synchronous_wait_path() {
    assert_eq!(
        DEFAULT_BLAME_FRESHNESS_POLICY,
        BlameFreshnessPolicy::LatestCommitted
    );
    let latest_calls = AtomicUsize::new(0);
    let generation = prepare_blame_generation_with(
        BlameFreshnessPolicy::LatestCommitted,
        || {
            latest_calls.fetch_add(1, Ordering::SeqCst);
            Ok("a".repeat(64))
        },
        || panic!("ordinary blame must not synchronously wait for Pro catch-up"),
    )
    .unwrap();

    assert_eq!(generation, "a".repeat(64));
    assert_eq!(latest_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn explicit_wait_policy_uses_only_the_synchronous_wait_path() {
    let wait_calls = AtomicUsize::new(0);
    let generation = prepare_blame_generation_with(
        BlameFreshnessPolicy::WaitForCurrent,
        || panic!("explicit wait policy must not use latest-committed preparation"),
        || {
            wait_calls.fetch_add(1, Ordering::SeqCst);
            Ok("b".repeat(64))
        },
    )
    .unwrap();

    assert_eq!(generation, "b".repeat(64));
    assert_eq!(wait_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn daemon_disabled_still_reads_the_latest_committed_core_generation() {
    let generation = latest_committed_blame_generation_with(
        || anyhow::bail!("daemon start was suppressed (not_allowed)"),
        || Ok("a".repeat(64)),
    )
    .unwrap();

    assert_eq!(generation, "a".repeat(64));
}

#[test]
fn dead_daemon_does_not_hide_an_existing_committed_core_generation() {
    let generation = latest_committed_blame_generation_with(
        || anyhow::bail!("ctx daemon did not become ready: daemon process exited"),
        || Ok("b".repeat(64)),
    )
    .unwrap();

    assert_eq!(generation, "b".repeat(64));
}

#[test]
fn missing_core_generation_preserves_bounded_daemon_wake_failure() {
    let error = latest_committed_blame_generation_with(
        || anyhow::bail!("ctx daemon did not become ready"),
        || anyhow::bail!("source_unavailable: active verified Core generation is missing"),
    )
    .unwrap_err();

    assert_eq!(stable_error_code(&error), Some("source_unavailable"));
    assert!(format!("{error:#}").contains("bounded daemon wake failed"));
}

#[test]
fn already_materialized_graph_stays_bound_to_its_exact_source_generation() {
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
        &receipt.core_generation_id,
    )
    .unwrap();

    let ctx_pro_host_protocol::QuerySnapshotExpectation::Source { receipt: identity } =
        request.expected_snapshot;
    assert_eq!(identity.core_generation_id, receipt.core_generation_id);
}

#[test]
fn core_advance_during_blame_returns_typed_stale_source() {
    let error = ensure_blame_generation_is_current(&"a".repeat(64), &"b".repeat(64)).unwrap_err();
    assert_eq!(stable_error_code(&error), Some("stale_source"));
}

#[test]
fn cli_and_mcp_use_the_same_default_blame_operation() {
    let cli = include_str!("../commands/blame.rs");
    let mcp = include_str!("../mcp/pro.rs");
    let operation = "crate::pro::blame(";

    assert!(cli.contains(operation));
    assert!(mcp.contains(operation));
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
    let materialization = include_str!("client/materialization.rs");
    assert_eq!(
        materialization
            .matches("ProClient::connect_for_status(data_root, &required)")
            .count(),
        2,
        "both materialization status probes must bind the installation identity"
    );
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
