use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use ctx_core::ids::WorkspaceId;
use ctx_core::models::{Workspace, Worktree};
use ctx_settings_model::{ContainerRuntimeKind, ExecutionMode};
use ctx_store::Store;
use ctx_workspace_runtime::HarnessRuntimeManager;
use ctx_worktree_data_plane::apply_data_plane_to_execution_settings;
use ctx_worktree_data_plane::resolve_worktree_data_plane_with_host as resolve_worktree_data_plane;
use ctx_worktree_data_plane::WorktreeDataPlaneHost;
use ctx_worktree_vcs_service::{
    SandboxContainerRuntime, VcsHooksHost, WorktreeExecutionLocation, WorktreeHookExecution,
};

use crate::daemon::execution_effective;
use crate::daemon::DaemonState;
use crate::daemon::ProtectedWorkspaceStoreLookup;

#[path = "host/git_config.rs"]
mod git_config;

pub(in crate::daemon) struct WorkspaceVcsHookHost {
    data_root: PathBuf,
    daemon_url: String,
    global_store: Store,
    workspace_stores: ProtectedWorkspaceStoreLookup,
    harness: Arc<HarnessRuntimeManager>,
}

impl WorkspaceVcsHookHost {
    pub(in crate::daemon) fn new(
        data_root: PathBuf,
        daemon_url: String,
        global_store: Store,
        workspace_stores: ProtectedWorkspaceStoreLookup,
        harness: Arc<HarnessRuntimeManager>,
    ) -> Self {
        Self {
            data_root,
            daemon_url,
            global_store,
            workspace_stores,
            harness,
        }
    }

    async fn effective_execution_settings(
        &self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<ctx_settings_model::ExecutionSettings> {
        let store = self
            .workspace_stores
            .store_for_workspace(workspace_id)
            .await?;
        ctx_settings_service::effective_execution_settings_classified(&self.global_store, &store)
            .await
            .map_err(ctx_settings_service::EffectiveExecutionSettingsError::into_inner)
    }
}

#[async_trait]
impl WorktreeDataPlaneHost for WorkspaceVcsHookHost {
    async fn get_workspace(state: &Self, workspace_id: WorkspaceId) -> Result<Option<Workspace>> {
        state.global_store.get_workspace(workspace_id).await
    }

    async fn workspace_store(state: &Self, workspace_id: WorkspaceId) -> Result<Store> {
        state
            .workspace_stores
            .store_for_workspace(workspace_id)
            .await
    }
}

#[async_trait]
impl VcsHooksHost for WorkspaceVcsHookHost {
    fn data_root(&self) -> &Path {
        &self.data_root
    }

    async fn worktree_execution(
        &self,
        workspace: &Workspace,
        worktree: &Worktree,
    ) -> Result<WorktreeHookExecution> {
        let data_plane = resolve_worktree_data_plane(self, worktree).await?;
        let settings = self.effective_execution_settings(workspace.id).await?;
        let settings = apply_data_plane_to_execution_settings(&settings, &data_plane)?;
        if matches!(settings.mode, ExecutionMode::Host) {
            return Ok(WorktreeHookExecution {
                location: WorktreeExecutionLocation::Host,
                live_worktree_root: None,
                container_runtime: None,
            });
        }
        Ok(WorktreeHookExecution {
            location: WorktreeExecutionLocation::Sandbox,
            live_worktree_root: Some(data_plane.live_worktree_root.to_string_lossy().to_string()),
            container_runtime: Some(match settings.container.runtime {
                ContainerRuntimeKind::NativeContainer => SandboxContainerRuntime::NativeContainer,
                ContainerRuntimeKind::SharedVmContainer => {
                    SandboxContainerRuntime::SharedVmContainer
                }
            }),
        })
    }

    async fn ensure_workspace_container(
        &self,
        workspace: &Workspace,
        worktree: &Worktree,
    ) -> Result<()> {
        let data_plane = resolve_worktree_data_plane(self, worktree).await?;
        let settings = self.effective_execution_settings(workspace.id).await?;
        let settings = apply_data_plane_to_execution_settings(&settings, &data_plane)?;
        self.harness
            .ensure_workspace_container_for_worktree(
                workspace,
                worktree,
                &settings,
                &self.daemon_url,
            )
            .await
    }

