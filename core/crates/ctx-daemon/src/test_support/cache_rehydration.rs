use std::path::PathBuf;

use anyhow::Context;
use ctx_core::ids::{MessageId, RunId, TaskId, TurnId, WorkspaceId};
use ctx_core::models::{
    ExecutionEnvironment, Message, MessageDelivery, MessageRole, Session, SessionActivityState,
    SessionEvent, SessionEventType, SessionHeadDelta, SessionHeadSnapshot, SessionTurn,
    SessionTurnStatus, Task, VcsKind, Workspace, WorkspaceActiveTaskSummary, Worktree,
};

use super::TestDaemon;

#[derive(Clone)]
pub struct CacheRehydrationSessionFixture {
    pub workspace: Workspace,
    pub worktree: Worktree,
    pub task: Task,
    pub session: Session,
}

#[derive(Clone)]
pub struct CacheRehydrationSubagentFixture {
    pub workspace: Workspace,
    pub worktree: Worktree,
    pub task: Task,
    pub primary: Session,
    pub subagent: Session,
}

#[derive(Clone)]
pub struct CacheRehydrationTurnFixture {
    pub run_id: RunId,
    pub turn_id: TurnId,
    pub event: SessionEvent,
    pub projection_rev: i64,
}

impl TestDaemon {
    pub async fn seed_cache_rehydration_session_for_test(
        &self,
        mark_primary: bool,
        index_task: bool,
    ) -> anyhow::Result<CacheRehydrationSessionFixture> {
        let workspace_root = cache_rehydration_workspace_root(self.data_root());
        tokio::fs::create_dir_all(&workspace_root)
            .await
            .context("create cache rehydration workspace root")?;

        let workspace = self
            .global_store()
            .create_workspace(
                "ws".to_string(),
                workspace_root.to_string_lossy().to_string(),
                VcsKind::Git,
            )
            .await
            .context("create cache rehydration workspace")?;
        let store = self
            .store_for_workspace(workspace.id)
            .await
            .context("open cache rehydration workspace store")?;
        let worktree = store
            .create_worktree(
                workspace.id,
                workspace_root.to_string_lossy().to_string(),
                "deadbeef".to_string(),
                None,
            )
            .await
            .context("create cache rehydration worktree")?;
        let task = store
            .create_task(workspace.id, "task".to_string(), None)
            .await
            .context("create cache rehydration task")?;
        let session = store
            .create_session(
                task.id,
                workspace.id,
                worktree.id,
                ExecutionEnvironment::Host,
                "fake".to_string(),
                "model".to_string(),
                "implementer".to_string(),
                None,
                None,
                None,
            )
            .await
            .context("create cache rehydration session")?;

        if mark_primary {
            store
                .set_task_primary_session(task.id, session.id, worktree.id)
                .await
                .context("set cache rehydration primary session")?;
        }
        if index_task {
            self.global_store()
                .upsert_workspace_task_index(task.id, workspace.id)
                .await
                .context("index cache rehydration task")?;
        }
        self.global_store()
            .upsert_workspace_session_index(session.id, workspace.id)
            .await
            .context("index cache rehydration session")?;

        Ok(CacheRehydrationSessionFixture {
            workspace,
            worktree,
            task,
            session,
        })
    }

    pub async fn seed_cache_rehydration_primary_and_subagent_for_test(
        &self,
    ) -> anyhow::Result<CacheRehydrationSubagentFixture> {
        let primary_fixture = self
            .seed_cache_rehydration_session_for_test(true, false)
            .await?;
        let store = self
            .store_for_workspace(primary_fixture.workspace.id)
            .await
            .context("open cache rehydration subagent workspace store")?;
        let subagent = store
            .create_session(
                primary_fixture.task.id,
                primary_fixture.workspace.id,
                primary_fixture.worktree.id,
                ExecutionEnvironment::Host,
                "fake".to_string(),
                "model".to_string(),
                "reviewer".to_string(),
                Some(primary_fixture.session.id),
                Some("sub_agent".to_string()),
                None,
            )
            .await
            .context("create cache rehydration subagent session")?;
        self.global_store()
            .upsert_workspace_session_index(subagent.id, primary_fixture.workspace.id)
            .await
            .context("index cache rehydration subagent session")?;

        Ok(CacheRehydrationSubagentFixture {
            workspace: primary_fixture.workspace,
            worktree: primary_fixture.worktree,
            task: primary_fixture.task,
            primary: primary_fixture.session,
            subagent,
        })
    }

