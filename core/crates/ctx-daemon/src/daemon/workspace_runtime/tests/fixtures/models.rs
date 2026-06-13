use super::*;

pub(in crate::daemon::workspace_runtime::tests) fn sample_workspace(tmp: &TempDir) -> Workspace {
    Workspace {
        id: WorkspaceId::new(),
        name: "ws".to_string(),
        root_path: tmp.path().to_string_lossy().to_string(),
        created_at: Utc::now(),
        vcs_kind: None,
    }
}

pub(in crate::daemon::workspace_runtime::tests) fn sample_worktree(
    tmp: &TempDir,
    workspace_id: WorkspaceId,
) -> Worktree {
    Worktree {
        id: WorktreeId::new(),
        workspace_id,
        root_path: tmp.path().to_string_lossy().to_string(),
        base_commit_sha: "deadbeef".to_string(),
        git_branch: Some("main".to_string()),
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

pub(in crate::daemon::workspace_runtime::tests) async fn runtime_manager(
    tmp: &TempDir,
) -> HarnessRuntimeManager {
    HarnessRuntimeManager::new(tmp.path().to_path_buf())
}

#[cfg(target_os = "macos")]
pub(in crate::daemon::workspace_runtime::tests) async fn create_session_with_environment(
    stores: &StoreManager,
    root: &std::path::Path,
    execution_environment: ExecutionEnvironment,
) -> SessionId {
    let workspace = stores
        .global()
        .create_workspace(
            "ws".to_string(),
            root.to_string_lossy().to_string(),
            ctx_core::models::VcsKind::Git,
        )
        .await
        .expect("create workspace");
    let store = stores
        .workspace(workspace.id)
        .await
        .expect("workspace store");
    let worktree = store
        .create_worktree(
            workspace.id,
            root.to_string_lossy().to_string(),
            "base".to_string(),
            None,
        )
        .await
        .expect("create worktree");
    let task = store
        .create_task(workspace.id, "task".to_string(), None)
        .await
        .expect("create task");
    store
        .create_session(
            task.id,
            workspace.id,
            worktree.id,
            execution_environment,
            "fake".to_string(),
            "fake-model".to_string(),
            "assistant".to_string(),
            None,
            None,
            None,
        )
        .await
        .expect("create session")
        .id
}
