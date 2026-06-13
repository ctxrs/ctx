use super::*;

use chrono::Utc;
use ctx_core::ids::{AccountId, DaemonEnrollmentId, OrgId, OrgMembershipId, WorkspaceId};
use ctx_core::models::{
    DaemonEnrollment, DaemonEnrollmentStatus, OrgMembershipRole, PlanType,
    PolicySignatureAlgorithm, VcsKind, WorkspacePolicyOverlay,
};
use ctx_route_contracts::org_policy::OrgPolicyRouteErrorKind;
use tempfile::tempdir;

use crate::test_support::TestDaemon;

async fn test_daemon() -> (tempfile::TempDir, TestDaemon) {
    let temp = tempdir().expect("tempdir");
    let daemon = TestDaemon::new_for_test(
        temp.path().to_path_buf(),
        "http://127.0.0.1:4567".to_string(),
    )
    .await
    .expect("test daemon");
    (temp, daemon)
}

fn enrollment(org_id: OrgId) -> DaemonEnrollment {
    let now = Utc::now();
    DaemonEnrollment {
        id: DaemonEnrollmentId::new(),
        account_id: AccountId::new(),
        org_id,
        org_membership_id: OrgMembershipId::new(),
        membership_role: OrgMembershipRole::Owner,
        plan_type: PlanType::Team,
        status: DaemonEnrollmentStatus::Active,
        policy_signature_algorithm: PolicySignatureAlgorithm::Hs256,
        policy_signing_key: "policy-signing-secret".to_string(),
        active_policy_snapshot_id: None,
        enrolled_at: now,
        updated_at: now,
        revoked_at: None,
    }
}

fn overlay(workspace_id: WorkspaceId, org_id: OrgId) -> WorkspacePolicyOverlay {
    WorkspacePolicyOverlay {
        workspace_id,
        org_id,
        allowed_providers: Some(vec!["fake".to_string()]),
        allowed_models: Default::default(),
        required_execution_environment: None,
        allowed_network_profiles: None,
        allowed_route_types: None,
        features: Default::default(),
    }
}

async fn create_workspace(daemon: &TestDaemon, name: &str) -> ctx_core::models::Workspace {
    daemon
        .global_store()
        .create_workspace(
            name.to_string(),
            daemon.data_root().join(name).to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await
        .expect("create workspace")
}

#[tokio::test]
async fn workspace_overlay_route_missing_enrollment_wins_before_unavailable_workspace_store() {
    let (_temp, daemon) = test_daemon().await;
    let workspace = create_workspace(&daemon, "unavailable-before-enrollment").await;
    daemon
        .cache_rehydration_make_workspace_store_unopenable_for_test(workspace.id)
        .await
        .expect("block workspace store");

    let error = match daemon
        .workspace_org_policy_handle_for_test()
        .upsert_workspace_policy_overlay_for_route(
            OrgPolicyWorkspaceRouteParams::new(workspace.id.0.to_string()),
            UpsertWorkspacePolicyOverlayRouteRequest::from(overlay(workspace.id, OrgId::new())),
        )
        .await
    {
        Ok(_) => panic!("missing enrollment should fail before workspace store lookup"),
        Err(error) => error,
    };

    assert_eq!(error.kind(), OrgPolicyRouteErrorKind::Conflict);
    assert_eq!(error.message(), "daemon is not enrolled for this org");
}

#[tokio::test]
async fn workspace_overlay_route_put_deleting_workspace_returns_not_found() {
    let (_temp, daemon) = test_daemon().await;
    let org_id = OrgId::new();
    daemon
        .org_policy_handle_for_test()
        .upsert_daemon_enrollment_unchecked(enrollment(org_id))
        .await
        .expect("seed enrollment");
    let workspace = create_workspace(&daemon, "deleting-overlay").await;
    daemon.stores().begin_workspace_delete(workspace.id).await;

    let error = match daemon
        .workspace_org_policy_handle_for_test()
        .upsert_workspace_policy_overlay_for_route(
            OrgPolicyWorkspaceRouteParams::new(workspace.id.0.to_string()),
            UpsertWorkspacePolicyOverlayRouteRequest::from(overlay(workspace.id, org_id)),
        )
        .await
    {
        Ok(_) => panic!("deleting workspace should look missing"),
        Err(error) => error,
    };

    assert_eq!(error.kind(), OrgPolicyRouteErrorKind::NotFound);
    assert_eq!(error.message(), "workspace not found for org policy");
    daemon.stores().finish_workspace_delete(workspace.id).await;
}

#[tokio::test]
async fn workspace_overlay_route_get_unavailable_workspace_store_returns_internal() {
    let (_temp, daemon) = test_daemon().await;
    let workspace = create_workspace(&daemon, "unavailable-overlay").await;
    daemon
        .cache_rehydration_make_workspace_store_unopenable_for_test(workspace.id)
        .await
        .expect("block workspace store");

    let error = match daemon
        .workspace_org_policy_handle_for_test()
        .get_workspace_policy_overlay_for_route(OrgPolicyWorkspaceRouteParams::new(
            workspace.id.0.to_string(),
        ))
        .await
    {
        Ok(_) => panic!("unavailable workspace store should be internal"),
        Err(error) => error,
    };

    assert_eq!(error.kind(), OrgPolicyRouteErrorKind::Internal);
    assert!(error
        .message()
        .contains("failed to load workspace org policy"));
}
