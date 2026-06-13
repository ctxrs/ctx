use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use ctx_core::models::{Workspace, Worktree};
use ctx_harness_runtime::sandbox_container_command;
use ctx_workspace_container::workspace_container_name;
use ctx_worktree_vcs_service::{SandboxContainerRuntime, WorktreeHookExecution};
use tokio::process::Command;

pub(super) fn sandbox_command(
    data_root: &Path,
    workspace: &Workspace,
    worktree: &Worktree,
    execution: &WorktreeHookExecution,
    command: &str,
    args: &[String],
) -> Result<Command> {
    let live_worktree_root = execution
        .live_worktree_root
        .as_ref()
        .context("sandbox hook execution missing live worktree root")?;
    let runtime = execution
        .container_runtime
        .context("sandbox hook execution missing runtime kind")?;
    match runtime {
        SandboxContainerRuntime::NativeContainer => {
            let mut cmd = sandbox_container_command(data_root)?;
            cmd.arg("exec")
                .arg("--interactive")
                .arg("--workdir")
                .arg(live_worktree_root)
                .arg(workspace_container_name(workspace.id))
                .arg(command);
            cmd.args(args);
            Ok(cmd)
        }
        SandboxContainerRuntime::SharedVmContainer => {
            ctx_avf_linux_runtime::build_guest_exec_command(
                data_root,
                workspace.id,
                worktree.id,
                Path::new(live_worktree_root),
                command,
                args,
                &HashMap::new(),
                None,
                false,
            )
        }
    }
}
