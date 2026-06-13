use std::path::Path;
use std::time::Duration;

use ctx_core::ids::{MessageId, SessionId, TaskId, WorkspaceId, WorktreeId};
use ctx_core::models::{
    ExecutionEnvironment, Message, MessageDelivery, MessageRole, SandboxBinding,
    SandboxGuestIdentity, SandboxProfile, Session, Task, VcsKind, Workspace, Worktree,
};
use ctx_settings_model::{ExecutionSettings, Settings};
use ctx_store::Store;

use crate::daemon;

use super::{
    GlobalIdRoutingSessionFixture, GlobalIdRoutingWorkspaceSessionSeed,
    TaskArchiveManagedWorktreesSnapshot, TaskDefaultSessionSnapshot,
    TaskLifecycleSandboxBindingSeed, TaskLifecycleSessionSeed, TaskLifecycleSnapshot,
    TaskLifecycleWorktreeSeed, TaskSessionCreationLockGuardForTest, TestDaemon,
    WorkspaceAttachmentsDemoFixture,
};

impl TestDaemon {
    pub async fn seed_task_default_workspace_for_test(
        &self,
        name: &str,
        root_path: &Path,
        vcs_kind: VcsKind,
    ) -> anyhow::Result<Workspace> {
        self.seed_workspace_for_test(name, root_path, vcs_kind)
            .await
    }

    pub async fn seed_workspace_for_test(
        &self,
        name: &str,
        root_path: &Path,
        vcs_kind: VcsKind,
    ) -> anyhow::Result<Workspace> {
        let workspace = self
            .state
            .global_store()
            .create_workspace(
                name.to_string(),
                root_path.to_string_lossy().to_string(),
                vcs_kind,
            )
            .await?;
        let _ = self.state.store_for_workspace(workspace.id).await?;
        Ok(workspace)
    }

    pub async fn seed_workspace_attachments_demo_fixture_for_test(
        &self,
        name: &str,
        root_path: &Path,
        base_commit_sha: String,
    ) -> anyhow::Result<WorkspaceAttachmentsDemoFixture> {
        let workspace = self
            .state
            .global_store()
            .create_workspace(
                name.to_string(),
                root_path.to_string_lossy().to_string(),
                VcsKind::Git,
            )
            .await?;
        let store = self.state.store_for_workspace(workspace.id).await?;
        let worktree = store
            .create_worktree(
                workspace.id,
                root_path.to_string_lossy().to_string(),
                base_commit_sha,
                Some("main".to_string()),
            )
            .await?;
        let task = store
            .create_task(workspace.id, name.to_string(), None)
            .await?;
        store
            .set_task_primary_worktree(task.id, worktree.id)
            .await?;
        let task = store
            .get_task(task.id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("seeded attachment demo task missing"))?;
        self.state
            .global_store()
            .upsert_workspace_worktree_index(worktree.id, workspace.id)
            .await?;
        self.state
            .global_store()
            .upsert_workspace_task_index(task.id, workspace.id)
            .await?;

        Ok(WorkspaceAttachmentsDemoFixture {
            workspace,
            worktree,
            task,
        })
    }

    pub async fn save_execution_settings_for_test(
        &self,
        execution: ExecutionSettings,
    ) -> anyhow::Result<()> {
        let settings = Settings {
            execution: Some(execution),
            ..Default::default()
        };
        ctx_settings_service::save_settings(self.state.global_store(), &settings).await?;
        Ok(())
    }

    pub async fn preseed_settings_for_data_root_for_test(
        data_root: &Path,
        settings: &Settings,
    ) -> anyhow::Result<()> {
        let db_path = data_root.join("db").join("db.sqlite");
        let store = Store::open_sqlite(&db_path, None).await?;
        ctx_settings_service::save_settings(&store, settings).await?;
        store.close().await;
        Ok(())
    }

