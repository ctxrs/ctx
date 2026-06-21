use chrono::{DateTime, Utc};
use ctx_core::ids::{
    WorkEventId, WorkEvidenceId, WorkRecordId, WorkRecordLinkId, WorkSummaryClaimId, WorkSummaryId,
    WorkspaceAttachmentId, WorkspaceId, WorktreeId,
};
use ctx_core::models::{
    AttachmentMode, AttachmentUpdatePolicy, ChangeSet, Contribution, RecordFidelity, RecordSource,
    RecordTrust, VcsKind, WorkActorKind, WorkEventType, WorkEvidenceFreshness, WorkEvidenceKind,
    WorkEvidenceStatus, WorkLifecycle, WorkLinkRole, WorkLinkTargetKind, WorkRedactionClass,
    WorkSummaryAudience, WorkSummaryFreshness, WorkSummaryGenerationMethod, WorkSummaryKind,
    WorkTrustVerdict, Workspace, WorkspaceActiveHeadBatch, WorkspaceActiveSnapshot,
    WorkspaceAttachment, WorkspaceAttachmentKind, WorkspaceAttachmentStatus, Worktree,
    WorktreeBootstrapStatus,
};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceRouteResponse {
    pub id: WorkspaceId,
    pub name: String,
    pub root_path: String,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcs_kind: Option<VcsKind>,
}

