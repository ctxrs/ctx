use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::*;

use super::{
    Artifact, Message, Session, SessionActivityState, SessionEvent, SessionMetadata, SessionTurn,
    SessionTurnToolSummary, Task, WorkspaceTaskSummary, WorktreeBootstrapStatus,
    WorktreeVcsSnapshot,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceIndexCursor {
    pub sort_at: DateTime<Utc>,
    pub task_id: TaskId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceIndexPage {
    pub workspace_id: WorkspaceId,
    pub snapshot_rev: i64,
    pub tasks: Vec<WorkspaceTaskSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<WorkspaceIndexCursor>,
    pub total_active: i64,
    pub total_archived: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceArchivedPage {
    pub workspace_id: WorkspaceId,
    #[serde(default)]
    pub archived_rev: i64,
    pub tasks: Vec<WorkspaceTaskSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<WorkspaceIndexCursor>,
    pub total_archived: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkspaceIndexEvent {
    Ready {
        workspace_id: WorkspaceId,
        snapshot_rev: i64,
    },
    TaskUpsert {
        workspace_id: WorkspaceId,
        snapshot_rev: i64,
        task: Box<WorkspaceTaskSummary>,
    },
    TaskDelete {
        workspace_id: WorkspaceId,
        snapshot_rev: i64,
        task_id: TaskId,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceActiveTaskSummary {
    pub task: Task,
    pub primary_session: SessionSnapshotSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_session_head: Option<SessionHeadSnapshot>,
    #[serde(default)]
    pub sessions: Vec<SessionSnapshotSummary>,
    pub sort_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceActivePage {
    pub tasks: Vec<WorkspaceActiveTaskSummary>,
    pub total_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceActiveSnapshot {
    pub workspace_id: WorkspaceId,
    pub snapshot_rev: i64,
    #[serde(default)]
    pub archived_rev: i64,
    pub active: WorkspaceActivePage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceActiveHeadBatch {
    pub workspace_id: WorkspaceId,
    pub snapshot_rev: i64,
    #[serde(default)]
    pub heads: Vec<SessionHeadSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshotSummary {
    pub session: SessionMetadata,
    pub last_message_at: Option<DateTime<Utc>>,
    pub last_message_preview: Option<String>,
    pub last_event_seq: Option<i64>,
    #[serde(default)]
    pub projection_rev: i64,
    #[serde(default)]
    pub state_rev: i64,
    #[serde(default)]
    pub activity: SessionActivityState,
    pub unread: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskDeltaKind {
    Updated,
    Archived,
    Unarchived,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDelta {
    pub task: Task,
    pub kind: TaskDeltaKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummaryDelta {
    pub session_id: SessionId,
    pub task_id: TaskId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<SessionActivityState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message_preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event_seq: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection_rev: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_rev: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emitted_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummaryCheckpoint {
    pub session_id: SessionId,
    pub checkpoint_id: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_turn_id: Option<TurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event_seq: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionHeadWindow {
    pub turn_limit: i64,
    pub message_limit: i64,
    pub event_limit: i64,
    pub byte_limit: i64,
    pub turn_count: i64,
    pub message_count: i64,
    pub event_count: i64,
    pub bytes: i64,
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionHeadSnapshot {
    pub session: SessionMetadata,
    #[serde(default)]
    pub turns: Vec<SessionTurn>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_summaries: Vec<SessionTurnToolSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<SessionEvent>,
    #[serde(default)]
    pub messages: Vec<Message>,
    pub last_event_seq: i64,
    #[serde(default)]
    pub projection_rev: i64,
    #[serde(default)]
    pub state_rev: i64,
    #[serde(default)]
    pub activity: SessionActivityState,
    pub has_more_turns: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_cursor: Option<i64>,
    #[serde(default)]
    pub has_more_history: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_checkpoint: Option<SessionSummaryCheckpoint>,
    #[serde(default)]
    pub head_window: SessionHeadWindow,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionHead {
    pub session: Session,
    #[serde(default)]
    pub turns: Vec<SessionTurn>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_summaries: Vec<SessionTurnToolSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<SessionEvent>,
    #[serde(default)]
    pub messages: Vec<Message>,
    pub last_event_seq: i64,
    #[serde(default)]
    pub projection_rev: i64,
    #[serde(default)]
    pub activity: SessionActivityState,
    pub has_more_turns: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_checkpoint: Option<SessionSummaryCheckpoint>,
    #[serde(default)]
    pub head_window: SessionHeadWindow,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionGitStatusSummary {
    pub summary_line: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    pub ahead: i64,
    pub behind: i64,
    pub detached: bool,
    pub staged: i64,
    pub unstaged: i64,
    pub untracked: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionState {
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_status: Option<SessionGitStatusSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub summary: SessionSnapshotSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<SessionHeadSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<SessionState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionHeadDelta {
    pub session_id: SessionId,
    pub last_event_seq: i64,
    #[serde(default)]
    pub projection_rev: i64,
    #[serde(default)]
    pub state_rev: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emitted_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<SessionActivityState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<SessionEvent>,
    pub turn: Option<SessionTurn>,
    pub message: Option<Message>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_summaries: Vec<SessionTurnToolSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionHistoryPage {
    pub session_id: SessionId,
    #[serde(default)]
    pub turns: Vec<SessionTurn>,
    #[serde(default)]
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<i64>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEventsPage {
    pub session_id: SessionId,
    #[serde(default)]
    pub events: Vec<SessionEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<i64>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeBootstrapNotice {
    pub worktree_id: WorktreeId,
    pub worktree_root: String,
    pub status: WorktreeBootstrapStatus,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_sec: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_truncated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkspaceActiveSnapshotEvent {
    Ready {
        workspace_id: WorkspaceId,
        snapshot_rev: i64,
        #[serde(default)]
        archived_rev: i64,
    },
    ActiveTaskUpsert {
        workspace_id: WorkspaceId,
        snapshot_rev: i64,
        task: Box<WorkspaceActiveTaskSummary>,
    },
    ActiveTaskDelete {
        workspace_id: WorkspaceId,
        snapshot_rev: i64,
        task_id: TaskId,
    },
    TaskDelta {
        workspace_id: WorkspaceId,
        snapshot_rev: i64,
        delta: Box<TaskDelta>,
    },
    SessionSummary {
        workspace_id: WorkspaceId,
        snapshot_rev: i64,
        summary: Box<SessionSnapshotSummary>,
    },
    SessionSummaryDelta {
        workspace_id: WorkspaceId,
        snapshot_rev: i64,
        delta: Box<SessionSummaryDelta>,
    },
    SessionRemoved {
        workspace_id: WorkspaceId,
        snapshot_rev: i64,
        session_id: SessionId,
    },
    SessionHeadDelta {
        workspace_id: WorkspaceId,
        snapshot_rev: i64,
        delta: Box<SessionHeadDelta>,
    },
    SessionHeadSeed {
        workspace_id: WorkspaceId,
        snapshot_rev: i64,
        head: Box<SessionHeadSnapshot>,
    },
    SessionGap {
        workspace_id: WorkspaceId,
        snapshot_rev: i64,
        session_id: SessionId,
        after_seq: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default, skip_serializing_if = "super::is_false")]
        seed_follows: bool,
    },
    WorktreeBootstrap {
        workspace_id: WorkspaceId,
        snapshot_rev: i64,
        notice: WorktreeBootstrapNotice,
    },
    ArchivedTaskUpsert {
        workspace_id: WorkspaceId,
        archived_rev: i64,
        task: Box<WorkspaceTaskSummary>,
    },
    ArchivedTaskDelete {
        workspace_id: WorkspaceId,
        archived_rev: i64,
        task_id: TaskId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceActiveSnapshotStreamSource {
    Live,
    Replay,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkspaceActiveSnapshotStreamMessage {
    Snapshot {
        rev: i64,
        active_snapshot: WorkspaceActiveSnapshot,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active_heads: Option<WorkspaceActiveHeadBatch>,
    },
    Event {
        rev: i64,
        event: Box<WorkspaceActiveSnapshotEvent>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stream_source: Option<WorkspaceActiveSnapshotStreamSource>,
    },
    HeadsBatch {
        rev: i64,
        snapshot_rev: i64,
        #[serde(default)]
        deltas: Vec<SessionHeadDelta>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stream_source: Option<WorkspaceActiveSnapshotStreamSource>,
    },
    ResetRequired {
        latest_rev: i64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum WorkspaceActiveSnapshotSessionReplay {
    Auto,
    Reset,
    Resume {
        after_seq: i64,
        #[serde(default)]
        after_projection_rev: i64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceActiveSnapshotSessionSubscription {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<WorkspaceActiveSnapshotSessionIntent>,
    pub replay: WorkspaceActiveSnapshotSessionReplay,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceActiveSnapshotSessionIntent {
    Head,
    Replay,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceActiveSnapshotSubscribeScope {
    Active,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkspaceActiveSnapshotClientMessage {
    Subscribe {
        #[serde(default)]
        session_ids: Vec<SessionId>,
        #[serde(default)]
        sessions: Vec<WorkspaceActiveSnapshotSessionSubscription>,
        #[serde(default)]
        task_ids: Vec<TaskId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        foreground_session_id: Option<SessionId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<WorkspaceActiveSnapshotSubscribeScope>,
        #[serde(default, skip_serializing_if = "super::is_false")]
        include_active_heads: bool,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeVcsStreamTier {
    Summary,
    Details,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorktreeVcsStreamClientMessage {
    ReplaceSubscription {
        #[serde(default)]
        summary_worktree_ids: Vec<WorktreeId>,
        #[serde(default)]
        detail_worktree_ids: Vec<WorktreeId>,
    },
    Refresh {
        #[serde(default)]
        worktree_ids: Vec<WorktreeId>,
        tier: WorktreeVcsStreamTier,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorktreeVcsStreamMessage {
    Ready {
        workspace_id: WorkspaceId,
        vcs_generation: i64,
    },
    Subscribed {
        workspace_id: WorkspaceId,
        demand_generation: i64,
        summary_worktree_ids: Vec<WorktreeId>,
        detail_worktree_ids: Vec<WorktreeId>,
    },
    SummarySnapshot {
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
        demand_generation: i64,
        snapshot: WorktreeVcsSnapshot,
    },
    DetailsSnapshot {
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
        demand_generation: i64,
        snapshot: WorktreeVcsSnapshot,
    },
    UnavailableSnapshot {
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
        demand_generation: i64,
        snapshot: WorktreeVcsSnapshot,
    },
    ResetRequired {
        workspace_id: WorkspaceId,
        vcs_generation: i64,
    },
}
