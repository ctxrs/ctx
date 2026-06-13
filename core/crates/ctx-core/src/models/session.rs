use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::*;

use super::workspace::Task;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Active,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionEnvironment {
    #[default]
    Host,
    Sandbox,
}

impl<'de> Deserialize<'de> for ExecutionEnvironment {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        let trimmed = raw.trim();
        if trimmed.eq_ignore_ascii_case("host") {
            return Ok(Self::Host);
        }
        if trimmed.eq_ignore_ascii_case("sandbox") || trimmed.starts_with("container_") {
            return Ok(Self::Sandbox);
        }
        Err(serde::de::Error::custom(format!(
            "unknown execution environment: {trimmed}"
        )))
    }
}

impl ExecutionEnvironment {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Sandbox => "sandbox",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub task_id: TaskId,
    pub workspace_id: WorkspaceId,
    pub worktree_id: WorktreeId,
    pub execution_environment: ExecutionEnvironment,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relationship: Option<String>,
    pub provider_id: String,
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    pub title: String,
    pub agent_role: String,
    pub status: SessionStatus,
    pub provider_session_ref: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub id: SessionId,
    pub task_id: TaskId,
    pub workspace_id: WorkspaceId,
    pub worktree_id: WorktreeId,
    pub execution_environment: ExecutionEnvironment,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relationship: Option<String>,
    pub provider_id: String,
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    pub title: String,
    pub agent_role: String,
    pub status: SessionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_session_ref: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentInvocation {
    pub id: String,
    pub tool_call_id: String,
    pub parent_session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_turn_id: Option<TurnId>,
    pub requested_count: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_json: Option<serde_json::Value>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub children: Vec<SubagentInvocationChild>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentInvocationChild {
    pub invocation_id: String,
    pub child_session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    pub position: i64,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    pub prompt_length: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalStatus {
    Running,
    Exited,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalSession {
    pub id: TerminalId,
    pub workspace_id: WorkspaceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<WorktreeId>,
    pub cwd: String,
    pub shell: String,
    pub title: String,
    pub status: TerminalStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub stream_path: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageDelivery {
    Immediate,
    Queued,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageAttachment {
    Image {
        mime_type: String,
        data_base64: String,
        #[serde(default)]
        name: Option<String>,
    },
    ImageRef {
        blob_id: String,
        mime_type: String,
        #[serde(default)]
        name: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub run_id: Option<RunId>,
    pub turn_id: Option<TurnId>,
    #[serde(default)]
    pub turn_sequence: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_seq: Option<i64>,
    pub role: MessageRole,
    pub content: String,
    #[serde(default)]
    pub attachments: Vec<MessageAttachment>,
    pub delivery: MessageDelivery,
    pub delivered_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub id: ArtifactId,
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub workspace_id: WorkspaceId,
    pub worktree_id: WorktreeId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub absolute_path: String,
    pub mime_type: String,
    pub bytes: i64,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missing: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionTurnStatus {
    Queued,
    Starting,
    Running,
    Completed,
    Interrupted,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct SessionActivityState {
    pub is_working: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_turn_status: Option<SessionTurnStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionTurnFailure {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTurn {
    pub turn_id: TurnId,
    pub session_id: SessionId,
    pub run_id: Option<RunId>,
    pub user_message_id: Option<MessageId>,
    pub status: SessionTurnStatus,
    pub start_seq: Option<i64>,
    pub end_seq: Option<i64>,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub assistant_partial: Option<String>,
    pub thought_partial: Option<String>,
    pub metrics_json: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<SessionTurnFailure>,
    pub tool_total: i64,
    pub tool_pending: i64,
    pub tool_running: i64,
    pub tool_completed: i64,
    pub tool_failed: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTurnTool {
    pub session_id: SessionId,
    pub tool_call_id: String,
    pub turn_id: TurnId,
    pub tool_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_tool_name: Option<String>,
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    pub status: Option<String>,
    pub input_json: Option<serde_json::Value>,
    pub output_text: Option<String>,
    pub order_seq: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_event_seq: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_truncated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_original_bytes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_truncated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_original_bytes: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTurnToolSummary {
    pub session_id: SessionId,
    pub tool_call_id: String,
    pub turn_id: TurnId,
    pub tool_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_tool_name: Option<String>,
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_preview: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_preview: Option<String>,
    pub order_seq: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_event_seq: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_truncated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_original_bytes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_truncated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_original_bytes: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: SessionId,
    pub task_id: TaskId,
    pub workspace_id: WorkspaceId,
    pub execution_environment: ExecutionEnvironment,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relationship: Option<String>,
    pub provider_id: String,
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    pub title: String,
    pub status: SessionStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceTaskSummary {
    pub task: Task,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<SessionSummary>,
    pub sort_at: DateTime<Utc>,
}
