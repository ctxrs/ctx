use std::path::PathBuf;

use ctx_core::models::{Workspace, Worktree};
use ctx_transport_runtime::terminal_launch::{
    default_terminal_shell, resolve_container_terminal_cwd, resolve_host_terminal_cwd,
    resolve_terminal_host_root,
};
use ctx_worktree_data_plane::WorktreeDataPlane;

use super::{internal_error, TerminalLaunchError};

pub(super) struct ResolvedTerminalPaths {
    pub(super) cwd: PathBuf,
    pub(super) shell: String,
    pub(super) container_cwd_authority_root: Option<PathBuf>,
}

pub(super) async fn resolve_terminal_paths(
    workspace: &Workspace,
    worktree: Option<&Worktree>,
    worktree_data_plane: Option<&WorktreeDataPlane>,
    container_mode: bool,
    requested_cwd: Option<&str>,
    requested_shell: Option<&str>,
) -> Result<ResolvedTerminalPaths, TerminalLaunchError> {
    let workspace_root_path = PathBuf::from(&workspace.root_path);
    let workspace_root: PathBuf = resolve_terminal_host_root(
        &workspace_root_path,
        container_mode,
        "workspace root is unavailable",
    )
    .await?;

    let worktree_root: Option<PathBuf> = if let Some(worktree) = worktree {
        let root = PathBuf::from(&worktree.root_path);
        Some(
            resolve_terminal_host_root(&root, container_mode, "worktree root is unavailable")
                .await?,
        )
    } else {
        None
    };

    let requested_cwd = requested_cwd.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(PathBuf::from(trimmed))
        }
    });
    let container_cwd_authority_root = if container_mode {
        let data_plane = worktree_data_plane.ok_or_else(|| {
            internal_error("sandbox terminal requires a resolved worktree data plane")
        })?;
        Some(if worktree_root.is_some() {
            data_plane.live_worktree_root.clone()
        } else {
            data_plane.live_workspace_root.clone()
        })
    } else {
        None
    };
    let cwd = if container_mode {
        let data_plane = worktree_data_plane.ok_or_else(|| {
            internal_error("sandbox terminal requires a resolved worktree data plane")
        })?;
        resolve_container_terminal_cwd(
            &data_plane.live_workspace_root,
            worktree_root
                .as_ref()
                .map(|_| data_plane.live_worktree_root.as_path()),
            &workspace_root,
            worktree_root.as_deref(),
            requested_cwd.as_deref(),
        )?
    } else {
        let bound_root = worktree_root.as_deref().unwrap_or(&workspace_root);
        resolve_host_terminal_cwd(bound_root, requested_cwd.as_deref()).await?
    };

    let requested_shell = requested_shell.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    });
    let shell = if container_mode {
        requested_shell
            .map(ToString::to_string)
            .unwrap_or_else(|| "/bin/bash".to_string())
    } else {
        requested_shell
            .map(ToString::to_string)
            .unwrap_or_else(default_terminal_shell)
    };

    Ok(ResolvedTerminalPaths {
        cwd,
        shell,
        container_cwd_authority_root,
    })
}
