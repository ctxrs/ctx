use std::collections::HashMap;

use anyhow::Result;
use chrono::Utc;
use ctx_core::ids::{OrgId, RunId, WorkspaceId};
use ctx_core::models::{
    DaemonEnrollment, DaemonEnrollmentStatus, ExecutionEnvironment, OrgPolicySnapshot, PlanType,
    PolicySignatureAlgorithm, RunStatus, Session, WorkspacePolicyOverlay,
};
use ctx_store::Store;
use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use sha2::Digest;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum UpsertDaemonEnrollmentError {
    #[error("daemon enrollment requires a team or enterprise plan")]
    UnsupportedPlan,
    #[error("daemon enrollment requires a policy signing key")]
    MissingSigningKey,
    #[error(transparent)]
    Store(anyhow::Error),
}

#[derive(Debug, Error)]
pub enum CacheOrgPolicySnapshotError {
    #[error("daemon is not enrolled for this org")]
    EnrollmentMissing,
    #[error("failed to load daemon enrollment")]
    EnrollmentLoad(anyhow::Error),
    #[error("invalid policy snapshot signature: {message}")]
    InvalidSignature { message: String },
    #[error("failed to cache policy snapshot")]
    SnapshotStore(anyhow::Error),
    #[error("failed to activate policy snapshot")]
    EnrollmentActivation(anyhow::Error),
}

#[derive(Debug, Error)]
pub enum WorkspacePolicyOverlayError {
    #[error("workspace not found")]
    WorkspaceNotFound,
    #[error(transparent)]
    Store(anyhow::Error),
}

#[derive(Debug, Error)]
pub enum UpsertWorkspacePolicyOverlayError {
    #[error("daemon is not enrolled for this org")]
    EnrollmentMissing,
    #[error("failed to load daemon enrollment")]
    EnrollmentLoad(anyhow::Error),
    #[error("workspace not found")]
    WorkspaceNotFound,
    #[error(transparent)]
    Store(anyhow::Error),
}

pub async fn list_daemon_enrollments(store: &Store) -> Result<Vec<DaemonEnrollment>> {
    store.list_daemon_enrollments().await
}

pub async fn upsert_daemon_enrollment_unchecked(
    store: &Store,
    enrollment: DaemonEnrollment,
) -> Result<DaemonEnrollment> {
    store.upsert_daemon_enrollment(enrollment).await
}

pub async fn upsert_daemon_enrollment_checked(
    store: &Store,
    enrollment: DaemonEnrollment,
) -> Result<DaemonEnrollment, UpsertDaemonEnrollmentError> {
    if !matches!(enrollment.plan_type, PlanType::Team | PlanType::Enterprise) {
        return Err(UpsertDaemonEnrollmentError::UnsupportedPlan);
    }
    if enrollment.policy_signing_key.trim().is_empty() {
        return Err(UpsertDaemonEnrollmentError::MissingSigningKey);
    }
    store
        .upsert_daemon_enrollment(enrollment)
        .await
        .map_err(UpsertDaemonEnrollmentError::Store)
}

pub async fn get_daemon_enrollment_by_org_id(
    store: &Store,
    org_id: OrgId,
) -> Result<Option<DaemonEnrollment>> {
    store.get_daemon_enrollment_by_org_id(org_id).await
}

pub async fn upsert_org_policy_snapshot(
    store: &Store,
    snapshot: OrgPolicySnapshot,
) -> Result<OrgPolicySnapshot> {
    store.upsert_org_policy_snapshot(snapshot).await
}

pub async fn cache_and_activate_org_policy_snapshot(
    store: &Store,
    snapshot: OrgPolicySnapshot,
) -> Result<OrgPolicySnapshot, CacheOrgPolicySnapshotError> {
    let enrollment = store
        .get_daemon_enrollment_by_org_id(snapshot.org_id)
        .await
        .map_err(CacheOrgPolicySnapshotError::EnrollmentLoad)?
        .ok_or(CacheOrgPolicySnapshotError::EnrollmentMissing)?;
    verify_snapshot_signature(&enrollment, &snapshot)?;
    let snapshot = store
        .upsert_org_policy_snapshot(snapshot)
        .await
        .map_err(CacheOrgPolicySnapshotError::SnapshotStore)?;
    let mut enrollment = enrollment;
    enrollment.active_policy_snapshot_id = Some(snapshot.id);
    enrollment.updated_at = Utc::now();
    store
        .upsert_daemon_enrollment(enrollment)
        .await
        .map_err(CacheOrgPolicySnapshotError::EnrollmentActivation)?;
    Ok(snapshot)
}

pub async fn get_workspace_policy_overlay(
    store: &Store,
    workspace_id: WorkspaceId,
) -> Result<Option<WorkspacePolicyOverlay>, WorkspacePolicyOverlayError> {
    store
        .get_workspace_policy_overlay(workspace_id)
        .await
        .map_err(WorkspacePolicyOverlayError::Store)
}

pub async fn upsert_workspace_policy_overlay(
    store: &Store,
    overlay: WorkspacePolicyOverlay,
) -> Result<WorkspacePolicyOverlay, WorkspacePolicyOverlayError> {
    store
        .upsert_workspace_policy_overlay(overlay)
        .await
        .map_err(WorkspacePolicyOverlayError::Store)
}