    pub async fn cache_rehydration_seed_completed_notice_for_test(
        &self,
        session: &Session,
        task_id: TaskId,
        payload_json: serde_json::Value,
        assistant_message: Option<&str>,
    ) -> anyhow::Result<CacheRehydrationTurnFixture> {
        let store = self
            .store_for_session(session.id)
            .await
            .context("open cache rehydration session store")?;
        let run_id = RunId::new();
        let turn_id = TurnId::new();
        let now = chrono::Utc::now();
        store
            .insert_session_turn(SessionTurn {
                turn_id,
                session_id: session.id,
                run_id: Some(run_id),
                user_message_id: None,
                status: SessionTurnStatus::Running,
                start_seq: Some(1),
                end_seq: None,
                started_at: now,
                updated_at: now,
                assistant_partial: None,
                thought_partial: None,
                metrics_json: None,
                failure: None,
                tool_total: 0,
                tool_pending: 0,
                tool_running: 0,
                tool_completed: 0,
                tool_failed: 0,
            })
            .await
            .context("insert cache rehydration turn")?;
        let event = store
            .append_session_event(
                session.id,
                Some(run_id),
                Some(turn_id),
                SessionEventType::Notice,
                payload_json,
            )
            .await
            .context("append cache rehydration event")?;
        store
            .update_session_turn_status(
                session.id,
                turn_id,
                SessionTurnStatus::Completed,
                Some(event.seq),
                None,
                chrono::Utc::now(),
            )
            .await
            .context("complete cache rehydration turn")?;
        if let Some(content) = assistant_message {
            store
                .insert_message(Message {
                    id: MessageId::new(),
                    session_id: session.id,
                    task_id,
                    run_id: Some(run_id),
                    turn_id: Some(turn_id),
                    turn_sequence: Some(1),
                    order_seq: None,
                    role: MessageRole::Assistant,
                    content: content.to_string(),
                    attachments: vec![],
                    delivery: MessageDelivery::Immediate,
                    delivered_at: None,
                    created_at: chrono::Utc::now(),
                })
                .await
                .context("insert cache rehydration assistant message")?;
        }
        let projection_rev = store
            .get_session_projection_rev(session.id)
            .await
            .context("load cache rehydration projection rev")?;

        Ok(CacheRehydrationTurnFixture {
            run_id,
            turn_id,
            event,
            projection_rev,
        })
    }

    pub async fn cache_rehydration_full_head_for_test(
        &self,
        session_id: ctx_core::ids::SessionId,
        limit: u32,
        include_events: bool,
    ) -> anyhow::Result<SessionHeadSnapshot> {
        self.store_for_session(session_id)
            .await?
            .get_session_head_snapshot(session_id, limit, include_events)
            .await?
            .ok_or_else(|| anyhow::anyhow!("missing cache rehydration session head {session_id:?}"))
    }

    pub async fn cache_rehydration_active_head_for_test(
        &self,
        session_id: ctx_core::ids::SessionId,
    ) -> anyhow::Result<Option<SessionHeadSnapshot>> {
        self.store_for_session(session_id)
            .await?
            .get_active_snapshot_head(session_id)
            .await
            .map_err(Into::into)
    }

    pub async fn cache_rehydration_active_task_summary_for_test(
        &self,
        task_id: TaskId,
    ) -> anyhow::Result<WorkspaceActiveTaskSummary> {
        self.store_for_task(task_id)
            .await?
            .get_workspace_active_task_summary(task_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("missing cache rehydration active task {task_id:?}"))
    }

    pub async fn cache_rehydration_delete_workspace_rows_for_test(
        &self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<()> {
        self.global_store()
            .delete_workspace_indexes(workspace_id)
            .await
            .context("delete cache rehydration workspace indexes")?;
        self.global_store()
            .delete_workspace(workspace_id)
            .await
            .context("delete cache rehydration workspace")?;
        Ok(())
    }

    pub async fn cache_rehydration_publish_cold_running_delta_for_test(
        &self,
        session: &Session,
        turn: &CacheRehydrationTurnFixture,
    ) {
        let delta = SessionHeadDelta {
            session_id: session.id,
            last_event_seq: turn.event.seq,
            projection_rev: turn.projection_rev,
            state_rev: turn.event.seq,
            emitted_at_ms: None,
            session: None,
            activity: Some(SessionActivityState {
                is_working: true,
                last_turn_status: Some(SessionTurnStatus::Running),
            }),
            event: None,
            turn: None,
            message: None,
            tool_summaries: Vec::new(),
        };
        self.publish_session_head_delta(session, delta, true).await;
    }
}

fn cache_rehydration_workspace_root(data_root: &std::path::Path) -> PathBuf {
    data_root
        .join("cache-rehydration-workspaces")
        .join(uuid::Uuid::new_v4().to_string())
}