impl From<Workspace> for WorkspaceRouteResponse {
    fn from(workspace: Workspace) -> Self {
        Self {
            id: workspace.id,
            name: workspace.name,
            root_path: workspace.root_path,
            created_at: workspace.created_at,
            vcs_kind: workspace.vcs_kind,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct WorktreeRouteResponse {
    pub id: WorktreeId,
    pub workspace_id: WorkspaceId,
    pub root_path: String,
    pub base_commit_sha: String,
    pub git_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcs_kind: Option<VcsKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcs_ref: Option<String>,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap_status: Option<WorktreeBootstrapStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap_started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap_finished_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap_exit_code: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap_timeout_sec: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap_log_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap_log_truncated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap_script_path: Option<String>,
}

impl From<Worktree> for WorktreeRouteResponse {
    fn from(worktree: Worktree) -> Self {
        Self {
            id: worktree.id,
            workspace_id: worktree.workspace_id,
            root_path: worktree.root_path,
            base_commit_sha: worktree.base_commit_sha,
            git_branch: worktree.git_branch,
            vcs_kind: worktree.vcs_kind,
            base_revision: worktree.base_revision,
            vcs_ref: worktree.vcs_ref,
            created_at: worktree.created_at,
            bootstrap_status: worktree.bootstrap_status,
            bootstrap_started_at: worktree.bootstrap_started_at,
            bootstrap_finished_at: worktree.bootstrap_finished_at,
            bootstrap_exit_code: worktree.bootstrap_exit_code,
            bootstrap_timeout_sec: worktree.bootstrap_timeout_sec,
            bootstrap_error: worktree.bootstrap_error,
            bootstrap_log_path: worktree.bootstrap_log_path,
            bootstrap_log_truncated: worktree.bootstrap_log_truncated,
            bootstrap_command: worktree.bootstrap_command,
            bootstrap_script_path: worktree.bootstrap_script_path,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceAttachmentRouteResponse {
    pub id: WorkspaceAttachmentId,
    pub workspace_id: WorkspaceId,
    pub kind: WorkspaceAttachmentKind,
    pub name: String,
    pub source: String,
    pub revision: Option<String>,
    pub subpath: Option<String>,
    pub mount_relpath: String,
    pub mode: AttachmentMode,
    pub update_policy: AttachmentUpdatePolicy,
    pub status: WorkspaceAttachmentStatus,
    pub last_sync_at: Option<DateTime<Utc>>,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<WorkspaceAttachment> for WorkspaceAttachmentRouteResponse {
    fn from(attachment: WorkspaceAttachment) -> Self {
        Self {
            id: attachment.id,
            workspace_id: attachment.workspace_id,
            kind: attachment.kind,
            name: attachment.name,
            source: attachment.source,
            revision: attachment.revision,
            subpath: attachment.subpath,
            mount_relpath: attachment.mount_relpath,
            mode: attachment.mode,
            update_policy: attachment.update_policy,
            status: attachment.status,
            last_sync_at: attachment.last_sync_at,
            error_message: attachment.error_message,
            created_at: attachment.created_at,
            updated_at: attachment.updated_at,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceHarnessContainerMountModeRouteValue {
    DiskIsolated,
    Legacy,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceHarnessContainerNetworkModeRouteValue {
    LlmOnly,
    Allowlist,
    All,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceHarnessContainerStatusRouteResponse {
    pub name: String,
    pub running: bool,
    pub known: bool,
    pub mount_mode: Option<WorkspaceHarnessContainerMountModeRouteValue>,
    pub network_mode: Option<WorkspaceHarnessContainerNetworkModeRouteValue>,
    pub allowlist: Vec<String>,
    pub egress_guard: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceAgentWorkRouteResponse {
    pub change_sets: Vec<ChangeSet>,
    pub contributions: Vec<Contribution>,
}

impl WorkspaceAgentWorkRouteResponse {
    pub fn new(change_sets: Vec<ChangeSet>, contributions: Vec<Contribution>) -> Self {
        Self {
            change_sets,
            contributions,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct WorkspaceAgentWorkRouteQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_set_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_json: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct WorkspaceWorkListRouteQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct WorkspaceWorkContextRouteQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct WorkspaceWorkTimelineRouteQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceWorkListRouteResponse {
    pub work: Vec<WorkspaceWorkRecordRouteItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceWorkDetailRouteResponse {
    pub work: WorkspaceWorkRecordRouteItem,
    pub links: Vec<WorkspaceWorkLinkRouteItem>,
    pub evidence: Vec<WorkspaceWorkEvidenceRouteItem>,
    pub summaries: Vec<WorkspaceWorkSummaryRouteItem>,
    pub summary_claims: Vec<WorkspaceWorkSummaryClaimRouteItem>,
    pub duplicate_strong_links: Vec<WorkspaceWorkDuplicateStrongLinkRouteItem>,
    pub raw_detail_included: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceWorkTimelineRouteResponse {
    pub work_id: WorkRecordId,
    pub events: Vec<WorkspaceWorkEventRouteItem>,
    pub raw_transcript_included: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceWorkEvidenceRouteResponse {
    pub work_id: WorkRecordId,
    pub evidence: Vec<WorkspaceWorkEvidenceRouteItem>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct WorkspaceWorkEvidenceCreateRouteRequest {
    #[serde(default)]
    pub kind: WorkEvidenceKind,
    #[serde(default)]
    pub status: WorkEvidenceStatus,
    #[serde(default)]
    pub freshness: WorkEvidenceFreshness,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_ref: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<Value>,
    #[serde(default)]
    pub source: RecordSource,
    #[serde(default)]
    pub fidelity: RecordFidelity,
    #[serde(default)]
    pub trust: RecordTrust,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceWorkEvidenceCreateRouteResponse {
    pub work_id: WorkRecordId,
    pub evidence: WorkspaceWorkEvidenceRouteItem,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct WorkspaceWorkSummaryClaimCreateRouteRequest {
    pub claim_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_kind: Option<String>,
    #[serde(default)]
    pub source_kind: Option<String>,
    #[serde(default)]
    pub source_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_hash: Option<String>,
    #[serde(default)]
    pub freshness: WorkSummaryFreshness,
    #[serde(default)]
    pub redaction_class: WorkRedactionClass,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct WorkspaceWorkSummaryCreateRouteRequest {
    #[serde(default)]
    pub kind: WorkSummaryKind,
    #[serde(default)]
    pub audience: WorkSummaryAudience,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_json: Option<Value>,
    #[serde(default)]
    pub generation_method: WorkSummaryGenerationMethod,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    #[serde(default)]
    pub source_material_left_machine: bool,
    #[serde(default)]
    pub freshness: WorkSummaryFreshness,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_revision_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub claims: Vec<WorkspaceWorkSummaryClaimCreateRouteRequest>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceWorkSummaryCreateRouteResponse {
    pub work_id: WorkRecordId,
    pub summary: WorkspaceWorkSummaryRouteItem,
    pub claims: Vec<WorkspaceWorkSummaryClaimRouteItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceWorkTrustRouteSummary {
    pub verdict: WorkTrustVerdict,
    pub reason: String,
    pub recommended_next_action: String,
    pub open_risks: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceWorkEvidenceSummaryRouteResponse {
    pub total: usize,
    pub passing: usize,
    pub failing: usize,
    pub stale: usize,
    pub missing: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceWorkChangeSummaryRouteResponse {
    pub change_sets: usize,
    pub contributions: usize,
    pub pull_requests: Vec<Value>,
    pub commits: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceWorkSafeJsonRouteResponse {
    pub value: Value,
    pub redacted: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redaction_notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceWorkTranscriptItemRouteResponse {
    pub event_id: WorkEventId,
    pub sequence: i64,
    pub event_type: WorkEventType,
    pub event_time: DateTime<Utc>,
    pub actor_kind: WorkActorKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub redaction_class: WorkRedactionClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_preview: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceWorkCommandPreviewRouteResponse {
    pub evidence_id: WorkEvidenceId,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub status: WorkEvidenceStatus,
    pub freshness: WorkEvidenceFreshness,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_preview: Option<String>,
    #[serde(default)]
    pub output_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_limit_bytes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_size_bytes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_size_bytes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_sha256: Option<String>,
    #[serde(default)]
    pub stdout_truncated: bool,
    #[serde(default)]
    pub stderr_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_ref: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceWorkArtifactRenderKind {
    RasterImage,
    Text,
    DownloadOnly,
    Unavailable,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceWorkArtifactRouteItem {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<i64>,
    #[serde(default)]
    pub missing: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
    pub render_kind: WorkspaceWorkArtifactRenderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceWorkArtifactSummaryRouteResponse {
    pub total: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<WorkspaceWorkSafeJsonRouteResponse>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceWorkTimelineItemRouteResponse {
    pub sequence: i64,
    pub event_time: DateTime<Utc>,
    pub kind: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_event_id: Option<WorkEventId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_evidence_id: Option<WorkEvidenceId>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceWorkInspectorOverviewRouteResponse {
    pub title: Option<String>,
    pub objective: Option<String>,
    pub lifecycle: WorkLifecycle,
    pub primary_branch: Option<String>,
    pub base_commit: Option<String>,
    pub head_commit: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceWorkInspectorRouteResponse {
    pub work: WorkspaceWorkRecordRouteItem,
    pub links: Vec<WorkspaceWorkLinkRouteItem>,
    pub overview: WorkspaceWorkInspectorOverviewRouteResponse,
    pub trust: WorkspaceWorkTrustRouteSummary,
    pub context: WorkspaceWorkSafeJsonRouteResponse,
    pub safe_json: WorkspaceWorkSafeJsonRouteResponse,
    pub raw_redacted_json: WorkspaceWorkSafeJsonRouteResponse,
    pub evidence_summary: WorkspaceWorkEvidenceSummaryRouteResponse,
    pub change_summary: WorkspaceWorkChangeSummaryRouteResponse,
    pub artifact_summary: WorkspaceWorkArtifactSummaryRouteResponse,
    pub transcript: Vec<WorkspaceWorkTranscriptItemRouteResponse>,
    pub commands: Vec<WorkspaceWorkCommandPreviewRouteResponse>,
    pub artifacts: Vec<WorkspaceWorkArtifactRouteItem>,
    pub evidence: Vec<WorkspaceWorkEvidenceRouteItem>,
    pub change_sets: Vec<Value>,
    pub contributions: Vec<Value>,
    pub summaries: Vec<WorkspaceWorkSummaryRouteItem>,
    pub summary_claims: Vec<WorkspaceWorkSummaryClaimRouteItem>,
    pub timeline: Vec<WorkspaceWorkEventRouteItem>,
    pub timeline_items: Vec<WorkspaceWorkTimelineItemRouteResponse>,
    pub duplicate_strong_links: Vec<WorkspaceWorkDuplicateStrongLinkRouteItem>,
    pub raw_transcript_available: bool,
    pub raw_transcript_included: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceWorkReportRouteResponse {
    pub work: WorkspaceWorkRecordRouteItem,
    pub links: Vec<WorkspaceWorkLinkRouteItem>,
    pub trust: WorkspaceWorkTrustRouteSummary,
    pub evidence_summary: WorkspaceWorkEvidenceSummaryRouteResponse,
    pub evidence: Vec<WorkspaceWorkEvidenceRouteItem>,
    pub change_summary: WorkspaceWorkChangeSummaryRouteResponse,
    pub change_sets: Vec<Value>,
    pub contributions: Vec<Value>,
    pub summaries: Vec<WorkspaceWorkSummaryRouteItem>,
    pub summary_claims: Vec<WorkspaceWorkSummaryClaimRouteItem>,
    pub timeline: Vec<WorkspaceWorkEventRouteItem>,
    pub duplicate_strong_links: Vec<WorkspaceWorkDuplicateStrongLinkRouteItem>,
    pub raw_transcript_available: bool,
    pub raw_transcript_included: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceWorkContextRouteResponse {
    pub work_id: WorkRecordId,
    pub budget_tokens: usize,
    pub title: Option<String>,
    pub state: String,
    pub trust_verdict: WorkTrustVerdict,
    pub summary_freshness: WorkSummaryFreshness,
    pub context: Value,
    pub raw_transcript_available: bool,
    pub raw_transcript_included: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceWorkRecordRouteItem {
    pub work_id: WorkRecordId,
    pub workspace_id: WorkspaceId,
    pub title: Option<String>,
    pub objective: Option<String>,
    pub lifecycle: WorkLifecycle,
    pub primary_branch: Option<String>,
    pub base_commit: Option<String>,
    pub head_commit: Option<String>,
    pub trust_verdict: WorkTrustVerdict,
    pub summary_freshness: WorkSummaryFreshness,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub schema_version: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceWorkLinkRouteItem {
    pub link_id: WorkRecordLinkId,
    pub work_id: WorkRecordId,
    pub workspace_id: WorkspaceId,
    pub target_kind: WorkLinkTargetKind,
    pub target_id: Option<String>,
    pub target_json: Option<Value>,
    pub role: WorkLinkRole,
    pub source: RecordSource,
    pub fidelity: RecordFidelity,
    pub trust: RecordTrust,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub schema_version: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceWorkEventRouteItem {
    pub event_id: WorkEventId,
    pub work_id: WorkRecordId,
    pub workspace_id: WorkspaceId,
    pub sequence: i64,
    pub source_kind: Option<String>,
    pub source_id: Option<String>,
    pub event_type: WorkEventType,
    pub event_time: DateTime<Utc>,
    pub actor_kind: WorkActorKind,
    pub provider: Option<String>,
    pub harness: Option<String>,
    pub model: Option<String>,
    pub redaction_class: WorkRedactionClass,
    pub source: RecordSource,
    pub fidelity: RecordFidelity,
    pub trust: RecordTrust,
    pub redacted_text: Option<String>,
    pub created_at: DateTime<Utc>,
    pub schema_version: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceWorkEvidenceRouteItem {
    pub evidence_id: WorkEvidenceId,
    pub work_id: WorkRecordId,
    pub workspace_id: WorkspaceId,
    pub kind: WorkEvidenceKind,
    pub status: WorkEvidenceStatus,
    pub freshness: WorkEvidenceFreshness,
    pub claim: Option<String>,
    pub command: Option<String>,
    pub argv: Vec<String>,
    pub cwd: Option<String>,
    pub exit_code: Option<i32>,
    pub head_sha: Option<String>,
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_ref: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<Value>,
    pub source: RecordSource,
    pub fidelity: RecordFidelity,
    pub trust: RecordTrust,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub schema_version: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceWorkSummaryRouteItem {
    pub summary_id: WorkSummaryId,
    pub work_id: WorkRecordId,
    pub workspace_id: WorkspaceId,
    pub kind: WorkSummaryKind,
    pub audience: WorkSummaryAudience,
    pub text: String,
    pub structured_json: Option<Value>,
    pub generation_method: WorkSummaryGenerationMethod,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub template: Option<String>,
    pub source_material_left_machine: bool,
    pub freshness: WorkSummaryFreshness,
    pub source_revision_key: Option<String>,
    pub generated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub schema_version: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceWorkSummaryClaimRouteItem {
    pub claim_id: WorkSummaryClaimId,
    pub summary_id: WorkSummaryId,
    pub work_id: WorkRecordId,
    pub workspace_id: WorkspaceId,
    pub claim_text: String,
    pub claim_kind: Option<String>,
    pub source_kind: String,
    pub source_id: String,
    pub record_hash: Option<String>,
    pub freshness: WorkSummaryFreshness,
    pub redaction_class: WorkRedactionClass,
    pub created_at: DateTime<Utc>,
    pub schema_version: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceWorkDuplicateStrongLinkRouteItem {
    pub target_kind: WorkLinkTargetKind,
    pub target_id: String,
    pub work_ids: Vec<WorkRecordId>,
}

#[derive(Debug, Clone)]
pub struct WorkspaceActiveSnapshotRouteResponse {
    value: WorkspaceActiveSnapshot,
}

impl From<WorkspaceActiveSnapshot> for WorkspaceActiveSnapshotRouteResponse {
    fn from(value: WorkspaceActiveSnapshot) -> Self {
        Self { value }
    }
}

impl Serialize for WorkspaceActiveSnapshotRouteResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.value.serialize(serializer)
    }
}

#[derive(Debug, Clone)]
pub struct WorkspaceActiveHeadBatchRouteResponse {
    value: WorkspaceActiveHeadBatch,
}

impl From<WorkspaceActiveHeadBatch> for WorkspaceActiveHeadBatchRouteResponse {
    fn from(value: WorkspaceActiveHeadBatch) -> Self {
        Self { value }
    }
}

impl Serialize for WorkspaceActiveHeadBatchRouteResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.value.serialize(serializer)
    }
}
