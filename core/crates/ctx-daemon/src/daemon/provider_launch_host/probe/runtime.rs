use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ctx_core::models::{Workspace, Worktree};
use ctx_observability::logs;
use ctx_provider_runtime::provider_launch::probe::PreparedWorkspaceProbeRuntime;
use ctx_settings_model::ExecutionMode;
use ctx_store::Store;
use ctx_workspace_runtime::HarnessRuntimeManager;
use ctx_worktree_data_plane::{
    apply_data_plane_to_execution_settings, resolve_worktree_data_plane_with_host,
    workspace_data_plane, WorktreeDataPlaneHost,
};

use self::helpers::{probe_cwd_for_workspace_runtime, runtime_data_root, synthetic_probe_worktree};

mod helpers;

pub(in crate::daemon) async fn prepare_workspace_probe_runtime_parts<H>(
    data_plane_host: &H,
    global_store: &Store,
    data_root: &Path,
    daemon_url: &str,
    harness: &HarnessRuntimeManager,
    workspace: &Workspace,
) -> Result<PreparedWorkspaceProbeRuntime, String>
where
    H: WorktreeDataPlaneHost,
{
    let store = H::workspace_store(data_plane_host, workspace.id)
        .await
        .map_err(|err| {
            logs::redact_sensitive(&format!("effective execution settings failed: {err}"))
        })?;
    let effective = ctx_settings_service::effective_execution_settings(global_store, &store)
        .await
        .map_err(|err| {
            logs::redact_sensitive(&format!("effective execution settings failed: {err}"))
        })?;
    if matches!(effective.mode, ExecutionMode::Host) {
        return Ok(PreparedWorkspaceProbeRuntime {
            cwd: PathBuf::from(&workspace.root_path),
            runtime_data_root: None,
            env_overrides: HashMap::new(),
        });
    }

    let worktree = synthetic_probe_worktree(workspace);
    let worktree_data_plane = workspace_data_plane(workspace, effective.mode.clone());
    let effective = apply_data_plane_to_execution_settings(&effective, &worktree_data_plane)
        .map_err(|err| {
            logs::redact_sensitive(&format!(
                "applying workspace probe data plane failed: {err:#}"
            ))
        })?;
    let cwd = probe_cwd_for_workspace_runtime(
        &worktree_data_plane,
        &worktree,
        effective.mode.clone(),
        effective.container.mount_mode.clone(),
    );
    let runtime_plan = harness
        .prepare(workspace, &worktree, &effective, daemon_url)
        .await
        .map_err(|err| {
            logs::redact_sensitive(&format!("probe runtime preparation failed: {err:#}"))
        })?;
    let sandbox_mode =
        ctx_harness_runtime::selected_sandbox_command_mode(data_root).map_err(|err| {
            logs::redact_sensitive(&format!("sandbox command selection failed: {err:#}"))
        })?;
    ctx_sandbox_materialization::ensure_workspace_root_from_host_copy(
        data_root,
        &sandbox_mode,
        workspace,
    )
    .await
    .map_err(|err| {
        logs::redact_sensitive(&format!(
            "sandbox workspace root materialization failed: {err:#}"
        ))
    })?;
    let runtime_data_root = runtime_data_root(&runtime_plan.env_overrides);
    Ok(PreparedWorkspaceProbeRuntime {
        cwd,
        runtime_data_root,
        env_overrides: runtime_plan.env_overrides,
    })
}

pub(in crate::daemon) async fn prepare_worktree_probe_runtime_parts<H>(
    data_plane_host: &H,
    global_store: &Store,
    daemon_url: &str,
    harness: &HarnessRuntimeManager,
    workspace: &Workspace,
    worktree: &Worktree,
) -> Result<PreparedWorkspaceProbeRuntime, String>
where
    H: WorktreeDataPlaneHost,
{
    let store = H::workspace_store(data_plane_host, workspace.id)
        .await
        .map_err(|err| {
            logs::redact_sensitive(&format!("effective execution settings failed: {err}"))
        })?;
    let effective = ctx_settings_service::effective_execution_settings(global_store, &store)
        .await
        .map_err(|err| {
            logs::redact_sensitive(&format!("effective execution settings failed: {err}"))
        })?;
    if matches!(effective.mode, ExecutionMode::Host) {
        return Ok(PreparedWorkspaceProbeRuntime {
            cwd: PathBuf::from(&worktree.root_path),
            runtime_data_root: None,
            env_overrides: HashMap::new(),
        });
    }

    let worktree_data_plane = resolve_worktree_data_plane_with_host(data_plane_host, worktree)
        .await
        .map_err(|err| {
            logs::redact_sensitive(&format!(
                "resolving session auth worktree data plane failed: {err:#}"
            ))
        })?;
    let effective = apply_data_plane_to_execution_settings(&effective, &worktree_data_plane)
        .map_err(|err| {
            logs::redact_sensitive(&format!(
                "applying session auth worktree data plane failed: {err:#}"
            ))
        })?;
    let cwd = probe_cwd_for_workspace_runtime(
        &worktree_data_plane,
        worktree,
        effective.mode.clone(),
        effective.container.mount_mode.clone(),
    );
    let runtime_plan = harness
        .prepare(workspace, worktree, &effective, daemon_url)
        .await
        .map_err(|err| {
            logs::redact_sensitive(&format!("session auth runtime preparation failed: {err:#}"))
        })?;
    let runtime_data_root = runtime_data_root(&runtime_plan.env_overrides);
    Ok(PreparedWorkspaceProbeRuntime {
        cwd,
        runtime_data_root,
        env_overrides: runtime_plan.env_overrides,
    })
}
