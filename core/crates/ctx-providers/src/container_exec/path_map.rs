use anyhow::{Context, Result};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::crp::rewrite_bundled_path_for_linux;

use super::spec::{container_exec_spec, ContainerExecSpec};

pub(crate) fn translate_thread_cwd_for_container(
    env: &HashMap<String, String>,
    workdir: &Path,
) -> Result<PathBuf> {
    let Some(spec) = container_exec_spec(env) else {
        return Ok(workdir.to_path_buf());
    };
    match spec {
        ContainerExecSpec::NativeContainer {
            host_worktree_root: Some(host_worktree_root),
            guest_worktree_root: Some(guest_worktree_root),
            guest_workspace_root: Some(guest_workspace_root),
            ..
        } => resolve_linux_sandbox_cwd(
            workdir,
            &host_worktree_root,
            &guest_worktree_root,
            &guest_workspace_root,
        ),
        ContainerExecSpec::SharedVmContainer {
            host_worktree_root,
            guest_worktree_root,
            guest_workspace_root,
            ..
        } => resolve_linux_sandbox_cwd(
            workdir,
            &host_worktree_root,
            &guest_worktree_root,
            &guest_workspace_root,
        ),
        _ => Ok(workdir.to_path_buf()),
    }
}

pub(super) fn resolve_linux_sandbox_cwd(
    workdir: &Path,
    host_worktree_root: &Path,
    guest_worktree_root: &Path,
    guest_workspace_root: &Path,
) -> Result<PathBuf> {
    if workdir.starts_with(guest_workspace_root) {
        return Ok(workdir.to_path_buf());
    }
    if workdir == host_worktree_root {
        return Ok(guest_worktree_root.to_path_buf());
    }
    if workdir.starts_with(host_worktree_root) {
        let relative = workdir
            .strip_prefix(host_worktree_root)
            .context("mapping host worktree cwd for linux sandbox execution")?;
        return Ok(join_guest_relative(guest_worktree_root, relative));
    }
    if workdir.starts_with(guest_worktree_root) {
        return Ok(workdir.to_path_buf());
    }
    anyhow::bail!(
        "linux sandbox cwd mapping failed: workdir {} is outside host root {} and guest root {}",
        workdir.display(),
        host_worktree_root.display(),
        guest_worktree_root.display()
    );
}

pub(super) fn rewrite_container_env_value_for_linux(key: &str, value: &str) -> Result<String> {
    if key == "PATH" {
        return rewrite_container_path_list_for_linux(value);
    }
    if key == "CTX_MCP_COMMAND" {
        return rewrite_bundled_path_for_linux(value);
    }
    if key.ends_with("_PATH") {
        return rewrite_bundled_path_for_linux(value);
    }
    Ok(value.to_string())
}

pub(crate) fn rewrite_ctx_mcp_command_for_spec(
    spec: Option<&ContainerExecSpec>,
    value: &str,
) -> Result<String> {
    let rewritten = rewrite_bundled_path_for_linux(value)?;
    let Some(ContainerExecSpec::SharedVmContainer { .. }) = spec else {
        return Ok(rewritten);
    };
    let path = Path::new(&rewritten);
    if !path.is_absolute() {
        return Ok(rewritten);
    }
    // This command is consumed by the CRP process inside the harness container,
    // not by a command run directly in the AVF guest. The container bind mounts
    // the daemon data root at its original host path, so mapping to
    // /mnt/ctx-host would make the command invisible to the child process.
    Ok(rewritten)
}

pub(crate) fn rewrite_ctx_mcp_command_for_env(
    env: &HashMap<String, String>,
    value: &str,
) -> Result<String> {
    let spec = container_exec_spec(env);
    rewrite_ctx_mcp_command_for_spec(spec.as_ref(), value)
}

fn join_guest_relative(root: &Path, relative: &Path) -> PathBuf {
    let mut out = root.to_path_buf();
    if relative != Path::new("") {
        out.push(relative);
    }
    out
}

fn rewrite_container_path_list_for_linux(value: &str) -> Result<String> {
    if value.trim().is_empty() {
        return Ok(value.to_string());
    }
    let rewritten_paths = std::env::split_paths(OsStr::new(value))
        .map(|entry| {
            rewrite_bundled_path_for_linux(entry.to_string_lossy().as_ref()).map(PathBuf::from)
        })
        .collect::<Result<Vec<_>>>()?;
    let joined = std::env::join_paths(rewritten_paths)
        .context("joining rewritten PATH entries for linux container execution")?;
    Ok(joined.to_string_lossy().to_string())
}
