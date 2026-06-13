use super::super::WorkspaceSnapshotHydrationStore;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::Utc;
use ctx_core::ids::{SessionId, TaskId, WorkspaceId, WorktreeId};
use ctx_core::models::{
    ExecutionEnvironment, SessionActivityState, SessionHeadSnapshot, SessionHeadWindow,
    SessionMetadata, SessionSnapshotSummary, SessionStatus, SessionTurnStatus, Task, TaskStatus,
    WorkspaceActiveTaskSummary,
};
use std::sync::Mutex;

pub(super) struct FakeHydrationStore {
    pub(super) snapshot_state: (i64, i64),
    pub(super) tasks: Vec<WorkspaceActiveTaskSummary>,
    pub(super) heads: Vec<SessionHeadSnapshot>,
    pub(super) heads_error: Option<&'static str>,
    pub(super) calls: Mutex<Vec<&'static str>>,
}

#[async_trait]
impl WorkspaceSnapshotHydrationStore for FakeHydrationStore {
    async fn get_snapshot_state(&self, _workspace_id: WorkspaceId) -> Result<(i64, i64)> {
        self.calls.lock().unwrap().push("snapshot_state");
        Ok(self.snapshot_state)
    }

    async fn list_active_page_for_hydration(
        &self,
        _workspace_id: WorkspaceId,
        limit: i64,
    ) -> Result<Vec<WorkspaceActiveTaskSummary>> {
        self.calls.lock().unwrap().push("active_page");
        assert_eq!(limit, i64::MAX);
        Ok(self.tasks.clone())
    }

    async fn list_active_heads(
        &self,
        _workspace_id: WorkspaceId,
    ) -> Result<Vec<SessionHeadSnapshot>> {
        self.calls.lock().unwrap().push("active_heads");
        if let Some(message) = self.heads_error {
            return Err(anyhow!(message));
        }
        Ok(self.heads.clone())
    }
}

pub(super) fn test_task(workspace_id: WorkspaceId, task_id: TaskId, session_id: SessionId) -> Task {
    let now = Utc::now();
    Task {
        id: task_id,
        workspace_id,
        title: "hydrate".to_string(),
        description: None,
        status: TaskStatus::Pending,
        exec_plan_id: None,
        primary_session_id: Some(session_id),
        primary_worktree_id: Some(WorktreeId::new()),
        created_at: now,
        updated_at: now,
        archived_at: None,
        assistant_seen_at: None,
        last_activity_at: Some(now),
        last_assistant_message_at: None,
        has_active_session: true,
    }
}

pub(super) fn test_session_metadata(
    workspace_id: WorkspaceId,
    task_id: TaskId,
    session_id: SessionId,
) -> SessionMetadata {
    let now = Utc::now();
    SessionMetadata {
        id: session_id,
        task_id,
        workspace_id,
        worktree_id: WorktreeId::new(),
        execution_environment: ExecutionEnvironment::Host,
        parent_session_id: None,
        relationship: None,
        provider_id: "fake".to_string(),
        model_id: "fake-model".to_string(),
        reasoning_effort: None,
        title: String::new(),
        agent_role: "assistant".to_string(),
        status: SessionStatus::Active,
        provider_session_ref: None,
        created_at: now,
        updated_at: now,
    }
}

pub(super) fn test_summary(
    workspace_id: WorkspaceId,
    task_id: TaskId,
    session_id: SessionId,
) -> WorkspaceActiveTaskSummary {
    WorkspaceActiveTaskSummary {
        task: test_task(workspace_id, task_id, session_id),
        primary_session: SessionSnapshotSummary {
            session: test_session_metadata(workspace_id, task_id, session_id),
            last_message_at: None,
            last_message_preview: Some("canonical-summary".to_string()),
            last_event_seq: Some(44),
            projection_rev: 44,
            state_rev: 44,
            activity: SessionActivityState {
                is_working: false,
                last_turn_status: Some(SessionTurnStatus::Completed),
            },
            unread: None,
        },
        primary_session_head: None,
        sessions: Vec::new(),
        sort_at: Utc::now(),
    }
}

pub(super) fn test_head(
    workspace_id: WorkspaceId,
    task_id: TaskId,
    session_id: SessionId,
) -> SessionHeadSnapshot {
    SessionHeadSnapshot {
        session: test_session_metadata(workspace_id, task_id, session_id),
        turns: Vec::new(),
        tool_summaries: Vec::new(),
        events: Vec::new(),
        messages: Vec::new(),
        last_event_seq: 44,
        projection_rev: 44,
        state_rev: 44,
        activity: SessionActivityState {
            is_working: false,
            last_turn_status: Some(SessionTurnStatus::Completed),
        },
        has_more_turns: false,
        history_cursor: None,
        has_more_history: false,
        summary_checkpoint: None,
        head_window: SessionHeadWindow::default(),
    }
}