pub async fn validate_daemon_enrollment_for_overlay(
    store: &Store,
    org_id: OrgId,
) -> Result<(), UpsertWorkspacePolicyOverlayError> {
    let enrollment = store
        .get_daemon_enrollment_by_org_id(org_id)
        .await
        .map_err(UpsertWorkspacePolicyOverlayError::EnrollmentLoad)?
        .ok_or(UpsertWorkspacePolicyOverlayError::EnrollmentMissing)?;
    if !matches!(enrollment.status, DaemonEnrollmentStatus::Active) {
        return Err(UpsertWorkspacePolicyOverlayError::EnrollmentMissing);
    }
    Ok(())
}

fn verify_snapshot_signature(
    enrollment: &DaemonEnrollment,
    snapshot: &OrgPolicySnapshot,
) -> Result<(), CacheOrgPolicySnapshotError> {
    if snapshot.signature.trim().is_empty() {
        return Err(CacheOrgPolicySnapshotError::InvalidSignature {
            message: "missing signature".to_string(),
        });
    }
    if !matches!(
        enrollment.policy_signature_algorithm,
        PolicySignatureAlgorithm::Hs256
    ) {
        return Err(CacheOrgPolicySnapshotError::InvalidSignature {
            message: "unsupported signature algorithm in public ADE export".to_string(),
        });
    }
    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_audience(&["ctx.org_policy_snapshot"]);
    let token = decode::<PolicySnapshotClaims>(
        &snapshot.signature,
        &DecodingKey::from_secret(enrollment.policy_signing_key.as_bytes()),
        &validation,
    )
    .map_err(|error| CacheOrgPolicySnapshotError::InvalidSignature {
        message: error.to_string(),
    })?;
    let digest = signature::policy_snapshot_digest_hex(snapshot).map_err(|error| {
        CacheOrgPolicySnapshotError::InvalidSignature {
            message: error.to_string(),
        }
    })?;
    let claims = token.claims;
    if claims.org_id != snapshot.org_id.0.to_string()
        || claims.policy_version != snapshot.policy_version
        || claims.snapshot_sha256 != digest
    {
        return Err(CacheOrgPolicySnapshotError::InvalidSignature {
            message: "claims do not match snapshot".to_string(),
        });
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct PolicySnapshotClaims {
    org_id: String,
    policy_version: String,
    snapshot_sha256: String,
}

pub mod workspace_overlay {
    use super::*;

    pub fn upsert_workspace_policy_overlay_error(
        error: WorkspacePolicyOverlayError,
    ) -> UpsertWorkspacePolicyOverlayError {
        match error {
            WorkspacePolicyOverlayError::WorkspaceNotFound => {
                UpsertWorkspacePolicyOverlayError::WorkspaceNotFound
            }
            WorkspacePolicyOverlayError::Store(error) => {
                UpsertWorkspacePolicyOverlayError::Store(error)
            }
        }
    }
}

pub mod signature {
    use super::*;

    pub fn policy_snapshot_digest_hex(snapshot: &OrgPolicySnapshot) -> Result<String> {
        let mut digest_snapshot = snapshot.clone();
        digest_snapshot.signature.clear();
        let bytes = serde_json::to_vec(&digest_snapshot)?;
        let mut hasher = sha2::Sha256::new();
        hasher.update(bytes);
        Ok(hex::encode(hasher.finalize()))
    }
}

pub mod admission {
    use super::*;
    use ctx_harness_sources::HarnessSourceKind;
    use ctx_sandbox_contract::ContainerNetworkMode;

    pub struct RuntimeTurnAdmissionRequest<'a> {
        pub session: &'a Session,
        pub run_id: RunId,
        pub provider_id: &'a str,
        pub model_id: &'a str,
        pub execution_environment: ExecutionEnvironment,
        pub container_network_mode: ContainerNetworkMode,
        pub source_kind: HarnessSourceKind,
    }

    #[derive(Debug, Clone, Default)]
    pub struct RuntimeTurnAdmission {
        pub env: HashMap<String, String>,
    }

    pub async fn admit_runtime_turn(
        _global_store: &Store,
        _store: &Store,
        request: RuntimeTurnAdmissionRequest<'_>,
    ) -> Result<RuntimeTurnAdmission> {
        let _ = (
            request.session,
            request.run_id,
            request.provider_id,
            request.model_id,
            request.execution_environment,
            request.container_network_mode,
            request.source_kind,
        );
        Ok(RuntimeTurnAdmission::default())
    }

    pub fn apply_turn_admission_env(
        provider_env: &mut HashMap<String, String>,
        admission: &RuntimeTurnAdmission,
    ) {
        provider_env.extend(admission.env.clone());
    }

    pub async fn update_run_terminal_status(
        store: &Store,
        run_id: Option<RunId>,
        run_status: RunStatus,
    ) {
        let Some(run_id) = run_id else {
            return;
        };
        let completed_at = matches!(
            run_status,
            RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled
        )
        .then(Utc::now);
        if let Err(error) = store
            .update_run_status(run_id, run_status, completed_at)
            .await
        {
            tracing::warn!(run_id = %run_id.0, "failed to update run terminal status: {error:#}");
        }
    }
}
