use std::sync::Arc;

use anyhow::Context;
use ctx_core::ids::WorktreeId;
use ctx_core::models::{ExecutionEnvironment, Workspace, Worktree};
use ctx_settings_model::{ExecutionMode, ExecutionSettings};
use ctx_store::Store;
use ctx_worktree_data_plane::apply_data_plane_to_execution_settings;

use crate::daemon::DaemonState;

#[cfg(test)]
mod tests;

pub struct ResolvedExistingWorktreeExecution {
    pub worktree: Worktree,
    pub effective: ExecutionSettings,
}

impl ResolvedExistingWorktreeExecution {
    pub fn execution_environment(&self) -> ExecutionEnvironment {
        execution_environment_from_settings(&self.effective)
    }
}

pub fn execution_environment_from_settings(settings: &ExecutionSettings) -> ExecutionEnvironment {
    match settings.mode {
        ExecutionMode::Host => ExecutionEnvironment::Host,
        ExecutionMode::Sandbox => ExecutionEnvironment::Sandbox,
    }
}

pub async fn resolve_existing_worktree_execution(
    state: &Arc<DaemonState>,
    store: &Store,
    workspace: &Workspace,
    worktree_id: WorktreeId,
) -> anyhow::Result<ResolvedExistingWorktreeExecution> {
    let worktree = store
        .get_worktree(worktree_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("worktree not found"))?;
    let base_effective =
        super::super::execution_effective::effective_execution_settings(state, workspace.id)
            .await
            .context("loading workspace execution settings")?;
    let data_plane =
        ctx_worktree_data_plane::resolve_worktree_data_plane_with_host(state.as_ref(), &worktree)
            .await
            .context("resolving worktree data plane")?;
    let effective = apply_data_plane_to_execution_settings(&base_effective, &data_plane)
        .context("applying worktree data plane to execution settings")?;
    Ok(ResolvedExistingWorktreeExecution {
        worktree,
        effective,
    })
}
