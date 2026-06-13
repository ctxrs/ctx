use ctx_route_contracts::org_policy::{
    CacheOrgPolicySnapshotRouteRequest, DaemonEnrollmentRouteResponse,
    DaemonEnrollmentsRouteResponse, OrgPolicyOrgRouteParams, OrgPolicyRouteError,
    OrgPolicySnapshotRouteResponse, OrgPolicyWorkspaceRouteParams,
    UpsertDaemonEnrollmentRouteRequest, UpsertWorkspacePolicyOverlayRouteRequest,
    WorkspacePolicyOverlayOptionalRouteResponse, WorkspacePolicyOverlayRouteResponse,
};

use crate::daemon::org_policy::{
    CacheOrgPolicySnapshotError, UpsertDaemonEnrollmentError, UpsertWorkspacePolicyOverlayError,
    WorkspacePolicyOverlayError,
};
use crate::daemon::{OrgPolicyHandle, WorkspaceOrgPolicyHandle};

fn upsert_daemon_enrollment_route_error(error: UpsertDaemonEnrollmentError) -> OrgPolicyRouteError {
    match error {
        UpsertDaemonEnrollmentError::UnsupportedPlan => {
            OrgPolicyRouteError::bad_request("daemon enrollment requires a team or enterprise plan")
        }
        UpsertDaemonEnrollmentError::MissingSigningKey => {
            OrgPolicyRouteError::bad_request("daemon enrollment requires a policy signing key")
        }
        UpsertDaemonEnrollmentError::Store(error) => {
            OrgPolicyRouteError::internal(format!("failed to upsert daemon enrollment: {error:#}"))
        }
    }
}

fn cache_org_policy_snapshot_route_error(
    error: CacheOrgPolicySnapshotError,
) -> OrgPolicyRouteError {
    match error {
        CacheOrgPolicySnapshotError::EnrollmentMissing => {
            OrgPolicyRouteError::conflict("daemon is not enrolled for this org")
        }
        CacheOrgPolicySnapshotError::EnrollmentLoad(error) => {
            OrgPolicyRouteError::internal(format!("failed to load daemon enrollment: {error:#}"))
        }
        CacheOrgPolicySnapshotError::InvalidSignature { message } => {
            OrgPolicyRouteError::bad_request(format!(
                "invalid policy snapshot signature: {message}"
            ))
        }
        CacheOrgPolicySnapshotError::SnapshotStore(error) => {
            OrgPolicyRouteError::internal(format!("failed to cache policy snapshot: {error:#}"))
        }
        CacheOrgPolicySnapshotError::EnrollmentActivation(error) => {
            OrgPolicyRouteError::internal(format!("failed to activate policy snapshot: {error:#}"))
        }
    }
}

fn workspace_policy_route_error(
    error: WorkspacePolicyOverlayError,
    message: &'static str,
) -> OrgPolicyRouteError {
    match error {
        WorkspacePolicyOverlayError::WorkspaceNotFound => {
            OrgPolicyRouteError::not_found("workspace not found for org policy")
        }
        WorkspacePolicyOverlayError::Store(error) => {
            OrgPolicyRouteError::internal(format!("{message}: {error:#}"))
        }
    }
}

fn upsert_workspace_policy_route_error(
    error: UpsertWorkspacePolicyOverlayError,
) -> OrgPolicyRouteError {
    match error {
        UpsertWorkspacePolicyOverlayError::EnrollmentMissing => {
            OrgPolicyRouteError::conflict("daemon is not enrolled for this org")
        }
        UpsertWorkspacePolicyOverlayError::EnrollmentLoad(error) => {
            OrgPolicyRouteError::internal(format!("failed to load daemon enrollment: {error:#}"))
        }
        UpsertWorkspacePolicyOverlayError::WorkspaceNotFound => {
            OrgPolicyRouteError::not_found("workspace not found for org policy")
        }
        UpsertWorkspacePolicyOverlayError::Store(error) => OrgPolicyRouteError::internal(format!(
            "failed to upsert workspace org policy: {error:#}"
        )),
    }
}

