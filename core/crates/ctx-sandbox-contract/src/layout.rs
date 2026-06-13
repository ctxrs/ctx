use std::path::{Path, PathBuf};

use ctx_core::ids::WorktreeId;
use ctx_core::models::{Workspace, Worktree};

use crate::ExecutionMode;

pub const CTX_CONTAINER_WORKSPACE_ROOT: &str = "/ctx/ws";
pub const SHARED_VM_GUEST_HOST_DATA_ROOT: &str = "/mnt/ctx-host";

pub fn container_worktree_root(worktree_id: WorktreeId) -> PathBuf {
    PathBuf::from("/ctx/ws/worktrees").join(worktree_id.0.to_string())
}

pub fn shared_vm_guest_host_share_root() -> PathBuf {
    PathBuf::from(SHARED_VM_GUEST_HOST_DATA_ROOT)
}

pub fn shared_vm_guest_host_share_path(data_root: &Path, host_path: &Path) -> Option<PathBuf> {
    let relative = host_path.strip_prefix(data_root).ok()?;
    Some(shared_vm_guest_host_share_root().join(relative))
}

pub fn map_host_or_live_path_to_live_roots(
    live_workspace_root: &Path,
    live_worktree_root: &Path,
    host_workspace_root: &Path,
    host_worktree_root: Option<&Path>,
    requested: &Path,
) -> Option<PathBuf> {
    if requested.starts_with(live_workspace_root) || requested.starts_with(live_worktree_root) {
        return Some(requested.to_path_buf());
    }

    if let Some(host_worktree_root) = host_worktree_root {
        if requested.starts_with(host_worktree_root) {
            let relative = requested.strip_prefix(host_worktree_root).ok()?;
            return Some(live_worktree_root.join(relative));
        }
    }

    if requested.starts_with(host_workspace_root) {
        let relative = requested.strip_prefix(host_workspace_root).ok()?;
        return Some(live_workspace_root.join(relative));
    }

    None
}

pub fn sandbox_workspace_root() -> PathBuf {
    PathBuf::from(CTX_CONTAINER_WORKSPACE_ROOT)
}

pub fn sandbox_worktree_root(workspace: &Workspace, worktree: &Worktree) -> PathBuf {
    if worktree.root_path == workspace.root_path {
        return sandbox_workspace_root();
    }
    container_worktree_root(worktree.id)
}

pub fn live_workspace_root_for_mode(workspace: &Workspace, mode: ExecutionMode) -> PathBuf {
    match mode {
        ExecutionMode::Host => PathBuf::from(&workspace.root_path),
        ExecutionMode::Sandbox => sandbox_workspace_root(),
    }
}

pub fn live_worktree_root_for_mode(
    workspace: &Workspace,
    worktree: &Worktree,
    mode: ExecutionMode,
) -> PathBuf {
    match mode {
        ExecutionMode::Host => PathBuf::from(&worktree.root_path),
        ExecutionMode::Sandbox => sandbox_worktree_root(workspace, worktree),
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use ctx_core::ids::{WorkspaceId, WorktreeId};
    use ctx_core::models::{Workspace, Worktree};
    use uuid::Uuid;

    use super::*;

    fn sample_workspace(root_path: &str) -> Workspace {
        Workspace {
            id: WorkspaceId(Uuid::new_v4()),
            name: "ws".to_string(),
            root_path: root_path.to_string(),
            created_at: Utc::now(),
            vcs_kind: None,
        }
    }

    fn sample_worktree(workspace_id: WorkspaceId, root_path: &str) -> Worktree {
        Worktree {
            id: WorktreeId(Uuid::new_v4()),
            workspace_id,
            root_path: root_path.to_string(),
            base_commit_sha: String::new(),
            git_branch: None,
            vcs_kind: None,
            base_revision: None,
            vcs_ref: None,
            created_at: Utc::now(),
            bootstrap_status: None,
            bootstrap_started_at: None,
            bootstrap_finished_at: None,
            bootstrap_exit_code: None,
            bootstrap_timeout_sec: None,
            bootstrap_error: None,
            bootstrap_log_path: None,
            bootstrap_log_truncated: None,
            bootstrap_command: None,
            bootstrap_script_path: None,
        }
    }

    #[test]
    fn sandbox_root_for_workspace_root_reuses_workspace_mount() {
        let workspace = sample_workspace("/repo");
        let worktree = sample_worktree(workspace.id, "/repo");
        assert_eq!(
            sandbox_worktree_root(&workspace, &worktree),
            sandbox_workspace_root()
        );
    }

    #[test]
    fn sandbox_root_for_managed_worktree_is_deterministic() {
        let workspace = sample_workspace("/repo");
        let worktree = sample_worktree(workspace.id, "/repo/.ctx/worktree");
        assert_eq!(
            sandbox_worktree_root(&workspace, &worktree),
            container_worktree_root(worktree.id)
        );
    }

    #[test]
    fn shared_vm_guest_host_share_path_projects_under_guest_host_mount_root() {
        let data_root = Path::new("/home/fixture/.ctx");
        let host_path = data_root.join("runtimes/ctx-egress-proxy");
        assert_eq!(
            shared_vm_guest_host_share_path(data_root, &host_path),
            Some(PathBuf::from("/mnt/ctx-host/runtimes/ctx-egress-proxy"))
        );
    }

    #[test]
    fn shared_vm_guest_host_share_path_rejects_paths_outside_data_root() {
        let data_root = Path::new("/home/fixture/.ctx");
        assert_eq!(
            shared_vm_guest_host_share_path(data_root, Path::new("/tmp/outside")),
            None
        );
    }
}
