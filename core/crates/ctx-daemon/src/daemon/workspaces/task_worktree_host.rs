use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};
use async_trait::async_trait;
use chrono::Utc;
use ctx_core::ids::{TaskId, WorkspaceId, WorktreeId};
use ctx_core::models::{
    SandboxBinding, Workspace, Worktree, WorktreeBootstrapNotice, WorktreeBootstrapStatus,
};
use ctx_sandbox_contract::sandbox_execution_settings_from_binding;
use ctx_settings_model::{ContainerRuntimeKind, ExecutionMode, ExecutionSettings};
use ctx_store::{Store, WorktreeBootstrapResultUpdate};
use ctx_workspace_active_snapshot::WorkspaceActiveSnapshotHub;
use ctx_workspace_runtime::HarnessRuntimeManager;
use ctx_worktree_bootstrap_service::{
    bootstrap_command_env, cleanup_command_env, normalize_bootstrap_config,
    normalize_cleanup_config, prepare_bootstrap_log_for_storage, run_bootstrap_command,
    shell_bootstrap_command, write_bootstrap_log, BootstrapCommandResult,
    BootstrapCommandRuntime, BootstrapConfig, BootstrapConfigInput, BootstrapReport, BootstrapStep,
    CleanupConfigInput, WorktreeBootstrapHost,
};
use ctx_worktree_data_plane::{
    apply_data_plane_to_execution_settings, resolve_worktree_data_plane_with_host,
    WorktreeDataPlaneHost,
};
use tokio::sync::{watch, Mutex};

use crate::daemon::state::{TimedEntry, WorktreeBootstrapGate};
use crate::daemon::workspaces::attachments::WorkspaceAttachmentsRuntime;
use crate::daemon::workspaces::vcs_hooks::WorkspaceVcsHookHost;
use crate::daemon::ProtectedWorkspaceStoreLookup;

type WorktreeBootstrapGates = Arc<Mutex<HashMap<WorktreeId, TimedEntry<WorktreeBootstrapGate>>>>;

#[derive(Clone)]
pub(in crate::daemon) struct TaskWorktreeHost {
    data_root: PathBuf,
    daemon_url: String,
    global_store: Store,
    workspace_stores: ProtectedWorkspaceStoreLookup,
    harness: Arc<HarnessRuntimeManager>,
    active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    bootstrap_gates: WorktreeBootstrapGates,
    attachments: Arc<WorkspaceAttachmentsRuntime>,
    vcs_hooks: Arc<WorkspaceVcsHookHost>,
}

pub(in crate::daemon) struct TaskWorktreeHostParts {
    pub(in crate::daemon) data_root: PathBuf,
    pub(in crate::daemon) daemon_url: String,
    pub(in crate::daemon) global_store: Store,
    pub(in crate::daemon) workspace_stores: ProtectedWorkspaceStoreLookup,
    pub(in crate::daemon) harness: Arc<HarnessRuntimeManager>,
    pub(in crate::daemon) active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    pub(in crate::daemon) bootstrap_gates: WorktreeBootstrapGates,
    pub(in crate::daemon) attachments: Arc<WorkspaceAttachmentsRuntime>,
    pub(in crate::daemon) vcs_hooks: Arc<WorkspaceVcsHookHost>,
}

impl TaskWorktreeHost {
    pub(in crate::daemon) fn new(parts: TaskWorktreeHostParts) -> Self {
        Self {
            data_root: parts.data_root,
            daemon_url: parts.daemon_url,
            global_store: parts.global_store,
            workspace_stores: parts.workspace_stores,
            harness: parts.harness,
            active_snapshot: parts.active_snapshot,
            bootstrap_gates: parts.bootstrap_gates,
            attachments: parts.attachments,
            vcs_hooks: parts.vcs_hooks,
        }
    }

    async fn workspace_store(&self, workspace_id: WorkspaceId) -> anyhow::Result<Store> {
        self.workspace_stores
            .store_for_workspace(workspace_id)
            .await
    }

