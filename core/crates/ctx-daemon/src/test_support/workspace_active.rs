use std::path::PathBuf;
use std::sync::Arc;

use ctx_core::ids::{
    RunId, SessionId, TaskId, TurnId, WorkspaceAttachmentId, WorkspaceId, WorktreeId,
};
use ctx_core::models::{
    Session, SessionEvent, SessionEventType, SessionHeadSnapshot, SessionTurn, SessionTurnStatus,
    WorkspaceActiveTaskSummary, WorkspaceAttachmentStatus, Worktree, WorktreeVcsSnapshot,
};

use crate::daemon;

use super::TestDaemon;

impl TestDaemon {
    pub async fn cache_rehydration_cleanup_session_for_test(&self, session_id: SessionId) {
        self.state
            .task_session_cleanup
            .cleanup_session(session_id)
            .await;
    }

    pub async fn cache_rehydration_cleanup_workspace_for_test(&self, workspace_id: WorkspaceId) {
        self.state
            .test_cleanup_workspace_runtime(workspace_id)
            .await;
    }

    pub async fn cache_rehydration_make_workspace_store_unopenable_for_test(
        &self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<()> {
        self.state.core.stores.evict_workspace(workspace_id).await;
        let workspace_store_path = self
            .data_root()
            .join("db")
            .join("workspaces")
            .join(workspace_id.0.to_string());
        match tokio::fs::metadata(&workspace_store_path).await {
            Ok(metadata) if metadata.is_dir() => {
                tokio::fs::remove_dir_all(&workspace_store_path).await?;
            }
            Ok(_) => {
                tokio::fs::remove_file(&workspace_store_path).await?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let parent = workspace_store_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("workspace store path has no parent"))?;
        tokio::fs::create_dir_all(parent).await?;
        tokio::fs::write(&workspace_store_path, b"blocked workspace store").await?;
        Ok(())
    }

    pub async fn cache_rehydration_begin_workspace_delete_for_test(
        &self,
        workspace_id: WorkspaceId,
    ) {
        self.state
            .core
            .stores
            .begin_workspace_delete(workspace_id)
            .await;
    }

    pub async fn cache_rehydration_finish_workspace_delete_for_test(
        &self,
        workspace_id: WorkspaceId,
    ) {
        self.state
            .core
            .stores
            .finish_workspace_delete(workspace_id)
            .await;
    }

    pub async fn cache_rehydration_hydrate_snapshot_for_test(
        &self,
        workspace_id: WorkspaceId,
        snapshot_rev: i64,
        archived_rev: i64,
        tasks: Vec<WorkspaceActiveTaskSummary>,
        heads: Vec<SessionHeadSnapshot>,
    ) {
        self.state
            .workspaces
            .workspace_active_snapshot
            .hydrate_snapshot(workspace_id, snapshot_rev, archived_rev, tasks, heads)
            .await;
    }

    pub async fn cache_rehydration_active_task_summary_cached_for_test(
        &self,
        workspace_id: WorkspaceId,
        task_id: TaskId,
    ) -> Option<WorkspaceActiveTaskSummary> {
        self.state
            .workspaces
            .workspace_active_snapshot
            .active_task_summary(workspace_id, task_id)
            .await
    }

    pub async fn cache_rehydration_workspace_needs_hydration_for_test(
        &self,
        workspace_id: WorkspaceId,
    ) -> bool {
        self.state
            .workspaces
            .workspace_active_snapshot
            .needs_hydration(workspace_id)
            .await
    }

    pub async fn ensure_workspace_active_snapshot_hydrated(
        &self,
        workspace_id: WorkspaceId,
    ) -> std::result::Result<(), daemon::workspaces::WorkspaceHydrationError> {
        self.workspace_stream_handle_for_test()
            .ensure_workspace_active_snapshot_hydrated(workspace_id)
            .await
    }

    pub async fn session_worktree_root_path_for_test(
        &self,
        session: &Session,
    ) -> anyhow::Result<PathBuf> {
        Ok(PathBuf::from(
            self.load_worktree_for_test(session.worktree_id)
                .await?
                .root_path,
        ))
    }

    pub async fn workspace_active_snapshot_make_store_unopenable_for_test(
        &self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<()> {
        self.cache_rehydration_make_workspace_store_unopenable_for_test(workspace_id)
            .await
    }

    pub async fn workspace_active_snapshot_append_and_publish_event_for_test(
        &self,
        session: &Session,
        run_id: Option<RunId>,
        turn_id: Option<TurnId>,
        event_type: SessionEventType,
        payload_json: serde_json::Value,
    ) -> anyhow::Result<SessionEvent> {
        self.state.sessions.remember_session_meta(session).await;
        let event = self
            .state
            .store_for_session(session.id)
            .await?
            .append_session_event(session.id, run_id, turn_id, event_type, payload_json)
            .await?;
        self.state
            .session_publication
            .publish_event(event.clone())
            .await;
        Ok(event)
    }

    pub async fn workspace_active_snapshot_seed_completed_turn_with_partials_for_test(
        &self,
        session: &Session,
        assistant_partial: &str,
        thought_partial: &str,
    ) -> anyhow::Result<TurnId> {
        let store = self.state.store_for_session(session.id).await?;
        let now = chrono::Utc::now();
        let turn_id = TurnId::new();
        store
            .insert_session_turn(SessionTurn {
                turn_id,
                session_id: session.id,
                run_id: None,
                user_message_id: None,
                status: SessionTurnStatus::Running,
                start_seq: Some(1),
                end_seq: None,
                started_at: now,
                updated_at: now,
                assistant_partial: Some(assistant_partial.to_string()),
                thought_partial: Some(thought_partial.to_string()),
                metrics_json: None,
                failure: None,
                tool_total: 0,
                tool_pending: 0,
                tool_running: 0,
                tool_completed: 0,
                tool_failed: 0,
            })
            .await?;
        store
            .append_session_event(
                session.id,
                None,
                Some(turn_id),
                SessionEventType::AssistantComplete,
                serde_json::json!({
                    "full_content": "final answer",
                    "message_id": "provider-msg-1",
                    "order_seq": 2
                }),
            )
            .await?;
        let checkpoint_event = store
            .append_session_event(
                session.id,
                None,
                Some(turn_id),
                SessionEventType::Notice,
                serde_json::json!({ "kind": "test_checkpoint", "message": "stable" }),
            )
            .await?;
        store
            .update_session_turn_status(
                session.id,
                turn_id,
                SessionTurnStatus::Completed,
                Some(checkpoint_event.seq),
                None,
                chrono::Utc::now(),
            )
            .await?;
        Ok(turn_id)
    }

    pub async fn workspace_active_snapshot_task_contains_sessions_for_test(
        &self,
        task_id: TaskId,
        expected_sessions: &[SessionId],
    ) -> anyhow::Result<bool> {
        let store = self.state.store_for_task(task_id).await?;
        let sessions = store.list_sessions_for_task(task_id).await?;
        Ok(expected_sessions
            .iter()
            .all(|session_id| sessions.iter().any(|stored| stored.id == *session_id)))
    }

    pub async fn workspace_active_snapshot_load_session_worktree_for_test(
        &self,
        session: &Session,
    ) -> anyhow::Result<Worktree> {
        self.load_worktree_for_test(session.worktree_id).await
    }

    pub async fn workspace_active_snapshot_mark_vcs_pane_open_for_test(
        &self,
        worktree_id: WorktreeId,
    ) {
        let mut next_open = std::collections::HashSet::new();
        next_open.insert(worktree_id);
        self.state
            .test_update_worktree_vcs_open_panes(&std::collections::HashSet::new(), &next_open)
            .await;
    }

    pub async fn workspace_active_snapshot_mark_vcs_pane_closed_for_test(
        &self,
        worktree_id: WorktreeId,
    ) {
        let mut previous_open = std::collections::HashSet::new();
        previous_open.insert(worktree_id);
        self.state
            .test_update_worktree_vcs_open_panes(&previous_open, &std::collections::HashSet::new())
            .await;
    }

    pub async fn workspace_active_snapshot_worktree_has_vcs_watcher_for_test(
        &self,
        worktree_id: WorktreeId,
    ) -> bool {
        self.state
            .test_worktree_has_git_status_watcher(worktree_id)
            .await
    }

    pub async fn workspace_active_snapshot_hold_vcs_refresh_lock_for_test(
        &self,
        worktree_id: WorktreeId,
    ) -> tokio::sync::OwnedMutexGuard<()> {
        let refresh_lock = self.state.test_worktree_vcs_refresh_lock(worktree_id).await;
        refresh_lock.lock_owned().await
    }

    pub async fn workspace_active_snapshot_vcs_refresh_lock_token_for_test(
        &self,
        worktree_id: WorktreeId,
    ) -> usize {
        let refresh_lock = self.state.test_worktree_vcs_refresh_lock(worktree_id).await;
        Arc::as_ptr(&refresh_lock) as *const () as usize
    }

    pub async fn workspace_active_snapshot_verify_vcs_refresh_lock_eviction_for_test(
        &self,
        worktree_id: WorktreeId,
    ) -> anyhow::Result<()> {
        self.mark_worktree_vcs_active_for_test(worktree_id).await;
        let initial_lock = self.state.test_worktree_vcs_refresh_lock(worktree_id).await;
        self.mark_worktree_vcs_inactive_for_test(worktree_id).await;

        let next_lock = self.state.test_worktree_vcs_refresh_lock(worktree_id).await;
        if !Arc::ptr_eq(&initial_lock, &next_lock) {
            anyhow::bail!("worktree VCS reactivation should reuse an in-flight refresh lock");
        }

        let old_lock = Arc::downgrade(&initial_lock);
        drop(next_lock);
        drop(initial_lock);

        let replacement_lock = self.state.test_worktree_vcs_refresh_lock(worktree_id).await;
        if old_lock.upgrade().is_some() {
            anyhow::bail!("evicted refresh lock should be released once no refreshes are using it");
        }
        if Arc::strong_count(&replacement_lock) != 1 {
            anyhow::bail!(
                "replacement refresh lock should have one strong reference, got {}",
                Arc::strong_count(&replacement_lock)
            );
        }
        Ok(())
    }

    pub async fn workspace_active_snapshot_seed_ready_vcs_summary_for_test(
        &self,
        worktree: Worktree,
    ) -> anyhow::Result<WorktreeVcsSnapshot> {
        self.mark_worktree_vcs_active_for_test(worktree.id).await;
        self.refresh_worktree_vcs_summary_for_test(worktree.clone())
            .await?;
        self.worktree_vcs_snapshot(worktree.id)
            .await
            .ok_or_else(|| anyhow::anyhow!("expected VCS snapshot for worktree {:?}", worktree.id))
    }

    pub async fn reconcile_turn_terminal_state_for_test(
        &self,
        session_id: SessionId,
        run_id: Option<RunId>,
        turn_id: TurnId,
        fallback_reason: &str,
    ) -> anyhow::Result<()> {
        daemon::scheduler::reconcile_turn_terminal_state(
            &self.state,
            session_id,
            run_id,
            turn_id,
            fallback_reason,
        )
        .await
    }

    pub async fn reconcile_turn_failed_on_provider_exit_for_test(
        &self,
        session_id: SessionId,
        run_id: Option<RunId>,
        turn_id: TurnId,
        fallback_reason: &str,
    ) -> anyhow::Result<()> {
        daemon::scheduler::reconcile_turn_failed_on_provider_exit(
            &self.state,
            session_id,
            run_id,
            turn_id,
            fallback_reason,
        )
        .await
    }

    pub async fn mark_worktree_vcs_active_for_test(&self, worktree_id: WorktreeId) {
        let mut next_active = std::collections::HashSet::new();
        next_active.insert(worktree_id);
        self.state
            .test_update_worktree_vcs_activity(&std::collections::HashSet::new(), &next_active)
            .await;
    }

    pub async fn mark_worktree_vcs_inactive_for_test(&self, worktree_id: WorktreeId) {
        let mut previous_active = std::collections::HashSet::new();
        previous_active.insert(worktree_id);
        self.state
            .test_update_worktree_vcs_activity(&previous_active, &std::collections::HashSet::new())
            .await;
    }

    pub fn worktree_vcs_enabled_for_test(&self) -> bool {
        self.state.test_worktree_vcs_enabled()
    }

    pub async fn is_worktree_vcs_active_for_test(&self, worktree_id: WorktreeId) -> bool {
        self.state.test_is_worktree_vcs_active(worktree_id).await
    }

    pub async fn emit_worktree_vcs_snapshot_for_worktree(
        &self,
        worktree: &Worktree,
        include_commit_info: bool,
    ) -> anyhow::Result<()> {
        self.state
            .test_emit_worktree_vcs_snapshot_for_worktree(worktree, include_commit_info)
            .await
    }

    pub async fn request_worktree_vcs_refresh_for_test(
        &self,
        worktree: &Worktree,
        summary: bool,
        touched_files: bool,
    ) -> anyhow::Result<()> {
        self.state
            .test_request_worktree_vcs_refresh_for_worktree(worktree, summary, touched_files)
            .await
    }

    pub async fn mark_worktree_vcs_filesystem_dirty_for_test(
        &self,
        worktree: &Worktree,
        candidate_path: impl Into<String>,
    ) -> anyhow::Result<()> {
        self.state
            .test_mark_worktree_vcs_dirty_for_worktree(
                worktree,
                ctx_worktree_vcs_service::WorktreeVcsDirtyBits {
                    worktree_fs: true,
                    vcs_meta: false,
                },
                vec![candidate_path.into()],
            )
            .await
    }

    pub async fn mark_worktree_vcs_metadata_dirty_for_test(
        &self,
        worktree: &Worktree,
        candidate_path: impl Into<String>,
    ) -> anyhow::Result<()> {
        self.state
            .test_mark_worktree_vcs_dirty_for_worktree(
                worktree,
                ctx_worktree_vcs_service::WorktreeVcsDirtyBits {
                    worktree_fs: false,
                    vcs_meta: true,
                },
                vec![candidate_path.into()],
            )
            .await
    }

    pub async fn refresh_worktree_vcs_summary_for_test(
        &self,
        worktree: Worktree,
    ) -> anyhow::Result<()> {
        self.state
            .test_refresh_worktree_vcs_summary_for_worktree(worktree)
            .await
    }

    pub async fn run_git_status_watcher_for_test(&self, worktree: Worktree) -> anyhow::Result<()> {
        self.state
            .test_run_git_status_watcher_for_worktree(worktree)
            .await
    }

    pub async fn load_worktree_for_test(
        &self,
        worktree_id: WorktreeId,
    ) -> anyhow::Result<Worktree> {
        self.state
            .store_for_worktree(worktree_id)
            .await?
            .get_worktree(worktree_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("worktree {worktree_id:?} not found"))
    }

    pub async fn workspace_primary_branch_for_test(
        &self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<Option<String>> {
        let store = self.state.store_for_workspace(workspace_id).await?;
        ctx_workspace_config::load_primary_branch(&store).await
    }

    pub async fn set_workspace_attachment_status_for_test(
        &self,
        workspace_id: WorkspaceId,
        attachment_id: WorkspaceAttachmentId,
        status: WorkspaceAttachmentStatus,
    ) -> anyhow::Result<()> {
        self.state
            .store_for_workspace(workspace_id)
            .await?
            .update_workspace_attachment_status(
                attachment_id,
                status,
                None,
                None,
                chrono::Utc::now(),
            )
            .await
    }

    pub async fn worktree_vcs_snapshot(
        &self,
        worktree_id: WorktreeId,
    ) -> Option<WorktreeVcsSnapshot> {
        self.state.test_get_worktree_vcs_snapshot(worktree_id).await
    }
}
