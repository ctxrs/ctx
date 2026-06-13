use ctx_core::ids::{OrgId, WorkspaceId};
use ctx_core::models::{
    DaemonEnrollment, DaemonEnrollmentStatus, OrgMembershipRole, OrgPolicySnapshot, PlanType,
    PolicySignatureAlgorithm, WorkspacePolicyOverlay,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct OrgPolicyOrgRouteParams {
    org_id: String,
}

impl OrgPolicyOrgRouteParams {
    pub fn new(org_id: impl Into<String>) -> Self {
        Self {
            org_id: org_id.into(),
        }
    }

    pub fn parse(&self) -> Result<OrgId, OrgPolicyRouteError> {
        uuid::Uuid::parse_str(&self.org_id)
            .map(OrgId)
            .map_err(|_| OrgPolicyRouteError::bad_request("invalid org id"))
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct OrgPolicyWorkspaceRouteParams {
    workspace_id: String,
}

impl OrgPolicyWorkspaceRouteParams {
    pub fn new(workspace_id: impl Into<String>) -> Self {
        Self {
            workspace_id: workspace_id.into(),
        }
    }

    pub fn parse(&self) -> Result<WorkspaceId, OrgPolicyRouteError> {
        uuid::Uuid::parse_str(&self.workspace_id)
            .map(WorkspaceId)
            .map_err(|_| OrgPolicyRouteError::bad_request("invalid workspace id"))
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(transparent)]
pub struct UpsertDaemonEnrollmentRouteRequest(DaemonEnrollment);

impl UpsertDaemonEnrollmentRouteRequest {
    pub fn new(enrollment: DaemonEnrollment) -> Self {
        Self(enrollment)
    }

    pub fn into_inner(self) -> DaemonEnrollment {
        self.0
    }
}

impl From<DaemonEnrollment> for UpsertDaemonEnrollmentRouteRequest {
    fn from(enrollment: DaemonEnrollment) -> Self {
        Self::new(enrollment)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(transparent)]
pub struct CacheOrgPolicySnapshotRouteRequest(OrgPolicySnapshot);

impl CacheOrgPolicySnapshotRouteRequest {
    pub fn new(snapshot: OrgPolicySnapshot) -> Self {
        Self(snapshot)
    }

    pub fn into_inner(self) -> OrgPolicySnapshot {
        self.0
    }
}

impl From<OrgPolicySnapshot> for CacheOrgPolicySnapshotRouteRequest {
    fn from(snapshot: OrgPolicySnapshot) -> Self {
        Self::new(snapshot)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(transparent)]
pub struct UpsertWorkspacePolicyOverlayRouteRequest(WorkspacePolicyOverlay);

impl UpsertWorkspacePolicyOverlayRouteRequest {
    pub fn new(overlay: WorkspacePolicyOverlay) -> Self {
        Self(overlay)
    }

    pub fn into_inner(self) -> WorkspacePolicyOverlay {
        self.0
    }
}

impl From<WorkspacePolicyOverlay> for UpsertWorkspacePolicyOverlayRouteRequest {
    fn from(overlay: WorkspacePolicyOverlay) -> Self {
        Self::new(overlay)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DaemonEnrollmentRouteResponse {
    id: ctx_core::ids::DaemonEnrollmentId,
    account_id: ctx_core::ids::AccountId,
    org_id: OrgId,
    org_membership_id: ctx_core::ids::OrgMembershipId,
    membership_role: OrgMembershipRole,
    plan_type: PlanType,
    status: DaemonEnrollmentStatus,
    policy_signature_algorithm: PolicySignatureAlgorithm,
    policy_signing_key_present: bool,
    active_policy_snapshot_id: Option<ctx_core::ids::OrgPolicySnapshotId>,
    enrolled_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    revoked_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl From<DaemonEnrollment> for DaemonEnrollmentRouteResponse {
    fn from(enrollment: DaemonEnrollment) -> Self {
        Self {
            id: enrollment.id,
            account_id: enrollment.account_id,
            org_id: enrollment.org_id,
            org_membership_id: enrollment.org_membership_id,
            membership_role: enrollment.membership_role,
            plan_type: enrollment.plan_type,
            status: enrollment.status,
            policy_signature_algorithm: enrollment.policy_signature_algorithm,
            policy_signing_key_present: !enrollment.policy_signing_key.trim().is_empty(),
            active_policy_snapshot_id: enrollment.active_policy_snapshot_id,
            enrolled_at: enrollment.enrolled_at,
            updated_at: enrollment.updated_at,
            revoked_at: enrollment.revoked_at,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct DaemonEnrollmentsRouteResponse(Vec<DaemonEnrollmentRouteResponse>);

impl DaemonEnrollmentsRouteResponse {
    pub fn new(enrollments: Vec<DaemonEnrollmentRouteResponse>) -> Self {
        Self(enrollments)
    }
}

impl From<Vec<DaemonEnrollmentRouteResponse>> for DaemonEnrollmentsRouteResponse {
    fn from(enrollments: Vec<DaemonEnrollmentRouteResponse>) -> Self {
        Self::new(enrollments)
    }
}

impl From<Vec<DaemonEnrollment>> for DaemonEnrollmentsRouteResponse {
    fn from(enrollments: Vec<DaemonEnrollment>) -> Self {
        Self::new(
            enrollments
                .into_iter()
                .map(DaemonEnrollmentRouteResponse::from)
                .collect(),
        )
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct OrgPolicySnapshotRouteResponse(OrgPolicySnapshot);

impl From<OrgPolicySnapshot> for OrgPolicySnapshotRouteResponse {
    fn from(snapshot: OrgPolicySnapshot) -> Self {
        Self(snapshot)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct WorkspacePolicyOverlayRouteResponse(WorkspacePolicyOverlay);

impl From<WorkspacePolicyOverlay> for WorkspacePolicyOverlayRouteResponse {
    fn from(overlay: WorkspacePolicyOverlay) -> Self {
        Self(overlay)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct WorkspacePolicyOverlayOptionalRouteResponse(Option<WorkspacePolicyOverlay>);

impl From<Option<WorkspacePolicyOverlay>> for WorkspacePolicyOverlayOptionalRouteResponse {
    fn from(overlay: Option<WorkspacePolicyOverlay>) -> Self {
        Self(overlay)
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum OrgPolicyRouteErrorKind {
    BadRequest,
    Conflict,
    NotFound,
    Internal,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct OrgPolicyRouteError {
    kind: OrgPolicyRouteErrorKind,
    message: String,
}

impl OrgPolicyRouteError {
    pub fn new(kind: OrgPolicyRouteErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(OrgPolicyRouteErrorKind::BadRequest, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(OrgPolicyRouteErrorKind::Conflict, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(OrgPolicyRouteErrorKind::NotFound, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(OrgPolicyRouteErrorKind::Internal, message)
    }

    pub fn kind(&self) -> OrgPolicyRouteErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use ctx_core::ids::{AccountId, DaemonEnrollmentId, OrgMembershipId, OrgPolicySnapshotId};
    use ctx_core::models::{
        ArchiveMode, ArchivePolicy, NetworkProfile, PolicyFeatureState, RoutePolicy, RouteType,
    };
    use serde_json::json;
    use std::collections::BTreeMap;

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

    #[test]
    fn route_params_parse_ids_and_classify_invalid_values() {
        let org_id = OrgId::new();
        assert_eq!(
            OrgPolicyOrgRouteParams::new(org_id.0.to_string())
                .parse()
                .unwrap(),
            org_id
        );
        let org_error = OrgPolicyOrgRouteParams::new("not-an-org")
            .parse()
            .unwrap_err();
        assert_eq!(org_error.kind(), OrgPolicyRouteErrorKind::BadRequest);
        assert_eq!(org_error.message(), "invalid org id");

        let workspace_id = WorkspaceId::new();
        assert_eq!(
            OrgPolicyWorkspaceRouteParams::new(workspace_id.0.to_string())
                .parse()
                .unwrap(),
            workspace_id
        );
        let workspace_error = OrgPolicyWorkspaceRouteParams::new("not-a-workspace")
            .parse()
            .unwrap_err();
        assert_eq!(workspace_error.kind(), OrgPolicyRouteErrorKind::BadRequest);
        assert_eq!(workspace_error.message(), "invalid workspace id");
    }

    #[test]
    fn route_request_wrappers_preserve_unknown_field_compatibility() {
        let org_id = OrgId::new();
        let workspace_id = WorkspaceId::new();

        let enrollment_value = serde_json::to_value(enrollment(org_id)).expect("enrollment json");
        let mut enrollment_object = enrollment_value.as_object().expect("object").clone();
        enrollment_object.insert("unknown_field".to_string(), json!("ignored"));
        serde_json::from_value::<UpsertDaemonEnrollmentRouteRequest>(json!(enrollment_object))
            .expect("enrollment route request allows unknown fields");

        let snapshot_value = serde_json::to_value(snapshot(org_id)).expect("snapshot json");
        let mut snapshot_object = snapshot_value.as_object().expect("object").clone();
        snapshot_object.insert("unknown_field".to_string(), json!("ignored"));
        serde_json::from_value::<CacheOrgPolicySnapshotRouteRequest>(json!(snapshot_object))
            .expect("snapshot route request allows unknown fields");

        let overlay_value =
            serde_json::to_value(overlay(workspace_id, org_id)).expect("overlay json");
        let mut overlay_object = overlay_value.as_object().expect("object").clone();
        overlay_object.insert("unknown_field".to_string(), json!("ignored"));
        serde_json::from_value::<UpsertWorkspacePolicyOverlayRouteRequest>(json!(overlay_object))
            .expect("overlay route request allows unknown fields");
    }

    #[test]
    fn route_request_wrappers_expose_route_neutral_constructors() {
        let org_id = OrgId::new();
        let enrollment = enrollment(org_id);
        assert_eq!(
            UpsertDaemonEnrollmentRouteRequest::new(enrollment.clone()).into_inner(),
            enrollment
        );

        let snapshot = snapshot(org_id);
        assert_eq!(
            CacheOrgPolicySnapshotRouteRequest::from(snapshot.clone()).into_inner(),
            snapshot
        );

        let overlay = overlay(WorkspaceId::new(), org_id);
        assert_eq!(
            UpsertWorkspacePolicyOverlayRouteRequest::from(overlay.clone()).into_inner(),
            overlay
        );
    }

    #[test]
    fn enrollment_route_response_redacts_signing_key() {
        let response = DaemonEnrollmentRouteResponse::from(enrollment(OrgId::new()));
        let value = serde_json::to_value(response).expect("response json");
        assert!(value.get("policy_signing_key").is_none());
        assert_eq!(
            value
                .get("policy_signing_key_present")
                .and_then(|value| value.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn route_response_wrappers_preserve_json_shapes() {
        let org_id = OrgId::new();
        let enrollment_response = DaemonEnrollmentRouteResponse::from(enrollment(org_id));
        let list = DaemonEnrollmentsRouteResponse::new(vec![enrollment_response]);
        assert!(serde_json::to_value(list).unwrap().is_array());

        let snapshot_value =
            serde_json::to_value(OrgPolicySnapshotRouteResponse::from(snapshot(org_id))).unwrap();
        let org_id_text = org_id.0.to_string();
        assert_eq!(
            snapshot_value
                .get("org_id")
                .and_then(|value| value.as_str()),
            Some(org_id_text.as_str())
        );

        let workspace_id = WorkspaceId::new();
        let overlay_value = serde_json::to_value(WorkspacePolicyOverlayRouteResponse::from(
            overlay(workspace_id, org_id),
        ))
        .unwrap();
        let workspace_id_text = workspace_id.0.to_string();
        assert_eq!(
            overlay_value
                .get("workspace_id")
                .and_then(|value| value.as_str()),
            Some(workspace_id_text.as_str())
        );

        assert_eq!(
            serde_json::to_value(WorkspacePolicyOverlayOptionalRouteResponse::from(None)).unwrap(),
            serde_json::Value::Null
        );
    }
}