    async fn effective_execution_settings(
        &self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<ExecutionSettings> {
        let store = self.workspace_store(workspace_id).await?;
        ctx_settings_service::effective_execution_settings(&self.global_store, &store).await
    }

    pub(in crate::daemon) async fn resolve_existing_worktree_execution(
        &self,
        store: &Store,
        workspace: &Workspace,
        worktree_id: WorktreeId,
    ) -> anyhow::Result<crate::daemon::workspaces::ResolvedExistingWorktreeExecution> {
        let worktree = store
            .get_worktree(worktree_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("worktree not found"))?;
        let base_effective = self
            .effective_execution_settings(workspace.id)
            .await
            .context("loading workspace execution settings")?;
        let data_plane = resolve_worktree_data_plane_with_host(self, &worktree)
            .await
            .context("resolving worktree data plane")?;
        let effective = apply_data_plane_to_execution_settings(&base_effective, &data_plane)
            .context("applying worktree data plane to execution settings")?;
        Ok(
            crate::daemon::workspaces::ResolvedExistingWorktreeExecution {
                worktree,
                effective,
            },
        )
    }

    pub(in crate::daemon) async fn provision_worktree_for_execution(
        &self,
        workspace: &Workspace,
        worktree_id: WorktreeId,
        base_commit_sha: &str,
        branch_name: &str,
        effective: &ExecutionSettings,
    ) -> anyhow::Result<(PathBuf, Option<SandboxBinding>)> {
        let canonical_root = ctx_worktree_vcs_service::create_managed_worktree(
            &self.data_root,
            &workspace.root_path,
            workspace.id,
            worktree_id,
            base_commit_sha,
            branch_name,
        )
        .await?;

        let created_at = Utc::now();
        let worktree = ctx_worktree_vcs_service::managed_worktree_record(
            workspace.id,
            worktree_id,
            &canonical_root,
            base_commit_sha,
            branch_name,
            created_at,
        );
        let binding = ctx_workspace_runtime::materialize_sandbox_binding(
            ctx_workspace_runtime::MaterializeSandboxBindingParams {
                data_root: &self.data_root,
                daemon_url: &self.daemon_url,
                harness: self.harness.as_ref(),
                workspace,
                worktree: &worktree,
                canonical_root: &canonical_root,
                effective,
                created_at,
            },
        )
        .await?;

        Ok((canonical_root, binding))
    }

    pub(in crate::daemon) async fn persist_provisioned_worktree(
        self: &Arc<Self>,
        store: &Store,
        workspace: &Workspace,
        worktree: Worktree,
        sandbox_binding: Option<SandboxBinding>,
    ) -> anyhow::Result<Worktree> {
        store.insert_worktree(worktree.clone()).await?;
        if let Some(binding) = sandbox_binding {
            store.upsert_sandbox_binding(binding).await?;
        }
        crate::daemon::workspaces::retry_global_index_write(|| async {
            self.global_store
                .upsert_workspace_worktree_index(worktree.id, workspace.id)
                .await
        })
        .await?;

        if let Err(err) = ctx_worktree_bootstrap_service::spawn_worktree_bootstrap(
            Arc::clone(self),
            workspace.clone(),
            worktree.clone(),
        )
        .await
        {
            tracing::warn!(worktree_id = %worktree.id.0, "worktree bootstrap failed: {err:?}");
        }
        if let Err(err) = self
            .attachments
            .sync_workspace_attachments(workspace, false)
            .await
        {
            tracing::warn!(worktree_id = %worktree.id.0, "attachment sync failed: {err:?}");
        }
        if let Err(err) = self
            .attachments
            .ensure_worktree_attachment_mounts_if_materialized(workspace, &worktree)
            .await
        {
            tracing::warn!(worktree_id = %worktree.id.0, "attachment mounts failed: {err:?}");
        }

        Ok(worktree)
    }

    pub(in crate::daemon) async fn ensure_task_commit_hook(
        &self,
        workspace: &Workspace,
        worktree: &Worktree,
        task_id: TaskId,
    ) -> anyhow::Result<()> {
        ctx_worktree_vcs_service::ensure_task_commit_hook(
            self.vcs_hooks.as_ref(),
            workspace,
            worktree,
            task_id,
        )
        .await
    }

    pub(in crate::daemon) fn managed_worktree_root(
        &self,
        workspace: &Workspace,
        worktree: &Worktree,
    ) -> Option<PathBuf> {
        ctx_worktree_vcs_service::matching_managed_worktree_path(
            &self.data_root,
            workspace.id,
            worktree.id,
            PathBuf::from(&worktree.root_path),
        )
    }

    pub(in crate::daemon) async fn cleanup_task_worktrees(
        &self,
        workspace: &Workspace,
        task_id: TaskId,
        targets: &[crate::daemon::workspaces::TaskWorktreeCleanupTarget],
        mode: crate::daemon::workspaces::BranchCleanupErrorMode,
    ) -> Vec<anyhow::Error> {
        crate::daemon::workspaces::cleanup_task_worktrees_with_host(
            &self.data_root,
            self.vcs_hooks.as_ref(),
            workspace,
            task_id,
            targets,
            mode,
        )
        .await
    }

    pub(in crate::daemon) async fn rematerialize_sandbox_binding_for_worktree(
        &self,
        workspace: &Workspace,
        worktree: &Worktree,
        existing_binding: &SandboxBinding,
    ) -> anyhow::Result<SandboxBinding> {
        let canonical_root = self
            .managed_worktree_root(workspace, worktree)
            .ok_or_else(|| anyhow::anyhow!("worktree is not a managed ctx worktree"))?;
        ctx_workspace_runtime::materialize_sandbox_binding(
            ctx_workspace_runtime::MaterializeSandboxBindingParams {
                data_root: &self.data_root,
                daemon_url: &self.daemon_url,
                harness: self.harness.as_ref(),
                workspace,
                worktree,
                canonical_root: canonical_root.as_path(),
                effective: &sandbox_execution_settings_from_binding(existing_binding)?,
                created_at: existing_binding.created_at,
            },
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("sandbox binding rematerialization produced host mode"))
    }

    pub(in crate::daemon) async fn ensure_worktree_attachment_mounts_if_materialized(
        &self,
        workspace: &Workspace,
        worktree: &Worktree,
    ) -> anyhow::Result<()> {
        self.attachments
            .ensure_worktree_attachment_mounts_if_materialized(workspace, worktree)
            .await
            .map(|_| ())
    }

    pub(in crate::daemon) async fn spawn_worktree_bootstrap(
        self: &Arc<Self>,
        workspace: &Workspace,
        worktree: &Worktree,
    ) -> anyhow::Result<()> {
        ctx_worktree_bootstrap_service::spawn_worktree_bootstrap(
            Arc::clone(self),
            workspace.clone(),
            worktree.clone(),
        )
        .await
    }

    pub(in crate::daemon) async fn run_worktree_cleanup(
        &self,
        workspace: &Workspace,
        task_id: TaskId,
        worktree: &Worktree,
    ) -> anyhow::Result<bool> {
        let store = self.workspace_store(workspace.id).await?;
        let Some(cfg) = ctx_workspace_config::load_worktree_bootstrap_config(&store).await? else {
            return Ok(false);
        };
        let Some(cleanup) = normalize_cleanup_config(CleanupConfigInput {
            cleanup_command: cfg.cleanup_command,
            cleanup_timeout_sec: cfg.cleanup_timeout_sec,
        }) else {
            return Ok(false);
        };

        let result = self
            .execute_lifecycle_shell_command(
                workspace,
                worktree,
                &cleanup.command,
                cleanup.timeout,
                Some(task_id),
            )
            .await?;
        if result.timed_out {
            bail!(
                "worktree cleanup timed out after {}s",
                cleanup.timeout.as_secs()
            );
        }
        match result.exit_code {
            Some(0) => Ok(true),
            Some(code) => bail!("worktree cleanup exited with code {code}"),
            None => bail!("worktree cleanup terminated before reporting an exit code"),
        }
    }

    async fn execute_lifecycle_shell_command(
        &self,
        workspace: &Workspace,
        worktree: &Worktree,
        command: &str,
        timeout: Duration,
        cleanup_task_id: Option<TaskId>,
    ) -> anyhow::Result<BootstrapCommandResult> {
        let data_plane = resolve_worktree_data_plane_with_host(self, worktree).await?;
        let settings = self.effective_execution_settings(workspace.id).await?;
        let settings = apply_data_plane_to_execution_settings(&settings, &data_plane)?;
        let env = if let Some(task_id) = cleanup_task_id {
            cleanup_command_env(
                worktree,
                task_id,
                &data_plane.live_workspace_root,
                &data_plane.live_worktree_root,
            )
        } else {
            bootstrap_command_env(
                worktree,
                &data_plane.live_workspace_root,
                &data_plane.live_worktree_root,
            )
        };

        if matches!(settings.mode, ExecutionMode::Sandbox) {
            self.harness
                .ensure_workspace_container_for_worktree(
                    workspace,
                    worktree,
                    &settings,
                    &self.daemon_url,
                )
                .await?;
            let cmd = match settings.container.runtime {
                ContainerRuntimeKind::NativeContainer => {
                    let container_name =
                        ctx_workspace_container::workspace_container_name(workspace.id);
                    let mut cmd = ctx_harness_runtime::sandbox_container_command(&self.data_root)?;
                    cmd.arg("exec")
                        .arg("--workdir")
                        .arg(&data_plane.live_worktree_root);
                    for (key, value) in &env {
                        cmd.arg("--env").arg(format!("{key}={value}"));
                    }
                    cmd.arg(container_name).arg("sh").arg("-lc").arg(command);
                    cmd
                }
                ContainerRuntimeKind::SharedVmContainer => {
                    ctx_avf_linux_runtime::build_guest_exec_command(
                        &self.data_root,
                        workspace.id,
                        worktree.id,
                        &data_plane.live_worktree_root,
                        "sh",
                        &["-lc".to_string(), command.to_string()],
                        &env,
                        None,
                        false,
                    )?
                }
            };
            return run_bootstrap_command(cmd, timeout, BootstrapCommandRuntime::Container).await;
        }

        let mut cmd = shell_bootstrap_command(command);
        cmd.current_dir(&data_plane.live_worktree_root);
        for (key, value) in env {
            cmd.env(key, value);
        }
        run_bootstrap_command(cmd, timeout, BootstrapCommandRuntime::Host).await
    }
}

#[async_trait]
impl WorktreeDataPlaneHost for TaskWorktreeHost {
    async fn get_workspace(
        host: &Self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<Option<Workspace>> {
        host.global_store.get_workspace(workspace_id).await
    }

    async fn workspace_store(host: &Self, workspace_id: WorkspaceId) -> anyhow::Result<Store> {
        host.workspace_store(workspace_id).await
    }
}

#[async_trait]
impl WorktreeBootstrapHost for TaskWorktreeHost {
    async fn load_bootstrap_config(
        &self,
        workspace: &Workspace,
    ) -> anyhow::Result<Option<BootstrapConfig>> {
        let store = self.workspace_store(workspace.id).await?;
        let Some(cfg) = ctx_workspace_config::load_worktree_bootstrap_config(&store).await? else {
            return Ok(None);
        };

        Ok(normalize_bootstrap_config(BootstrapConfigInput {
            setup_command: cfg.setup_command,
            timeout_sec: cfg.timeout_sec,
            wait_for_completion: cfg.wait_for_completion,
        }))
    }

