use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::ids::*;

use super::session::ExecutionEnvironment;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanType {
    FreeLocal,
    Pro,
    Team,
    Enterprise,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OrgMembershipRole {
    Owner,
    Admin,
    Member,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PolicyFeatureState {
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum NetworkProfile {
    LlmOnly,
    Allowlist,
    All,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RouteType {
    CtxManaged,
    CustomerGateway,
    UserOauth,
    UserApiKey,
    UserProviderAccount,
}

impl RouteType {
    pub fn is_personal(self) -> bool {
        matches!(
            self,
            Self::UserOauth | Self::UserApiKey | Self::UserProviderAccount
        )
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveMode {
    LocalOnly,
    AccountPrivate,
    OrgSummary,
    OrgTranscript,
    OrgEvidence,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RequiredExecutionEnvironment {
    Sandbox,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PolicyDecisionSource {
    Local,
    CachedPolicy,
    LivePolicy,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PolicySignatureAlgorithm {
    Hs256,
    Rs256,
    EdDsa,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DaemonEnrollmentStatus {
    Active,
    Revoked,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PolicyDecisionOutcome {
    Granted,
    Denied,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PolicyDenyReason {
    PolicyHardExpired,
    ProviderNotAllowed,
    ModelNotAllowed,
    ExecutionEnvironmentNotAllowed,
    NetworkProfileNotAllowed,
    RouteTypeNotAllowed,
    PersonalRouteNotAllowed,
    DaemonEnrollmentMissing,
    DaemonEnrollmentRevoked,
    PolicySnapshotMissing,
    PolicySignatureInvalid,
    WorkspaceOrgMismatch,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PolicyWindowState {
    Fresh,
    Grace,
    Expired,
}

impl PolicyWindowState {
    pub fn permits_org_run(self) -> bool {
        !matches!(self, Self::Expired)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoutePolicy {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_route_types: Vec<RouteType>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArchivePolicy {
    pub mode: ArchiveMode,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaemonEnrollment {
    pub id: DaemonEnrollmentId,
    pub account_id: AccountId,
    pub org_id: OrgId,
    pub org_membership_id: OrgMembershipId,
    pub membership_role: OrgMembershipRole,
    pub plan_type: PlanType,
    pub status: DaemonEnrollmentStatus,
    pub policy_signature_algorithm: PolicySignatureAlgorithm,
    pub policy_signing_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_policy_snapshot_id: Option<OrgPolicySnapshotId>,
    pub enrolled_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrgPolicySnapshot {
    pub id: OrgPolicySnapshotId,
    pub org_id: OrgId,
    pub policy_version: String,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub grace_expires_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_providers: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub allowed_models: BTreeMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_execution_environment: Option<RequiredExecutionEnvironment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_network_profiles: Vec<NetworkProfile>,
    pub route_policy: RoutePolicy,
    pub archive_policy: ArchivePolicy,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub features: BTreeMap<String, PolicyFeatureState>,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspacePolicyOverlay {
    pub workspace_id: WorkspaceId,
    pub org_id: OrgId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_providers: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub allowed_models: BTreeMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_execution_environment: Option<RequiredExecutionEnvironment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_network_profiles: Option<Vec<NetworkProfile>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_route_types: Option<Vec<RouteType>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub features: BTreeMap<String, PolicyFeatureState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EffectiveWorkspacePolicy {
    pub org_id: OrgId,
    pub policy_snapshot_id: OrgPolicySnapshotId,
    pub policy_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_providers: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub allowed_models: BTreeMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_execution_environment: Option<RequiredExecutionEnvironment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_network_profiles: Vec<NetworkProfile>,
    pub route_policy: RoutePolicy,
    pub archive_policy: ArchivePolicy,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub features: BTreeMap<String, PolicyFeatureState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunGrant {
    pub id: RunGrantId,
    pub run_id: RunId,
    pub session_id: SessionId,
    pub workspace_id: WorkspaceId,
    pub account_id: AccountId,
    pub org_id: OrgId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub membership_role: Option<OrgMembershipRole>,
    pub policy_version: String,
    pub provider_id: String,
    pub model_id: String,
    pub execution_environment: ExecutionEnvironment,
    pub network_profile: NetworkProfile,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_type: Option<RouteType>,
    pub archive_mode: ArchiveMode,
    pub issued_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    pub decision_source: PolicyDecisionSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PolicyDecisionEvent {
    pub id: PolicyDecisionEventId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_grant_id: Option<RunGrantId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<AccountId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_id: Option<OrgId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_snapshot_id: Option<OrgPolicySnapshotId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_version: Option<String>,
    pub decision_source: PolicyDecisionSource,
    pub outcome: PolicyDecisionOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deny_reason: Option<PolicyDenyReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_execution_environment: Option<ExecutionEnvironment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_network_profile: Option<NetworkProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_route_type: Option<RouteType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub created_at: DateTime<Utc>,
}

mod evaluation;
mod merge;
#[cfg(test)]
mod tests;

pub use evaluation::*;
pub use merge::*;