impl OrgPolicyHandle {
    pub async fn list_daemon_enrollments_for_route(
        &self,
    ) -> Result<DaemonEnrollmentsRouteResponse, OrgPolicyRouteError> {
        self.list_daemon_enrollments()
            .await
            .map(DaemonEnrollmentsRouteResponse::from)
            .map_err(|error| {
                OrgPolicyRouteError::internal(format!(
                    "failed to list daemon enrollments: {error:#}"
                ))
            })
    }

    pub async fn upsert_daemon_enrollment_for_route(
        &self,
        params: OrgPolicyOrgRouteParams,
        request: UpsertDaemonEnrollmentRouteRequest,
    ) -> Result<DaemonEnrollmentRouteResponse, OrgPolicyRouteError> {
        let org_id = params.parse()?;
        let enrollment = request.into_inner();
        if enrollment.org_id != org_id {
            return Err(OrgPolicyRouteError::bad_request(
                "enrollment org_id must match route org id",
            ));
        }
        self.upsert_daemon_enrollment_checked(enrollment)
            .await
            .map(DaemonEnrollmentRouteResponse::from)
            .map_err(upsert_daemon_enrollment_route_error)
    }

    pub async fn cache_org_policy_snapshot_for_route(
        &self,
        params: OrgPolicyOrgRouteParams,
        request: CacheOrgPolicySnapshotRouteRequest,
    ) -> Result<OrgPolicySnapshotRouteResponse, OrgPolicyRouteError> {
        let org_id = params.parse()?;
        let snapshot = request.into_inner();
        if snapshot.org_id != org_id {
            return Err(OrgPolicyRouteError::bad_request(
                "policy snapshot org_id must match route org id",
            ));
        }
        self.cache_and_activate_org_policy_snapshot(snapshot)
            .await
            .map(OrgPolicySnapshotRouteResponse::from)
            .map_err(cache_org_policy_snapshot_route_error)
    }
}

impl WorkspaceOrgPolicyHandle {
    pub async fn get_workspace_policy_overlay_for_route(
        &self,
        params: OrgPolicyWorkspaceRouteParams,
    ) -> Result<WorkspacePolicyOverlayOptionalRouteResponse, OrgPolicyRouteError> {
        let workspace_id = params.parse()?;
        self.get_workspace_policy_overlay(workspace_id)
            .await
            .map(WorkspacePolicyOverlayOptionalRouteResponse::from)
            .map_err(|error| {
                workspace_policy_route_error(error, "failed to load workspace org policy")
            })
    }

    pub async fn upsert_workspace_policy_overlay_for_route(
        &self,
        params: OrgPolicyWorkspaceRouteParams,
        request: UpsertWorkspacePolicyOverlayRouteRequest,
    ) -> Result<WorkspacePolicyOverlayRouteResponse, OrgPolicyRouteError> {
        let workspace_id = params.parse()?;
        let overlay = request.into_inner();
        if overlay.workspace_id != workspace_id {
            return Err(OrgPolicyRouteError::bad_request(
                "workspace policy overlay workspace_id must match route workspace id",
            ));
        }
        self.upsert_workspace_policy_overlay_checked(overlay)
            .await
            .map(WorkspacePolicyOverlayRouteResponse::from)
            .map_err(upsert_workspace_policy_route_error)
    }
}

