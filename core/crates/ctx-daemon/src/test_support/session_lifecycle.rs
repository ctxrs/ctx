use std::path::Path;
use std::sync::Arc;

use ctx_core::ids::{MessageId, RunId, SessionId, TurnId, WorkspaceId, WorktreeId};
use ctx_core::models::{
    ArchiveVisibility, ExecutionEnvironment, Message, MessageDelivery, MessageRole,
    RetentionPolicyRef, RunArchiveState, RunRecord, RunStatus, SandboxBinding,
    SandboxGuestIdentity, SandboxProfile, SandboxSubstrate, Session, SessionEventType, SessionTurn,
    SessionTurnStatus, VcsKind, Worktree,
};

use crate::daemon;

use super::{
    RunArchiveRouteFixture, SessionModelSwitchFixture, ShutdownRunningTurnFixture, TestDaemon,
};

impl TestDaemon {
    pub async fn seed_shutdown_running_turn_for_test(
        &self,
        root_path: &Path,
        provider_id: &str,
        model_id: &str,
    ) -> anyhow::Result<ShutdownRunningTurnFixture> {
        let workspace = self
            .state
            .global_store()
            .create_workspace(
                "ws".to_string(),
                root_path.to_string_lossy().to_string(),
                VcsKind::Git,
            )
            .await?;
        let store = self.state.store_for_workspace(workspace.id).await?;
        let worktree = store
            .create_worktree(
                workspace.id,
                root_path.to_string_lossy().to_string(),
                "deadbeef".to_string(),
                None,
            )
            .await?;
        self.state
            .global_store()
            .upsert_workspace_worktree_index(worktree.id, workspace.id)
            .await?;
        let task = store
            .create_task(workspace.id, "task".to_string(), None)
            .await?;
        self.state
            .global_store()
            .upsert_workspace_task_index(task.id, workspace.id)
            .await?;
        let session = store
            .create_session(
                task.id,
                workspace.id,
                worktree.id,
                ExecutionEnvironment::Host,
                provider_id.to_string(),
                model_id.to_string(),
                "implementer".to_string(),
                None,
                None,
                None,
            )
            .await?;
        self.state
            .global_store()
            .upsert_workspace_session_index(session.id, workspace.id)
            .await?;

        let turn_id = TurnId::new();
        let now = chrono::Utc::now();
        store
            .insert_session_turn(SessionTurn {
                turn_id,
                session_id: session.id,
                run_id: Some(RunId::new()),
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
            .await?;

        Ok(ShutdownRunningTurnFixture {
            workspace_id: workspace.id,
            session_id: session.id,
            turn_id,
        })
    }

    pub async fn session_turn_status_for_test(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> anyhow::Result<Option<SessionTurnStatus>> {
        Ok(self
            .state
            .store_for_session(session_id)
            .await?
            .get_session_turn(session_id, turn_id)
            .await?
            .map(|turn| turn.status))
    }

    pub async fn seed_invalid_workspace_runtime_settings_document_for_test(
        &self,
        workspace_id: WorkspaceId,
        contents: &str,
    ) -> anyhow::Result<()> {
        self.state
            .store_for_workspace(workspace_id)
            .await?
            .upsert_runtime_settings_document(1, contents)
            .await?;
        Ok(())
    }

    pub async fn seed_workspace_execution_config_for_test(
        &self,
        workspace_id: WorkspaceId,
        update: ctx_workspace_config::ExecutionConfigUpdate,
    ) -> anyhow::Result<()> {
        let store = self.state.store_for_workspace(workspace_id).await?;
        ctx_workspace_config::update_execution_config(&store, update).await
    }

    pub async fn seed_workspace_runtime_settings_without_target_branch_for_test(
        &self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<()> {
        self.state
            .store_for_workspace(workspace_id)
            .await?
            .upsert_runtime_settings_document(1, "{}")
            .await?;
        Ok(())
    }

    pub async fn seed_org_visible_run_archive_fixture_for_test(
        &self,
        workspace_id: WorkspaceId,
        worktree_root: &Path,
    ) -> anyhow::Result<RunArchiveRouteFixture> {
        let store = self.state.store_for_workspace(workspace_id).await?;
        let task = store
            .create_task(workspace_id, "team archive".to_string(), None)
            .await?;
        self.state
            .global_store()
            .upsert_workspace_task_index(task.id, workspace_id)
            .await?;
        let worktree = store
            .create_worktree(
                workspace_id,
                worktree_root.to_string_lossy().into_owned(),
                "deadbeef".to_string(),
                None,
            )
            .await?;
        self.state
            .global_store()
            .upsert_workspace_worktree_index(worktree.id, workspace_id)
            .await?;
        let session = store
            .create_session(
                task.id,
                workspace_id,
                worktree.id,
                ExecutionEnvironment::Host,
                "fake".to_string(),
                "fake-model".to_string(),
                "implementer".to_string(),
                None,
                None,
                None,
            )
            .await?;
        self.state
            .global_store()
            .upsert_workspace_session_index(session.id, workspace_id)
            .await?;

        let now = chrono::Utc::now();
        let run_id = RunId::new();
        let turn_id = TurnId::new();
        store
            .upsert_run(RunRecord {
                id: run_id,
                session_id: session.id,
                task_id: task.id,
                workspace_id,
                worktree_id: session.worktree_id,
                parent_run_id: None,
                account_id: Some(ctx_core::ids::AccountId::new()),
                org_id: Some(ctx_core::ids::OrgId::new()),
                run_grant_id: None,
                status: RunStatus::Completed,
                archive_state: RunArchiveState::Archived,
                archive_visibility: ArchiveVisibility::OrgEvidence,
                retention_policy: Some(RetentionPolicyRef {
                    policy_key: "team-default".into(),
                    legal_hold_key: None,
                }),
                created_at: now,
                started_at: Some(now),
                completed_at: Some(now),
                archived_at: Some(now),
                updated_at: now,
            })
            .await?;
        store
            .insert_message(Message {
                id: MessageId::new(),
                session_id: session.id,
                task_id: task.id,
                run_id: Some(run_id),
                turn_id: Some(turn_id),
                turn_sequence: Some(1),
                order_seq: None,
                role: MessageRole::Assistant,
                content: "reviewed /home/fixture/project/.env".into(),
                attachments: Vec::new(),
                delivery: MessageDelivery::Immediate,
                delivered_at: None,
                created_at: now,
            })
            .await?;
        store
            .append_session_event(
                session.id,
                Some(run_id),
                Some(turn_id),
                SessionEventType::Notice,
                serde_json::json!({
                    "kind": "archive_evidence",
                    "api_key": "sk-12345678901234567890"
                }),
            )
            .await?;

        Ok(RunArchiveRouteFixture {
            workspace_id,
            run_id,
        })
    }

    pub async fn seed_title_generation_session_for_test(
        &self,
        root_path: &Path,
    ) -> anyhow::Result<Session> {
        let workspace = self
            .state
            .global_store()
            .create_workspace(
                "ws".to_string(),
                root_path.to_string_lossy().to_string(),
                VcsKind::Git,
            )
            .await?;
        let store = self.state.store_for_workspace(workspace.id).await?;
        let worktree = store
            .create_worktree(
                workspace.id,
                root_path.to_string_lossy().to_string(),
                "base".to_string(),
                None,
            )
            .await?;
        self.state
            .global_store()
            .upsert_workspace_worktree_index(worktree.id, workspace.id)
            .await?;
        let task = store
            .create_task(
                workspace.id,
                ctx_session_title_service::title_generation::DEFAULT_SESSION_TITLE.to_string(),
                None,
            )
            .await?;
        self.state
            .global_store()
            .upsert_workspace_task_index(task.id, workspace.id)
            .await?;
        let session = store
            .create_session(
                task.id,
                workspace.id,
                worktree.id,
                ExecutionEnvironment::Host,
                "fake".to_string(),
                "fake-model".to_string(),
                "implementer".to_string(),
                None,
                None,
                None,
            )
            .await?;
        self.state
            .global_store()
            .upsert_workspace_session_index(session.id, workspace.id)
            .await?;
        Ok(session)
    }

    pub async fn seed_session_model_switch_session_for_test(
        &self,
        root_path: &Path,
        provider_id: &str,
        model_id: &str,
        reasoning_effort: Option<&str>,
    ) -> anyhow::Result<SessionModelSwitchFixture> {
        let workspace = self
            .state
            .global_store()
            .create_workspace(
                "ws".to_string(),
                root_path.to_string_lossy().to_string(),
                VcsKind::Git,
            )
            .await?;
        let store = self.state.store_for_workspace(workspace.id).await?;
        let worktree = store
            .create_worktree(
                workspace.id,
                root_path.to_string_lossy().to_string(),
                "test-base".to_string(),
                None,
            )
            .await?;
        self.state
            .global_store()
            .upsert_workspace_worktree_index(worktree.id, workspace.id)
            .await?;
        let task = store
            .create_task(workspace.id, "session-model".to_string(), None)
            .await?;
        self.state
            .global_store()
            .upsert_workspace_task_index(task.id, workspace.id)
            .await?;
        let session = store
            .create_session_with_reasoning_effort(
                task.id,
                workspace.id,
                worktree.id,
                ExecutionEnvironment::Host,
                provider_id.to_string(),
                model_id.to_string(),
                reasoning_effort.map(str::to_string),
                "assistant".to_string(),
                None,
                None,
                None,
            )
            .await?;
        self.state
            .global_store()
            .upsert_workspace_session_index(session.id, workspace.id)
            .await?;
        store
            .set_task_primary_session(task.id, session.id, worktree.id)
            .await?;
        Ok(SessionModelSwitchFixture {
            workspace,
            task,
            session,
        })
    }

    pub async fn schedule_fallback_title_generation_for_test(
        &self,
        session_id: SessionId,
        prompt: &str,
        force: bool,
    ) -> anyhow::Result<bool> {
        let session = self
            .state
            .store_for_session(session_id)
            .await?
            .get_session(session_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("session {session_id:?} not found"))?;
        Ok(
            daemon::sessions::title_generation::schedule_session_title_generation(
                Arc::clone(&self.state),
                session,
                prompt.to_string(),
                force,
            )
            .await,
        )
    }

    pub async fn session_title_for_test(
        &self,
        session_id: SessionId,
    ) -> anyhow::Result<Option<String>> {
        Ok(self
            .state
            .store_for_session(session_id)
            .await?
            .get_session(session_id)
            .await?
            .map(|session| session.title))
    }

    pub async fn seed_sandbox_bound_worktree_for_test(
        &self,
        workspace_name: &str,
        workspace_root: &Path,
        host_worktree_root: &Path,
        live_workspace_root: &str,
    ) -> anyhow::Result<Worktree> {
        let workspace = self
            .state
            .global_store()
            .create_workspace(
                workspace_name.to_string(),
                workspace_root.to_string_lossy().to_string(),
                VcsKind::Git,
            )
            .await?;
        let store = self.state.store_for_workspace(workspace.id).await?;
        let worktree = store
            .insert_worktree(Worktree {
                id: WorktreeId::new(),
                workspace_id: workspace.id,
                root_path: host_worktree_root.to_string_lossy().to_string(),
                base_commit_sha: "abc123".to_string(),
                git_branch: Some("ctx/test".to_string()),
                vcs_kind: Some(VcsKind::Git),
                base_revision: Some("abc123".to_string()),
                vcs_ref: Some("ctx/test".to_string()),
                created_at: chrono::Utc::now(),
                bootstrap_status: None,
                bootstrap_started_at: None,
                bootstrap_finished_at: None,
                bootstrap_exit_code: None,
                bootstrap_timeout_sec: None,
                bootstrap_error: None,
                bootstrap_log_path: None,
                bootstrap_log_truncated: None,
                bootstrap_command: None,
                bootstrap_script_path: None,
            })
            .await?;
        store
            .upsert_sandbox_binding(SandboxBinding {
                worktree_id: worktree.id,
                workspace_id: workspace.id,
                sandbox_instance_id: ctx_core::models::sandbox_instance_id_for_workspace(
                    workspace.id,
                ),
                substrate: SandboxSubstrate::SharedVmContainer,
                guest_identity: SandboxGuestIdentity::linux_container_ubuntu(),
                profile: SandboxProfile::Standard,
                live_workspace_root: live_workspace_root.to_string(),
                live_worktree_root: format!(
                    "{}/worktrees/{}",
                    live_workspace_root.trim_end_matches('/'),
                    worktree.id.0
                ),
                execution_settings_json: None,
                container_name: Some("ctx-test".to_string()),
                host_materialization_root: None,
                created_at: chrono::Utc::now(),
            })
            .await?;
        self.state
            .global_store()
            .upsert_workspace_worktree_index(worktree.id, workspace.id)
            .await?;
        Ok(worktree)
    }
}