    async fn execute_bootstrap_step(
        &self,
        workspace: &Workspace,
        worktree: &Worktree,
        step: &BootstrapStep,
        timeout: Duration,
    ) -> anyhow::Result<BootstrapCommandResult> {
        self.execute_lifecycle_shell_command(workspace, worktree, &step.command, timeout, None)
            .await
    }

    async fn persist_bootstrap_report(
        &self,
        workspace_id: WorkspaceId,
        worktree: &Worktree,
        report: BootstrapReport,
    ) {
        let (log, log_truncated) = prepare_bootstrap_log_for_storage(&report.raw_log);
        let log_path = write_bootstrap_log(&self.data_root, worktree.id, &log)
            .await
            .ok();

        if let Ok(store) = self.workspace_stores.store_for_worktree(worktree.id).await {
            let _ = store
                .update_worktree_bootstrap_result(WorktreeBootstrapResultUpdate {
                    worktree_id: worktree.id,
                    status: report.status.clone(),
                    started_at: report.started_at,
                    finished_at: report.finished_at,
                    exit_code: report.exit_code,
                    timeout_sec: Some(report.timeout_sec),
                    error: report.error.clone(),
                    log_path: log_path.as_ref().map(|p| p.to_string_lossy().to_string()),
                    log_truncated: Some(log_truncated),
                    command: report.command.clone(),
                    script_path: None,
                })
                .await;
        }

        if report.status != WorktreeBootstrapStatus::Success {
            let notice = WorktreeBootstrapNotice {
                worktree_id: worktree.id,
                worktree_root: worktree.root_path.clone(),
                status: report.status,
                started_at: report.started_at,
                finished_at: report.finished_at,
                exit_code: report.exit_code,
                timeout_sec: Some(report.timeout_sec),
                command: report.command,
                script_path: None,
                log_path: log_path.map(|p| p.to_string_lossy().to_string()),
                log_truncated: Some(log_truncated),
                error: report.error,
            };
            self.active_snapshot
                .publish_worktree_bootstrap(workspace_id, notice)
                .await;
        }
    }

    async fn register_bootstrap(&self, worktree_id: WorktreeId, wait_for_completion: bool) {
        let (done_tx, _) = watch::channel(false);
        let mut map = self.bootstrap_gates.lock().await;
        map.insert(
            worktree_id,
            TimedEntry::new(WorktreeBootstrapGate {
                wait_for_completion,
                done_tx,
            }),
        );
    }

    async fn finish_bootstrap(&self, worktree_id: WorktreeId) {
        let gate = {
            let mut map = self.bootstrap_gates.lock().await;
            map.remove(&worktree_id)
        };
        if let Some(gate) = gate {
            let _ = gate.value.done_tx.send(true);
        }
    }
}
