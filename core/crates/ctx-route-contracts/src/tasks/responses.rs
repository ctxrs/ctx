use chrono::{DateTime, Utc};
use ctx_core::ids::{SessionId, TaskId, WorkspaceId, WorktreeId};
use ctx_core::models::{
    ExecutionEnvironment, Session, SessionStatus, SessionSummary, Task, TaskStatus,
    WorkspaceArchivedPage, WorkspaceIndexCursor, WorkspaceTaskSummary,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct TaskRouteResponse {
    pub id: TaskId,
    pub workspace_id: WorkspaceId,
    pub title: String,
    pub description: Option<String>,
    pub status: TaskStatusRouteResponse,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub exec_plan_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_worktree_id: Option<WorktreeId>,
    #[serde(default)]
    pub archived_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant_seen_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_assistant_message_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub has_active_session: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatusRouteResponse {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionRouteResponse {
    pub id: SessionId,
    pub task_id: TaskId,
    pub workspace_id: WorkspaceId,
    pub worktree_id: WorktreeId,
    pub execution_environment: ExecutionEnvironmentRouteValue,
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
    pub status: SessionStatusRouteResponse,
    pub provider_session_ref: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionSummaryRouteResponse {
    pub id: SessionId,
    pub task_id: TaskId,
    pub workspace_id: WorkspaceId,
    pub execution_environment: ExecutionEnvironmentRouteValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relationship: Option<String>,
    pub provider_id: String,
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    pub title: String,
    pub status: SessionStatusRouteResponse,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatusRouteResponse {
    Active,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionEnvironmentRouteValue {
    Host,
    Sandbox,
}

impl<'de> Deserialize<'de> for ExecutionEnvironmentRouteValue {
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

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceIndexCursorRouteResponse {
    pub sort_at: DateTime<Utc>,
    pub task_id: TaskId,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceArchivedPageRouteResponse {
    pub workspace_id: WorkspaceId,
    #[serde(default)]
    pub archived_rev: i64,
    pub tasks: Vec<WorkspaceTaskSummaryRouteResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<WorkspaceIndexCursorRouteResponse>,
    pub total_archived: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceTaskSummaryRouteResponse {
    pub task: TaskRouteResponse,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<SessionSummaryRouteResponse>,
    pub sort_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArchiveTaskRouteResponse {
    #[serde(flatten)]
    pub task: TaskRouteResponse,
    pub cleanup_failed: bool,
}

impl ArchiveTaskRouteResponse {
    pub fn new(task: TaskRouteResponse, cleanup_failed: bool) -> Self {
        Self {
            task,
            cleanup_failed,
        }
    }

    pub fn from_task(task: Task, cleanup_failed: bool) -> Self {
        Self::new(task.into(), cleanup_failed)
    }
}

impl From<Task> for TaskRouteResponse {
    fn from(task: Task) -> Self {
        Self {
            id: task.id,
            workspace_id: task.workspace_id,
            title: task.title,
            description: task.description,
            status: task.status.into(),
            created_at: task.created_at,
            updated_at: task.updated_at,
            exec_plan_id: task.exec_plan_id,
            primary_session_id: task.primary_session_id,
            primary_worktree_id: task.primary_worktree_id,
            archived_at: task.archived_at,
            assistant_seen_at: task.assistant_seen_at,
            last_activity_at: task.last_activity_at,
            last_assistant_message_at: task.last_assistant_message_at,
            has_active_session: task.has_active_session,
        }
    }
}

impl From<TaskStatus> for TaskStatusRouteResponse {
    fn from(status: TaskStatus) -> Self {
        match status {
            TaskStatus::Pending => Self::Pending,
            TaskStatus::Running => Self::Running,
            TaskStatus::Completed => Self::Completed,
            TaskStatus::Failed => Self::Failed,
            TaskStatus::Cancelled => Self::Cancelled,
        }
    }
}

impl From<Session> for SessionRouteResponse {
    fn from(session: Session) -> Self {
        Self {
            id: session.id,
            task_id: session.task_id,
            workspace_id: session.workspace_id,
            worktree_id: session.worktree_id,
            execution_environment: session.execution_environment.into(),
            parent_session_id: session.parent_session_id,
            relationship: session.relationship,
            provider_id: session.provider_id,
            model_id: session.model_id,
            reasoning_effort: session.reasoning_effort,
            title: session.title,
            agent_role: session.agent_role,
            status: session.status.into(),
            provider_session_ref: session.provider_session_ref,
            created_at: session.created_at,
            updated_at: session.updated_at,
        }
    }
}

impl From<SessionSummary> for SessionSummaryRouteResponse {
    fn from(session: SessionSummary) -> Self {
        Self {
            id: session.id,
            task_id: session.task_id,
            workspace_id: session.workspace_id,
            execution_environment: session.execution_environment.into(),
            parent_session_id: session.parent_session_id,
            relationship: session.relationship,
            provider_id: session.provider_id,
            model_id: session.model_id,
            reasoning_effort: session.reasoning_effort,
            title: session.title,
            status: session.status.into(),
            created_at: session.created_at,
            updated_at: session.updated_at,
        }
    }
}

impl From<SessionStatus> for SessionStatusRouteResponse {
    fn from(status: SessionStatus) -> Self {
        match status {
            SessionStatus::Active => Self::Active,
            SessionStatus::Completed => Self::Completed,
            SessionStatus::Failed => Self::Failed,
            SessionStatus::Cancelled => Self::Cancelled,
        }
    }
}

impl From<ExecutionEnvironment> for ExecutionEnvironmentRouteValue {
    fn from(value: ExecutionEnvironment) -> Self {
        match value {
            ExecutionEnvironment::Host => Self::Host,
            ExecutionEnvironment::Sandbox => Self::Sandbox,
        }
    }
}

impl From<ExecutionEnvironmentRouteValue> for ExecutionEnvironment {
    fn from(value: ExecutionEnvironmentRouteValue) -> Self {
        match value {
            ExecutionEnvironmentRouteValue::Host => Self::Host,
            ExecutionEnvironmentRouteValue::Sandbox => Self::Sandbox,
        }
    }
}

impl From<WorkspaceIndexCursor> for WorkspaceIndexCursorRouteResponse {
    fn from(cursor: WorkspaceIndexCursor) -> Self {
        Self {
            sort_at: cursor.sort_at,
            task_id: cursor.task_id,
        }
    }
}

impl From<WorkspaceTaskSummary> for WorkspaceTaskSummaryRouteResponse {
    fn from(summary: WorkspaceTaskSummary) -> Self {
        Self {
            task: summary.task.into(),
            provider_ids: summary.provider_ids,
            sessions: summary.sessions.into_iter().map(Into::into).collect(),
            sort_at: summary.sort_at,
        }
    }
}

impl From<WorkspaceArchivedPage> for WorkspaceArchivedPageRouteResponse {
    fn from(page: WorkspaceArchivedPage) -> Self {
        Self {
            workspace_id: page.workspace_id,
            archived_rev: page.archived_rev,
            tasks: page.tasks.into_iter().map(Into::into).collect(),
            next_cursor: page.next_cursor.map(Into::into),
            total_archived: page.total_archived,
        }
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}
