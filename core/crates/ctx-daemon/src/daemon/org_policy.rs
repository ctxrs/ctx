use ctx_core::ids::{OrgId, WorkspaceId};
use ctx_core::models::{DaemonEnrollment, OrgPolicySnapshot, WorkspacePolicyOverlay};

use crate::daemon::{OrgPolicyHandle, WorkspaceOrgPolicyHandle, WorkspaceStoreAccessError};

pub use ctx_org_policy::{
    CacheOrgPolicySnapshotError, UpsertDaemonEnrollmentError, UpsertWorkspacePolicyOverlayError,
    WorkspacePolicyOverlayError,
};

fn workspace_policy_store_error(error: WorkspaceStoreAccessError) -> WorkspacePolicyOverlayError {
    match error {
        WorkspaceStoreAccessError::NotFound => WorkspacePolicyOverlayError::WorkspaceNotFound,
        WorkspaceStoreAccessError::Unavailable(error) => WorkspacePolicyOverlayError::Store(error),
    }
}

fn upsert_workspace_policy_overlay_error(
    error: WorkspacePolicyOverlayError,
) -> UpsertWorkspacePolicyOverlayError {
    ctx_org_policy::workspace_overlay::upsert_workspace_policy_overlay_error(error)
}

impl OrgPolicyHandle {
    pub async fn list_daemon_enrollments(&self) -> anyhow::Result<Vec<DaemonEnrollment>> {
        ctx_org_policy::list_daemon_enrollments(self.store()).await
    }

    #[cfg(test)]
    pub(in crate::daemon) async fn upsert_daemon_enrollment_unchecked(
        &self,
        enrollment: DaemonEnrollment,
    ) -> anyhow::Result<DaemonEnrollment> {
        ctx_org_policy::upsert_daemon_enrollment_unchecked(self.store(), enrollment).await
    }

    pub async fn upsert_daemon_enrollment_checked(
        &self,
        enrollment: DaemonEnrollment,
    ) -> Result<DaemonEnrollment, UpsertDaemonEnrollmentError> {
        ctx_org_policy::upsert_daemon_enrollment_checked(self.store(), enrollment).await
    }

    pub async fn get_daemon_enrollment_by_org_id(
        &self,
        org_id: OrgId,
    ) -> anyhow::Result<Option<DaemonEnrollment>> {
        ctx_org_policy::get_daemon_enrollment_by_org_id(self.store(), org_id).await
    }

    pub async fn upsert_org_policy_snapshot(
        &self,
        snapshot: OrgPolicySnapshot,
    ) -> anyhow::Result<OrgPolicySnapshot> {
        ctx_org_policy::upsert_org_policy_snapshot(self.store(), snapshot).await
    }

    pub async fn cache_and_activate_org_policy_snapshot(
        &self,
        snapshot: OrgPolicySnapshot,
    ) -> Result<OrgPolicySnapshot, CacheOrgPolicySnapshotError> {
        ctx_org_policy::cache_and_activate_org_policy_snapshot(self.store(), snapshot).await
    }
}

impl WorkspaceOrgPolicyHandle {
    pub async fn get_workspace_policy_overlay(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Option<WorkspacePolicyOverlay>, WorkspacePolicyOverlayError> {
        let store = self
            .existing_workspace_store(workspace_id)
            .await
            .map_err(workspace_policy_store_error)?;
        ctx_org_policy::get_workspace_policy_overlay(&store, workspace_id).await
    }

    pub async fn upsert_workspace_policy_overlay(
        &self,
        overlay: WorkspacePolicyOverlay,
    ) -> Result<WorkspacePolicyOverlay, WorkspacePolicyOverlayError> {
        let store = self
            .existing_workspace_store(overlay.workspace_id)
            .await
            .map_err(workspace_policy_store_error)?;
        ctx_org_policy::upsert_workspace_policy_overlay(&store, overlay).await
    }

    pub async fn upsert_workspace_policy_overlay_checked(
        &self,
        overlay: WorkspacePolicyOverlay,
    ) -> Result<WorkspacePolicyOverlay, UpsertWorkspacePolicyOverlayError> {
        ctx_org_policy::validate_daemon_enrollment_for_overlay(self.global_store(), overlay.org_id)
            .await?;
        self.upsert_workspace_policy_overlay(overlay)
            .await
            .map_err(upsert_workspace_policy_overlay_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use ctx_core::ids::{AccountId, DaemonEnrollmentId, OrgId, OrgMembershipId, WorkspaceId};
    use ctx_core::models::{
        DaemonEnrollmentStatus, OrgMembershipRole, PlanType, PolicySignatureAlgorithm, VcsKind,
    };
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

    #[tokio::test]
    async fn upsert_workspace_policy_overlay_checked_preserves_missing_enrollment_ordering() {
        let (_temp, daemon) = test_daemon().await;

        let error = daemon
            .workspace_org_policy_handle_for_test()
            .upsert_workspace_policy_overlay_checked(overlay(WorkspaceId::new(), OrgId::new()))
            .await
            .expect_err("missing enrollment should fail before workspace lookup");

        assert!(matches!(
            error,
            UpsertWorkspacePolicyOverlayError::EnrollmentMissing
        ));
    }

    #[tokio::test]
    async fn upsert_workspace_policy_overlay_checked_rejects_missing_workspace() {
        let (_temp, daemon) = test_daemon().await;
        let org_id = OrgId::new();
        daemon
            .org_policy_handle_for_test()
            .upsert_daemon_enrollment_unchecked(enrollment(org_id))
            .await
            .expect("seed enrollment");

        let error = daemon
            .workspace_org_policy_handle_for_test()
            .upsert_workspace_policy_overlay_checked(overlay(WorkspaceId::new(), org_id))
            .await
            .expect_err("missing workspace should fail");

        assert!(matches!(
            error,
            UpsertWorkspacePolicyOverlayError::WorkspaceNotFound
        ));
    }

    #[tokio::test]
    async fn upsert_workspace_policy_overlay_checked_resolves_workspace_store() {
        let (_temp, daemon) = test_daemon().await;
        let org_id = OrgId::new();
        daemon
            .org_policy_handle_for_test()
            .upsert_daemon_enrollment_unchecked(enrollment(org_id))
            .await
            .expect("seed enrollment");
        let workspace = daemon
            .global_store()
            .create_workspace(
                "workspace".to_string(),
                daemon
                    .data_root()
                    .join("workspace")
                    .to_string_lossy()
                    .to_string(),
                VcsKind::Git,
            )
            .await
            .expect("create workspace");

        let stored = daemon
            .workspace_org_policy_handle_for_test()
            .upsert_workspace_policy_overlay_checked(overlay(workspace.id, org_id))
            .await
            .expect("upsert overlay");

        assert_eq!(stored.workspace_id, workspace.id);
        assert_eq!(stored.org_id, org_id);
    }
}
