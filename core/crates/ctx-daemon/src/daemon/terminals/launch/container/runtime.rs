use std::path::{Path as FsPath, PathBuf};

use ctx_sandbox_container_runtime::{
    sandbox_cli_invocation, sandbox_container_command, SandboxCommandMode,
};
use ctx_transport_runtime::terminal_launch::canonicalize_container_terminal_cwd;
use ctx_transport_runtime::terminals::{
    NativeContainerTerminalSpec, SharedVmContainerTerminalSpec,
};

use ctx_core::ids::WorkspaceId;
use ctx_core::models::{Workspace, Worktree};

use super::super::{internal_error, TerminalLaunchError, TerminalLaunchHost};

pub(super) async fn prepare_native_container_terminal_launch(
    host: &TerminalLaunchHost,
    workspace: &Workspace,
    worktree: Option<&Worktree>,
    effective: &ctx_settings_model::ExecutionSettings,
    workspace_id: WorkspaceId,
    cwd: &FsPath,
    container_cwd_authority_root: &FsPath,
) -> Result<(PathBuf, NativeContainerTerminalSpec), TerminalLaunchError> {
    host.harness()
        .ensure_workspace_container(workspace, effective, host.daemon_url())
        .await
        .map_err(|e| internal_error(format!("failed to ensure harness container: {e}")))?;
    if worktree.is_none() {
        ensure_materialized_workspace_root(host, workspace).await?;
    }
    let container_name = ctx_workspace_container::workspace_container_name(workspace_id);
    let cwd_validation =
        sandbox_container_command(host.data_root(), &SandboxCommandMode::NativeContainer)
            .map_err(|e| internal_error(format!("sandbox container CLI unavailable: {e}")))?;
    let canonical_cwd = canonicalize_container_terminal_cwd(
        cwd_validation,
        &container_name,
        cwd,
        container_cwd_authority_root,
    )
    .await?;
    let inv = sandbox_cli_invocation(host.data_root())
        .map_err(|e| internal_error(format!("sandbox container CLI unavailable: {e}")))?;
    Ok((
        canonical_cwd.clone(),
        NativeContainerTerminalSpec {
            cli_bin: inv.bin,
            cli_env: inv.env,
            container_name,
            workdir: canonical_cwd.to_string_lossy().to_string(),
            user: Some(ctx_workspace_container::CONTAINER_TERMINAL_USER.to_string()),
        },
    ))
}

pub(super) async fn prepare_shared_vm_container_terminal_launch(
    host: &TerminalLaunchHost,
    workspace: &Workspace,
    worktree: Option<&Worktree>,
    effective: &ctx_settings_model::ExecutionSettings,
    workspace_id: WorkspaceId,
    cwd: &FsPath,
    container_cwd_authority_root: &FsPath,
) -> Result<(PathBuf, SharedVmContainerTerminalSpec), TerminalLaunchError> {
    if let Some(worktree) = worktree {
        host.harness()
            .ensure_workspace_container_for_worktree(
                workspace,
                worktree,
                effective,
                host.daemon_url(),
            )
            .await
            .map_err(|e| internal_error(format!("failed to ensure sandbox container: {e}")))?;
    } else {
        host.harness()
            .ensure_workspace_container(workspace, effective, host.daemon_url())
            .await
            .map_err(|e| internal_error(format!("failed to ensure sandbox container: {e}")))?;
    }
    if worktree.is_none() {
        ensure_materialized_workspace_root(host, workspace).await?;
    }
    let helper_path = ctx_avf_linux_runtime::helper_path()
        .map_err(|e| internal_error(format!("AVF helper unavailable: {e}")))?;
    let command_mode = SandboxCommandMode::SharedVm {
        helper_path: helper_path.clone(),
    };
    let cwd_validation = sandbox_container_command(host.data_root(), &command_mode)
        .map_err(|e| internal_error(format!("sandbox container CLI unavailable: {e}")))?;
    let container_name = ctx_workspace_container::workspace_container_name(workspace_id);
    let canonical_cwd = canonicalize_container_terminal_cwd(
        cwd_validation,
        &container_name,
        cwd,
        container_cwd_authority_root,
    )
    .await?;
    Ok((
        canonical_cwd.clone(),
        SharedVmContainerTerminalSpec {
            helper_path,
            data_root: host.data_root().to_path_buf(),
            workspace_id,
            workdir: canonical_cwd.to_string_lossy().to_string(),
            user: Some(ctx_workspace_container::CONTAINER_TERMINAL_USER.to_string()),
        },
    ))
}

async fn ensure_materialized_workspace_root(
    host: &TerminalLaunchHost,
    workspace: &Workspace,
) -> Result<(), TerminalLaunchError> {
    let sandbox_mode = ctx_harness_runtime::selected_sandbox_command_mode(host.data_root())
        .map_err(|err| internal_error(err.to_string()))?;
    ctx_sandbox_materialization::ensure_workspace_root_from_host_copy(
        host.data_root(),
        &sandbox_mode,
        workspace,
    )
    .await
    .map_err(|e| internal_error(format!("failed to materialize sandbox workspace root: {e}")))?;
    Ok(())
}