#[cfg(test)]
mod workspace_overlay_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use ctx_core::ids::{
        AccountId, DaemonEnrollmentId, OrgId, OrgMembershipId, OrgPolicySnapshotId, WorkspaceId,
    };
    use ctx_core::models::{
        ArchiveMode, ArchivePolicy, DaemonEnrollment, DaemonEnrollmentStatus, NetworkProfile,
        OrgMembershipRole, OrgPolicySnapshot, PlanType, PolicyFeatureState,
        PolicySignatureAlgorithm, RoutePolicy, RouteType, VcsKind, WorkspacePolicyOverlay,
    };
    use ctx_route_contracts::org_policy::OrgPolicyRouteErrorKind;
    use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
    use std::collections::BTreeMap;
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

    fn snapshot(org_id: OrgId) -> OrgPolicySnapshot {
        let now = Utc::now();
        OrgPolicySnapshot {
            id: OrgPolicySnapshotId::new(),
            org_id,
            policy_version: "2026-05-16.1".to_string(),
            issued_at: now,
            expires_at: now + Duration::minutes(30),
            grace_expires_at: now + Duration::minutes(60),
            allowed_providers: Some(vec!["fake".to_string()]),
            allowed_models: BTreeMap::new(),
            required_execution_environment: None,
            allowed_network_profiles: vec![NetworkProfile::LlmOnly],
            route_policy: RoutePolicy {
                allowed_route_types: vec![RouteType::UserProviderAccount],
            },
            archive_policy: ArchivePolicy {
                mode: ArchiveMode::OrgSummary,
            },
            features: BTreeMap::from([("org_policy".to_string(), PolicyFeatureState::Enabled)]),
            signature: String::new(),
        }
    }

    fn sign_snapshot(enrollment: &DaemonEnrollment, snapshot: &OrgPolicySnapshot) -> String {
        let claims = serde_json::json!({
            "aud": "ctx.org_policy_snapshot",
            "exp": (Utc::now() + Duration::minutes(60)).timestamp(),
            "org_id": snapshot.org_id.0.to_string(),
            "policy_version": snapshot.policy_version,
            "snapshot_sha256": ctx_org_policy::signature::policy_snapshot_digest_hex(snapshot)
                .expect("snapshot digest"),
        });
        encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(enrollment.policy_signing_key.as_bytes()),
        )
        .expect("sign snapshot")
    }

    fn overlay(workspace_id: WorkspaceId, org_id: OrgId) -> WorkspacePolicyOverlay {
        WorkspacePolicyOverlay {
            workspace_id,
            org_id,
            allowed_providers: Some(vec!["fake".to_string()]),
            allowed_models: BTreeMap::new(),
            required_execution_environment: None,
            allowed_network_profiles: None,
            allowed_route_types: None,
            features: BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn enrollment_route_checks_route_body_mismatch_first() {
        let (_temp, daemon) = test_daemon().await;
        let route_org_id = OrgId::new();
        let mut request = enrollment(OrgId::new());
        request.plan_type = PlanType::Pro;

        let error = daemon
            .org_policy_handle_for_test()
            .upsert_daemon_enrollment_for_route(
                OrgPolicyOrgRouteParams::new(route_org_id.0.to_string()),
                UpsertDaemonEnrollmentRouteRequest::from(request),
            )
            .await
            .expect_err("route/body mismatch should fail first");

        assert_eq!(error.kind(), OrgPolicyRouteErrorKind::BadRequest);
        assert_eq!(error.message(), "enrollment org_id must match route org id");
    }

    #[tokio::test]
    async fn snapshot_route_checks_route_body_mismatch_first() {
        let (_temp, daemon) = test_daemon().await;
        let error = daemon
            .org_policy_handle_for_test()
            .cache_org_policy_snapshot_for_route(
                OrgPolicyOrgRouteParams::new(OrgId::new().0.to_string()),
                CacheOrgPolicySnapshotRouteRequest::from(snapshot(OrgId::new())),
            )
            .await
            .expect_err("route/body mismatch should fail first");

        assert_eq!(error.kind(), OrgPolicyRouteErrorKind::BadRequest);
        assert_eq!(
            error.message(),
            "policy snapshot org_id must match route org id"
        );
    }

    #[tokio::test]
    async fn overlay_route_checks_route_body_mismatch_first() {
        let (_temp, daemon) = test_daemon().await;
        let error = daemon
            .workspace_org_policy_handle_for_test()
            .upsert_workspace_policy_overlay_for_route(
                OrgPolicyWorkspaceRouteParams::new(WorkspaceId::new().0.to_string()),
                UpsertWorkspacePolicyOverlayRouteRequest::from(overlay(
                    WorkspaceId::new(),
                    OrgId::new(),
                )),
            )
            .await
            .expect_err("route/body mismatch should fail first");

        assert_eq!(error.kind(), OrgPolicyRouteErrorKind::BadRequest);
        assert_eq!(
            error.message(),
            "workspace policy overlay workspace_id must match route workspace id"
        );
    }

    #[tokio::test]
    async fn route_facades_classify_domain_errors() {
        let (_temp, daemon) = test_daemon().await;
        let org_id = OrgId::new();

        let snapshot_error = daemon
            .org_policy_handle_for_test()
            .cache_org_policy_snapshot_for_route(
                OrgPolicyOrgRouteParams::new(org_id.0.to_string()),
                CacheOrgPolicySnapshotRouteRequest::from(snapshot(org_id)),
            )
            .await
            .expect_err("missing enrollment should conflict");
        assert_eq!(snapshot_error.kind(), OrgPolicyRouteErrorKind::Conflict);
        assert_eq!(
            snapshot_error.message(),
            "daemon is not enrolled for this org"
        );

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
        let overlay_error = daemon
            .workspace_org_policy_handle_for_test()
            .upsert_workspace_policy_overlay_for_route(
                OrgPolicyWorkspaceRouteParams::new(workspace.id.0.to_string()),
                UpsertWorkspacePolicyOverlayRouteRequest::from(overlay(workspace.id, OrgId::new())),
            )
            .await
            .expect_err("missing enrollment should conflict");
        assert_eq!(overlay_error.kind(), OrgPolicyRouteErrorKind::Conflict);
        assert_eq!(
            overlay_error.message(),
            "daemon is not enrolled for this org"
        );

        let org_id = OrgId::new();
        daemon
            .org_policy_handle_for_test()
            .upsert_daemon_enrollment_unchecked(enrollment(org_id))
            .await
            .expect("seed enrollment");
        let missing_workspace_id = WorkspaceId::new();
        let missing_workspace_error = daemon
            .workspace_org_policy_handle_for_test()
            .upsert_workspace_policy_overlay_for_route(
                OrgPolicyWorkspaceRouteParams::new(missing_workspace_id.0.to_string()),
                UpsertWorkspacePolicyOverlayRouteRequest::from(overlay(
                    missing_workspace_id,
                    org_id,
                )),
            )
            .await
            .expect_err("missing workspace should return not found");
        assert_eq!(
            missing_workspace_error.kind(),
            OrgPolicyRouteErrorKind::NotFound
        );
        assert_eq!(
            missing_workspace_error.message(),
            "workspace not found for org policy"
        );
    }

    #[tokio::test]
    async fn snapshot_route_classifies_invalid_signature() {
        let (_temp, daemon) = test_daemon().await;
        let org_id = OrgId::new();
        let enrollment = enrollment(org_id);
        daemon
            .org_policy_handle_for_test()
            .upsert_daemon_enrollment_unchecked(enrollment)
            .await
            .expect("seed enrollment");
        let mut snapshot = snapshot(org_id);
        snapshot.signature = "invalid".to_string();

        let error = daemon
            .org_policy_handle_for_test()
            .cache_org_policy_snapshot_for_route(
                OrgPolicyOrgRouteParams::new(org_id.0.to_string()),
                CacheOrgPolicySnapshotRouteRequest::from(snapshot),
            )
            .await
            .expect_err("invalid signature should fail");

        assert_eq!(error.kind(), OrgPolicyRouteErrorKind::BadRequest);
        assert!(error
            .message()
            .starts_with("invalid policy snapshot signature:"));
    }

    #[tokio::test]
    async fn snapshot_route_persists_signed_snapshot() {
        let (_temp, daemon) = test_daemon().await;
        let org_id = OrgId::new();
        let enrollment = enrollment(org_id);
        daemon
            .org_policy_handle_for_test()
            .upsert_daemon_enrollment_unchecked(enrollment.clone())
            .await
            .expect("seed enrollment");
        let mut snapshot = snapshot(org_id);
        snapshot.signature = sign_snapshot(&enrollment, &snapshot);

        let response = daemon
            .org_policy_handle_for_test()
            .cache_org_policy_snapshot_for_route(
                OrgPolicyOrgRouteParams::new(org_id.0.to_string()),
                CacheOrgPolicySnapshotRouteRequest::from(snapshot.clone()),
            )
            .await
            .expect("cache snapshot");

        let value = serde_json::to_value(response).expect("response json");
        let snapshot_id_text = snapshot.id.0.to_string();
        assert_eq!(
            value.get("id").and_then(|value| value.as_str()),
            Some(snapshot_id_text.as_str())
        );
    }
}