    pub async fn write_workspace_container_execution_without_runtime_probe_for_test(
        &self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<()> {
        let store = self.state.store_for_workspace(workspace_id).await?;
        ctx_workspace_config::update_execution_config(
            &store,
            ctx_workspace_config::ExecutionConfigUpdate {
                environment: ctx_workspace_config::ExecutionEnvironment::Sandbox,
                network_mode: None,
                allowlist: None,
                image: None,
            },
        )
        .await?;
        Ok(())
    }

    pub async fn task_default_session_snapshot_for_test(
        &self,
        workspace_id: WorkspaceId,
        task_id: TaskId,
    ) -> anyhow::Result<TaskDefaultSessionSnapshot> {
        let store = self.state.store_for_workspace(workspace_id).await?;
        let task = store.get_task(task_id).await?;
        let sessions = store.list_sessions_for_task(task_id).await?;
        let task_count = store.list_tasks(workspace_id).await?.len();
        let worktree_count = store.list_worktrees(workspace_id).await?.len();
        Ok(TaskDefaultSessionSnapshot {
            task,
            sessions,
            task_count,
            worktree_count,
        })
    }

    pub async fn task_default_workspace_counts_for_test(
        &self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<(usize, usize)> {
        let store = self.state.store_for_workspace(workspace_id).await?;
        Ok((
            store.list_tasks(workspace_id).await?.len(),
            store.list_worktrees(workspace_id).await?.len(),
        ))
    }

    pub async fn task_archive_managed_worktrees_snapshot_for_test(
        &self,
        workspace_id: WorkspaceId,
        task_id: TaskId,
    ) -> anyhow::Result<TaskArchiveManagedWorktreesSnapshot> {
        let workspace = self
            .state
            .global_store()
            .get_workspace(workspace_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("workspace {workspace_id:?} not found"))?;
        let store = self.state.store_for_workspace(workspace_id).await?;
        let task = store
            .get_task(task_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("task {task_id:?} not found"))?;
        let sessions = store.list_sessions_for_task(task_id).await?;

        let mut worktree_ids: std::collections::HashSet<WorktreeId> =
            sessions.iter().map(|session| session.worktree_id).collect();
        if let Some(primary_worktree_id) = task.primary_worktree_id {
            worktree_ids.insert(primary_worktree_id);
        }
        if worktree_ids.is_empty() {
            anyhow::bail!("task {task_id:?} has no worktrees to archive");
        }

        let worktree_count = worktree_ids.len();
        let mut managed_roots = Vec::new();
        let mut managed_branches = Vec::new();
        let mut managed_worktree_count = 0;
        for worktree_id in worktree_ids {
            let worktree = store
                .get_worktree(worktree_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("worktree {worktree_id:?} not found"))?;
            let managed_root = daemon::workspaces::managed_worktree_root(
                self.state.as_ref(),
                &workspace,
                &worktree,
            )
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "worktree {:?} for task {:?} is not managed",
                    worktree.id,
                    task_id
                )
            })?;
            let branch = worktree.git_branch.clone().ok_or_else(|| {
                anyhow::anyhow!(
                    "managed worktree {:?} for task {:?} has no git branch",
                    worktree.id,
                    task_id
                )
            })?;
            managed_roots.push(managed_root);
            managed_branches.push(branch);
            managed_worktree_count += 1;
        }

        managed_roots.sort();
        managed_roots.dedup();
        managed_branches.sort();
        managed_branches.dedup();

