use std::path::{Path as FsPath, PathBuf};

use ctx_core::ids::WorkspaceId;
use ctx_core::models::{Workspace, Worktree};
use ctx_settings_model::{ContainerRuntimeKind, ExecutionMode};
use ctx_transport_runtime::terminals::{
    NativeContainerTerminalSpec, SharedVmContainerTerminalSpec,
};

use super::{internal_error, TerminalLaunchError, TerminalLaunchHost};

#[path = "container/runtime.rs"]
mod runtime;

pub(super) async fn prepare_terminal_container_launch(
    host: &TerminalLaunchHost,
    workspace: &Workspace,
    worktree: Option<&Worktree>,
    effective: &ctx_settings_model::ExecutionSettings,
    workspace_id: WorkspaceId,
    cwd: &FsPath,
    container_cwd_authority_root: Option<&FsPath>,
) -> Result<
    (
        PathBuf,
        Option<NativeContainerTerminalSpec>,
        Option<SharedVmContainerTerminalSpec>,
    ),
    TerminalLaunchError,
> {
    if !matches!(effective.mode, ExecutionMode::Sandbox) {
        return Ok((cwd.to_path_buf(), None, None));
    }
    let container_cwd_authority_root = container_cwd_authority_root.ok_or_else(|| {
        internal_error("sandbox terminal requires a resolved container cwd authority root")
    })?;

    match effective.container.runtime {
        ContainerRuntimeKind::NativeContainer => {
            let (canonical_cwd, spec) = runtime::prepare_native_container_terminal_launch(
                host,
                workspace,
                worktree,
                effective,
                workspace_id,
                cwd,
                container_cwd_authority_root,
            )
            .await?;
            Ok((canonical_cwd, Some(spec), None))
        }
        ContainerRuntimeKind::SharedVmContainer => {
            let (canonical_cwd, spec) = runtime::prepare_shared_vm_container_terminal_launch(
                host,
                workspace,
                worktree,
                effective,
                workspace_id,
                cwd,
                container_cwd_authority_root,
            )
            .await?;
            Ok((canonical_cwd, None, Some(spec)))
        }
    }
}