    async fn sandbox_git_config_get(
        &self,
        workspace: &Workspace,
        worktree: &Worktree,
        execution: &WorktreeHookExecution,
        key: &str,
    ) -> Result<Option<String>> {
        git_config::sandbox_git_config_get(&self.data_root, workspace, worktree, execution, key)
            .await
    }

    async fn sandbox_git_config_set(
        &self,
        workspace: &Workspace,
        worktree: &Worktree,
        execution: &WorktreeHookExecution,
        key: &str,
        value: &str,
    ) -> Result<()> {
        git_config::sandbox_git_config_set(
            &self.data_root,
            workspace,
            worktree,
            execution,
            key,
            value,
        )
        .await
    }

    async fn sandbox_git_config_unset(
        &self,
        workspace: &Workspace,
        worktree: &Worktree,
        execution: &WorktreeHookExecution,
        key: &str,
    ) -> Result<()> {
        git_config::sandbox_git_config_unset(&self.data_root, workspace, worktree, execution, key)
            .await
    }
}

#[async_trait]
impl VcsHooksHost for DaemonState {
    fn data_root(&self) -> &Path {
        &self.core.data_root
    }

    async fn worktree_execution(
        &self,
        workspace: &Workspace,
        worktree: &Worktree,
    ) -> Result<WorktreeHookExecution> {
        let data_plane = resolve_worktree_data_plane(self, worktree).await?;
        let settings =
            execution_effective::effective_execution_settings(self, workspace.id).await?;
        let settings = apply_data_plane_to_execution_settings(&settings, &data_plane)?;
        if matches!(settings.mode, ExecutionMode::Host) {
            return Ok(WorktreeHookExecution {
                location: WorktreeExecutionLocation::Host,
                live_worktree_root: None,
                container_runtime: None,
            });
        }
        Ok(WorktreeHookExecution {
            location: WorktreeExecutionLocation::Sandbox,
            live_worktree_root: Some(data_plane.live_worktree_root.to_string_lossy().to_string()),
            container_runtime: Some(match settings.container.runtime {
                ContainerRuntimeKind::NativeContainer => SandboxContainerRuntime::NativeContainer,
                ContainerRuntimeKind::SharedVmContainer => {
                    SandboxContainerRuntime::SharedVmContainer
                }
            }),
        })
    }

    async fn ensure_workspace_container(
        &self,
        workspace: &Workspace,
        worktree: &Worktree,
    ) -> Result<()> {
        let data_plane = resolve_worktree_data_plane(self, worktree).await?;
        let settings =
            execution_effective::effective_execution_settings(self, workspace.id).await?;
        let settings = apply_data_plane_to_execution_settings(&settings, &data_plane)?;
        self.execution
            .harness
            .ensure_workspace_container_for_worktree(
                workspace,
                worktree,
                &settings,
                &self.core.daemon_url,
            )
            .await
    }

    async fn sandbox_git_config_get(
        &self,
        workspace: &Workspace,
        worktree: &Worktree,
        execution: &WorktreeHookExecution,
        key: &str,
    ) -> Result<Option<String>> {
        git_config::sandbox_git_config_get(
            &self.core.data_root,
            workspace,
            worktree,
            execution,
            key,
        )
        .await
    }

    async fn sandbox_git_config_set(
        &self,
        workspace: &Workspace,
        worktree: &Worktree,
        execution: &WorktreeHookExecution,
        key: &str,
        value: &str,
    ) -> Result<()> {
        git_config::sandbox_git_config_set(
            &self.core.data_root,
            workspace,
            worktree,
            execution,
            key,
            value,
        )
        .await
    }

    async fn sandbox_git_config_unset(
        &self,
        workspace: &Workspace,
        worktree: &Worktree,
        execution: &WorktreeHookExecution,
        key: &str,
    ) -> Result<()> {
        git_config::sandbox_git_config_unset(
            &self.core.data_root,
            workspace,
            worktree,
            execution,
            key,
        )
        .await
    }
}
