use std::path::Path;

use anyhow::Result;
use async_trait::async_trait;
use ctx_core::ids::WorkspaceId;
use ctx_core::models::{Workspace, Worktree};
use ctx_store::Store;
use ctx_worktree_data_plane::WorktreeDataPlaneHost;

use crate::daemon::{ProviderWorkspaceLaunchRuntime, SessionTitleModelModeHandle};
use ctx_observability::logs;
use ctx_provider_runtime::provider_launch::probe::PreparedWorkspaceProbeRuntime;

mod runtime;

#[async_trait]
impl WorktreeDataPlaneHost for ProviderWorkspaceLaunchRuntime {
    async fn get_workspace(state: &Self, workspace_id: WorkspaceId) -> Result<Option<Workspace>> {
        state.load_workspace(workspace_id).await
    }

    async fn workspace_store(state: &Self, workspace_id: WorkspaceId) -> Result<Store> {
        state.store_for_workspace(workspace_id).await
    }
}

#[async_trait]
impl ctx_provider_runtime::provider_launch::probe::ProviderProbeHost
    for ProviderWorkspaceLaunchRuntime
{
    fn data_root(&self) -> &Path {
        self.data_root()
    }

    fn daemon_url(&self) -> &str {
        self.daemon_url()
    }

    fn auth_token(&self) -> Option<&String> {
        self.auth_token()
    }

    fn redact_sensitive(&self, input: &str) -> String {
        logs::redact_sensitive(input)
    }

    async fn load_workspace(&self, workspace_id: WorkspaceId) -> Result<Option<Workspace>, String> {
        self.load_workspace(workspace_id)
            .await
            .map_err(|err| logs::redact_sensitive(&format!("loading workspace failed: {err:#}")))
    }

    async fn prepare_workspace_probe_runtime(
        &self,
        workspace: &Workspace,
    ) -> Result<PreparedWorkspaceProbeRuntime, String> {
        runtime::prepare_workspace_probe_runtime_parts(
            self,
            self.global_store(),
            self.data_root(),
            self.daemon_url(),
            self.harness(),
            workspace,
        )
        .await
    }

    async fn prepare_worktree_probe_runtime(
        &self,
        workspace: &Workspace,
        worktree: &Worktree,
    ) -> Result<PreparedWorkspaceProbeRuntime, String> {
        runtime::prepare_worktree_probe_runtime_parts(
            self,
            self.global_store(),
            self.daemon_url(),
            self.harness(),
            workspace,
            worktree,
        )
        .await
    }
}

#[async_trait]
impl ctx_provider_runtime::provider_launch::probe::ProviderProbeHost
    for SessionTitleModelModeHandle
{
    fn data_root(&self) -> &Path {
        self.data_root()
    }

    fn daemon_url(&self) -> &str {
        self.daemon_url()
    }

    fn auth_token(&self) -> Option<&String> {
        self.auth_token()
    }

    fn redact_sensitive(&self, input: &str) -> String {
        logs::redact_sensitive(input)
    }

    async fn load_workspace(&self, workspace_id: WorkspaceId) -> Result<Option<Workspace>, String> {
        self.get_workspace(workspace_id)
            .await
            .map_err(|err| logs::redact_sensitive(&format!("loading workspace failed: {err:#}")))
    }

    async fn prepare_workspace_probe_runtime(
        &self,
        workspace: &Workspace,
    ) -> Result<PreparedWorkspaceProbeRuntime, String> {
        runtime::prepare_workspace_probe_runtime_parts(
            self,
            self.global_store(),
            self.data_root(),
            self.daemon_url(),
            self.harness(),
            workspace,
        )
        .await
    }

    async fn prepare_worktree_probe_runtime(
        &self,
        workspace: &Workspace,
        worktree: &Worktree,
    ) -> Result<PreparedWorkspaceProbeRuntime, String> {
        runtime::prepare_worktree_probe_runtime_parts(
            self,
            self.global_store(),
            self.daemon_url(),
            self.harness(),
            workspace,
            worktree,
        )
        .await
    }
}
