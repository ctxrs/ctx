use std::path::Path as StdPath;

use chrono::{DateTime, Utc};
use ctx_core::models::{SandboxBinding, Workspace, Worktree};
use ctx_sandbox_contract::sandbox_execution_settings_from_binding;
use ctx_settings_model::ExecutionSettings;

use crate::daemon::DaemonState;

pub async fn materialize_sandbox_binding_for_worktree(
    state: &DaemonState,
    workspace: &Workspace,
    worktree: &Worktree,
    canonical_root: &StdPath,
    effective: &ExecutionSettings,
    created_at: DateTime<Utc>,
) -> anyhow::Result<Option<SandboxBinding>> {
    ctx_workspace_runtime::materialize_sandbox_binding(
        ctx_workspace_runtime::MaterializeSandboxBindingParams {
            data_root: &state.core.data_root,
            daemon_url: &state.core.daemon_url,
            harness: state.execution.harness.as_ref(),
            workspace,
            worktree,
            canonical_root,
            effective,
            created_at,
        },
    )
    .await
}

pub async fn rematerialize_sandbox_binding_for_worktree(
    state: &DaemonState,
    workspace: &Workspace,
    worktree: &Worktree,
    existing_binding: &SandboxBinding,
) -> anyhow::Result<SandboxBinding> {
    let canonical_root = super::managed_worktree_root(state, workspace, worktree)
        .ok_or_else(|| anyhow::anyhow!("worktree is not a managed ctx worktree"))?;
    materialize_sandbox_binding_for_worktree(
        state,
        workspace,
        worktree,
        &canonical_root,
        &sandbox_execution_settings_from_binding(existing_binding)?,
        existing_binding.created_at,
    )
    .await?
    .ok_or_else(|| anyhow::anyhow!("sandbox binding rematerialization produced host mode"))
}