        Ok(TaskArchiveManagedWorktreesSnapshot {
            session_count: sessions.len(),
            worktree_count,
            managed_worktree_count,
            managed_roots,
            managed_branches,
        })
    }

    pub async fn seed_task_default_session_task_for_test(
        &self,
        workspace_id: WorkspaceId,
        title: &str,
    ) -> anyhow::Result<Task> {
        let store = self.state.store_for_workspace(workspace_id).await?;
        let task = store
            .create_task(workspace_id, title.to_string(), None)
            .await?;
        self.state
            .global_store()
            .upsert_workspace_task_index(task.id, workspace_id)
            .await?;
        Ok(task)
    }

    pub async fn hold_task_session_creation_lock_for_test(
        &self,
        task_id: TaskId,
    ) -> TaskSessionCreationLockGuardForTest {
        let lock = self
            .state
            .sessions
            .task_session_creation_lock(task_id)
            .await;
        let guard = lock.clone().lock_owned().await;
        TaskSessionCreationLockGuardForTest {
            _lock: lock,
            _guard: guard,
        }
    }

    pub async fn wait_for_task_persisted_for_test(
        &self,
        workspace_id: WorkspaceId,
        task_id: TaskId,
        timeout: Duration,
    ) -> anyhow::Result<()> {
        let store = self.state.store_for_workspace(workspace_id).await?;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if store.get_task(task_id).await?.is_some() {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("task {task_id:?} was not persisted before timeout");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    pub async fn simulate_missing_workspace_task_index_for_test(
        &self,
        task_id: TaskId,
    ) -> anyhow::Result<()> {
        self.state
            .global_store()
            .delete_workspace_task_index(task_id)
            .await
            .map_err(Into::into)
    }

    pub async fn seed_task_lifecycle_workspace_for_test(
        &self,
        name: &str,
        root_path: &Path,
        vcs_kind: VcsKind,
    ) -> anyhow::Result<Workspace> {
        let workspace = self
            .state
            .global_store()
            .create_workspace(
                name.to_string(),
                root_path.to_string_lossy().to_string(),
                vcs_kind,
            )
            .await?;
        let _ = self.state.store_for_workspace(workspace.id).await?;
        Ok(workspace)
    }

    pub async fn seed_task_lifecycle_task_for_test(
        &self,
        workspace_id: WorkspaceId,
        title: &str,
    ) -> anyhow::Result<Task> {
        let store = self.state.store_for_workspace(workspace_id).await?;
        let task = store
            .create_task(workspace_id, title.to_string(), None)
            .await?;
        self.state
            .global_store()
            .upsert_workspace_task_index(task.id, workspace_id)
            .await?;
        Ok(task)
    }

    pub async fn seed_task_lifecycle_stale_task_index_for_test(
        &self,
        task_id: TaskId,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<()> {
        self.state
            .global_store()
            .upsert_workspace_task_index(task_id, workspace_id)
            .await?;
        Ok(())
    }

    pub async fn seed_task_lifecycle_worktree_for_test(
        &self,
        seed: TaskLifecycleWorktreeSeed,
    ) -> anyhow::Result<Worktree> {
        let store = self.state.store_for_workspace(seed.workspace_id).await?;
        let worktree = store
            .insert_worktree(Worktree {
                id: seed.worktree_id,
                workspace_id: seed.workspace_id,
                root_path: seed.root_path.to_string_lossy().to_string(),
                base_commit_sha: seed.base_commit.clone(),
                git_branch: Some(seed.git_branch),
                vcs_kind: Some(VcsKind::Git),
                base_revision: Some(seed.base_commit),
                vcs_ref: Some(String::new()),
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
        self.state
            .global_store()
            .upsert_workspace_worktree_index(worktree.id, seed.workspace_id)
            .await?;
        if seed.make_primary {
            store
                .set_task_primary_worktree(seed.owner_task_id, worktree.id)
                .await?;
        }
        Ok(worktree)
    }

    pub async fn seed_task_lifecycle_sandbox_binding_for_test(
        &self,
        seed: TaskLifecycleSandboxBindingSeed,
    ) -> anyhow::Result<()> {
        let store = self.state.store_for_workspace(seed.workspace_id).await?;
        store
            .upsert_sandbox_binding(SandboxBinding {
                worktree_id: seed.worktree_id,
                workspace_id: seed.workspace_id,
                sandbox_instance_id: ctx_core::models::sandbox_instance_id_for_workspace(
                    seed.workspace_id,
                ),
                substrate: seed.substrate,
                guest_identity: SandboxGuestIdentity::linux_container_ubuntu(),
                profile: SandboxProfile::Standard,
                live_workspace_root: seed.live_workspace_root,
                live_worktree_root: seed.live_worktree_root,
                execution_settings_json: seed.execution_settings_json,
                container_name: seed.container_name,
                host_materialization_root: seed
                    .host_materialization_root
                    .map(|path| path.to_string_lossy().to_string()),
                created_at: chrono::Utc::now(),
            })
            .await?;
        Ok(())
    }

    pub async fn save_task_lifecycle_execution_settings_for_test(
        &self,
        execution: ExecutionSettings,
    ) -> anyhow::Result<()> {
        self.save_execution_settings_for_test(execution).await
    }

    pub async fn task_lifecycle_effective_execution_settings_for_test(
        &self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<ExecutionSettings> {
        crate::daemon::execution_effective::effective_execution_settings(
            self.state.as_ref(),
            workspace_id,
        )
        .await
        .map_err(Into::into)
    }

    pub async fn restart_provider_for_auth_change_for_test(
        &self,
        provider_id: &str,
        reason: &str,
    ) -> anyhow::Result<()> {
        crate::daemon::providers::restart_provider_for_auth_change_with_runtime(
            &self.state.providers,
            provider_id,
            reason,
        )
        .await
    }

    pub async fn seed_task_lifecycle_session_for_test(
        &self,
        seed: TaskLifecycleSessionSeed,
    ) -> anyhow::Result<Session> {
        let store = self.state.store_for_workspace(seed.workspace_id).await?;
        let session = store
            .create_session(
                seed.task_id,
                seed.workspace_id,
                seed.worktree_id,
                seed.execution_environment,
                "fake".to_string(),
                "model".to_string(),
                seed.title,
                seed.parent_session_id,
                seed.role,
                None,
            )
            .await?;
        self.state
            .global_store()
            .upsert_workspace_session_index(session.id, seed.workspace_id)
            .await?;
        Ok(session)
    }

    pub async fn archive_task_lifecycle_row_for_test(
        &self,
        workspace_id: WorkspaceId,
        task_id: TaskId,
    ) -> anyhow::Result<bool> {
        let store = self.state.store_for_workspace(workspace_id).await?;
        store.archive_task(task_id).await.map_err(Into::into)
    }

    pub async fn archive_task_lifecycle_subagent_session_for_test(
        &self,
        workspace_id: WorkspaceId,
        parent_session_id: SessionId,
        child_session_id: SessionId,
    ) -> anyhow::Result<bool> {
        let store = self.state.store_for_workspace(workspace_id).await?;
        store
            .archive_subagent_session(parent_session_id, child_session_id)
            .await
            .map_err(Into::into)
    }

    pub async fn task_lifecycle_snapshot_for_test(
        &self,
        workspace_id: WorkspaceId,
        task_id: TaskId,
        worktree_id: WorktreeId,
    ) -> anyhow::Result<TaskLifecycleSnapshot> {
        let store = self.state.store_for_workspace(workspace_id).await?;
        Ok(TaskLifecycleSnapshot {
            task: store.get_task(task_id).await?,
            worktree: store.get_worktree(worktree_id).await?,
            worktree_index_workspace_id: self
                .state
                .global_store()
                .get_workspace_id_for_worktree(worktree_id)
                .await?,
            sandbox_binding: store.get_sandbox_binding(worktree_id).await?,
        })
    }

    pub async fn seed_global_id_routing_workspace_session_for_test(
        &self,
        seed: GlobalIdRoutingWorkspaceSessionSeed,
    ) -> anyhow::Result<GlobalIdRoutingSessionFixture> {
        let workspace = self
            .state
            .global_store()
            .create_workspace(
                seed.name.clone(),
                seed.root_path.to_string_lossy().to_string(),
                VcsKind::Git,
            )
            .await?;
        let store = self.state.store_for_workspace(workspace.id).await?;
        let worktree = store
            .create_worktree(
                workspace.id,
                seed.root_path.to_string_lossy().to_string(),
                seed.base_commit,
                None,
            )
            .await?;
        let task = store.create_task(workspace.id, seed.name, None).await?;
        let session = store
            .create_session(
                task.id,
                workspace.id,
                worktree.id,
                ExecutionEnvironment::Host,
                seed.provider_id,
                seed.model_id,
                "assistant".to_string(),
                None,
                None,
                None,
            )
            .await?;

        self.state
            .global_store()
            .upsert_workspace_task_index(task.id, workspace.id)
            .await?;
        self.state
            .global_store()
            .upsert_workspace_worktree_index(worktree.id, workspace.id)
            .await?;
        self.state
            .global_store()
            .upsert_workspace_session_index(session.id, workspace.id)
            .await?;

        Ok(GlobalIdRoutingSessionFixture {
            session_id: session.id,
        })
    }

    pub async fn seed_global_id_routing_queued_message_for_test(
        &self,
        session_id: SessionId,
        content: &str,
    ) -> anyhow::Result<MessageId> {
        let store = self.state.store_for_session(session_id).await?;
        let session = store
            .get_session(session_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("session {session_id:?} not found"))?;
        let message = store
            .insert_message(Message {
                id: MessageId::new(),
                session_id: session.id,
                task_id: session.task_id,
                run_id: None,
                turn_id: None,
                turn_sequence: None,
                order_seq: None,
                role: MessageRole::User,
                content: content.to_string(),
                attachments: Vec::new(),
                delivery: MessageDelivery::Queued,
                delivered_at: None,
                created_at: chrono::Utc::now(),
            })
            .await?;
        Ok(message.id)
    }

    pub async fn global_id_routing_message_exists_for_test(
        &self,
        session_id: SessionId,
        message_id: MessageId,
    ) -> anyhow::Result<bool> {
        let store = self.state.store_for_session(session_id).await?;
        Ok(store
            .get_message(message_id)
            .await?
            .is_some_and(|message| message.session_id == session_id))
    }
}
