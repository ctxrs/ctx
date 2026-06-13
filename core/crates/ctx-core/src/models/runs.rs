use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::*;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Queued,
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "paused" => Some(Self::Paused),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RunArchiveState {
    #[default]
    Active,
    Archived,
}

impl RunArchiveState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Archived => "archived",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "active" => Some(Self::Active),
            "archived" => Some(Self::Archived),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveVisibility {
    #[default]
    LocalOnly,
    AccountPrivate,
    OrgSummary,
    OrgTranscript,
    OrgEvidence,
}

impl ArchiveVisibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LocalOnly => "local_only",
            Self::AccountPrivate => "account_private",
            Self::OrgSummary => "org_summary",
            Self::OrgTranscript => "org_transcript",
            Self::OrgEvidence => "org_evidence",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "local_only" => Some(Self::LocalOnly),
            "account_private" => Some(Self::AccountPrivate),
            "org_summary" => Some(Self::OrgSummary),
            "org_transcript" => Some(Self::OrgTranscript),
            "org_evidence" => Some(Self::OrgEvidence),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RetentionPolicyRef {
    pub policy_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legal_hold_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunRecord {
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
    #[serde(default)]
    pub archive_state: RunArchiveState,
    #[serde(default)]
    pub archive_visibility: ArchiveVisibility,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_policy: Option<RetentionPolicyRef>,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuditActorKind {
    System,
    Account,
    Organization,
}

impl AuditActorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Account => "account",
            Self::Organization => "organization",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "system" => Some(Self::System),
            "account" => Some(Self::Account),
            "organization" => Some(Self::Organization),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditActor {
    pub kind: AuditActorKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<AccountId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_id: Option<OrgId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub membership_role: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuditEventKind {
    RunCreated,
    ArchiveStateChanged,
    ArchiveVisibilityChanged,
    RetentionPolicyChanged,
    HistoryAccessed,
}

impl AuditEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RunCreated => "run_created",
            Self::ArchiveStateChanged => "archive_state_changed",
            Self::ArchiveVisibilityChanged => "archive_visibility_changed",
            Self::RetentionPolicyChanged => "retention_policy_changed",
            Self::HistoryAccessed => "history_accessed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "run_created" => Some(Self::RunCreated),
            "archive_state_changed" => Some(Self::ArchiveStateChanged),
            "archive_visibility_changed" => Some(Self::ArchiveVisibilityChanged),
            "retention_policy_changed" => Some(Self::RetentionPolicyChanged),
            "history_accessed" => Some(Self::HistoryAccessed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuditEvent {
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
    pub payload_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn archive_visibility_serializes_snake_case() {
        let value = serde_json::to_string(&ArchiveVisibility::OrgTranscript).unwrap();
        assert_eq!(value, "\"org_transcript\"");
    }

    #[test]
    fn run_record_archive_state_is_separate_from_visibility() {
        let now = Utc::now();
        let run = RunRecord {
            id: RunId::new(),
            session_id: SessionId::new(),
            task_id: TaskId::new(),
            workspace_id: WorkspaceId::new(),
            worktree_id: WorktreeId::new(),
            parent_run_id: None,
            account_id: Some(AccountId::new()),
            org_id: Some(OrgId::new()),
            run_grant_id: Some(RunGrantId::new()),
            status: RunStatus::Completed,
            archive_state: RunArchiveState::Archived,
            archive_visibility: ArchiveVisibility::AccountPrivate,
            retention_policy: Some(RetentionPolicyRef {
                policy_key: "team-default".into(),
                legal_hold_key: Some("case-42".into()),
            }),
            created_at: now,
            started_at: Some(now),
            completed_at: Some(now),
            archived_at: Some(now),
            updated_at: now,
        };

        let json = serde_json::to_value(&run).unwrap();
        assert_eq!(json.get("archive_state"), Some(&json!("archived")));
        assert_eq!(
            json.get("archive_visibility"),
            Some(&json!("account_private"))
        );
        assert_ne!(json.get("archive_state"), json.get("archive_visibility"));

        let round_trip: RunRecord = serde_json::from_value(json).unwrap();
        assert_eq!(round_trip.archive_state, RunArchiveState::Archived);
        assert_eq!(
            round_trip.archive_visibility,
            ArchiveVisibility::AccountPrivate
        );
        assert_eq!(
            round_trip
                .retention_policy
                .unwrap()
                .legal_hold_key
                .as_deref(),
            Some("case-42")
        );
    }
}
