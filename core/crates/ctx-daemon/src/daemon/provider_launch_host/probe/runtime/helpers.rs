use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::Utc;
use ctx_core::ids::WorktreeId;
use ctx_core::models::{Workspace, Worktree};
use ctx_settings_model::{ContainerMountMode, ExecutionMode};
use ctx_worktree_data_plane::WorktreeDataPlane;

pub(super) fn synthetic_probe_worktree(workspace: &Workspace) -> Worktree {
    Worktree {
        id: WorktreeId(uuid::Uuid::nil()),
        workspace_id: workspace.id,
        root_path: workspace.root_path.clone(),
        base_commit_sha: String::new(),
        git_branch: None,
        vcs_kind: workspace.vcs_kind.clone(),
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

pub(super) fn probe_cwd_for_workspace_runtime(
    data_plane: &WorktreeDataPlane,
    worktree: &Worktree,
    mode: ExecutionMode,
    mount_mode: ContainerMountMode,
) -> PathBuf {
    if matches!(mode, ExecutionMode::Sandbox)
        && matches!(mount_mode, ContainerMountMode::DiskIsolated)
    {
        return data_plane.live_worktree_root.clone();
    }
    PathBuf::from(&worktree.root_path)
}

pub(super) fn runtime_data_root(env_overrides: &HashMap<String, String>) -> Option<PathBuf> {
    env_overrides
        .get("CTX_DATA_ROOT")
        .map(|value| Path::new(value).to_path_buf())
}
