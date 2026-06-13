use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ids::*;

use super::{
    ArchiveVisibility, AuditActor, AuditEventKind, MessageRole, RetentionPolicyRef,
    RunArchiveState, RunStatus, SessionEvent, SessionEventType,
};

mod normalize;

pub use normalize::*;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunArchiveIngestScope {
    None,
    Summary,
    Transcript,
    Evidence,
}

impl RunArchiveIngestScope {
    pub fn from_visibility(visibility: ArchiveVisibility) -> Self {
        match visibility {
            ArchiveVisibility::LocalOnly | ArchiveVisibility::AccountPrivate => Self::None,
            ArchiveVisibility::OrgSummary => Self::Summary,
            ArchiveVisibility::OrgTranscript => Self::Transcript,
            ArchiveVisibility::OrgEvidence => Self::Evidence,
        }
    }

    pub fn includes_transcript(self) -> bool {
        matches!(self, Self::Transcript | Self::Evidence)
    }

    pub fn includes_evidence_payloads(self) -> bool {
        matches!(self, Self::Evidence)
    }

    pub fn is_cloud_visible(self) -> bool {
        !matches!(self, Self::None)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Summary => "summary",
            Self::Transcript => "transcript",
            Self::Evidence => "evidence",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunArchiveIngestWatermark {
    pub session_event_seq: i64,
    pub audit_event_seq: i64,
}

impl RunArchiveIngestWatermark {
    pub fn advance_to(&mut self, other: Self) {
        self.session_event_seq = self.session_event_seq.max(other.session_event_seq);
        self.audit_event_seq = self.audit_event_seq.max(other.audit_event_seq);
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunArchiveNormalizationStats {
    pub redacted_absolute_paths: u32,
    pub redacted_secret_fields: u32,
    pub redacted_secret_values: u32,
    pub redacted_provider_refs: u32,
    pub redacted_pty_streams: u32,
    pub dropped_transient_events: u32,
    pub omitted_content_payloads: u32,
}

impl RunArchiveNormalizationStats {
    pub fn merge(&mut self, other: Self) {
        self.redacted_absolute_paths += other.redacted_absolute_paths;
        self.redacted_secret_fields += other.redacted_secret_fields;
        self.redacted_secret_values += other.redacted_secret_values;
        self.redacted_provider_refs += other.redacted_provider_refs;
        self.redacted_pty_streams += other.redacted_pty_streams;
        self.dropped_transient_events += other.dropped_transient_events;
        self.omitted_content_payloads += other.omitted_content_payloads;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunArchiveIngestCursor {
    pub run_id: RunId,
    pub workspace_id: WorkspaceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_id: Option<OrgId>,
    pub archive_visibility: ArchiveVisibility,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_policy: Option<RetentionPolicyRef>,
    pub watermark: RunArchiveIngestWatermark,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_batch_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_synced_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunArchiveIngestBatch {
    pub idempotency_key: String,
    pub run: RunArchiveIngestRun,
    pub scope: RunArchiveIngestScope,
    pub from: RunArchiveIngestWatermark,
    pub to: RunArchiveIngestWatermark,
    pub messages: Vec<RunArchiveIngestMessage>,
    pub session_events: Vec<RunArchiveIngestSessionEvent>,
    pub audit_events: Vec<RunArchiveIngestAuditEvent>,
    pub normalization: RunArchiveNormalizationStats,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunArchiveIngestRun {
    pub id: RunId,
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub workspace_id: WorkspaceId,
    pub worktree_id: WorktreeId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<AccountId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_id: Option<OrgId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_grant_id: Option<RunGrantId>,
    pub status: RunStatus,
    pub archive_state: RunArchiveState,
    pub archive_visibility: ArchiveVisibility,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_policy: Option<RetentionPolicyRef>,
    pub provider_id: String,
    pub model_id: String,
    pub execution_environment: String,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunArchiveIngestMessage {
    pub id: MessageId,
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub run_id: RunId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_sequence: Option<i64>,
    pub role: MessageRole,
    pub content: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunArchiveIngestSessionEvent {
    pub seq: i64,
    pub id: SessionEventId,
    pub session_id: SessionId,
    pub run_id: RunId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    pub event_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_json: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_omitted_reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunArchiveIngestAuditEvent {
    pub ingest_seq: i64,
    pub id: String,
    pub workspace_id: WorkspaceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<AccountId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_id: Option<OrgId>,
    pub actor: AuditActor,
    pub event_kind: AuditEventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_visibility: Option<ArchiveVisibility>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_policy: Option<RetentionPolicyRef>,
    pub payload_json: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NormalizedArchivePayload {
    pub value: Value,
    pub stats: RunArchiveNormalizationStats,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NormalizedArchiveText {
    pub text: String,
    pub stats: RunArchiveNormalizationStats,
}
