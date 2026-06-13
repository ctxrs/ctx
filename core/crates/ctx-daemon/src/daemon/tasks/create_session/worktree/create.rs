use super::*;

pub(super) async fn create_session_execution_worktree(
    handles: &TaskSessionHandles,
    store: &Store,
    task: &Task,
    workspace: &Workspace,
    workspace_effective: &ExecutionSettings,
) -> Result<WorktreeId, TaskSessionCreateError> {
    let workspace_root = StdPath::new(&workspace.root_path);
    let base = ctx_worktree_vcs_service::resolve_worktree_creation_base(workspace_root)
        .await
        .map_err(|error| {
            if error.is_client_error() {
                TaskSessionCreateError::BadRequest
            } else {
                TaskSessionCreateError::Internal(anyhow::anyhow!("{error:?}"))
            }
        })?;
    let worktree_id = WorktreeId::new();
    let branch_name = format!("ctx/{}/{}", task.id.0, worktree_id.0);
    let (wt_path, sandbox_binding) = handles
        .admission
        .provision_worktree_for_execution(
            workspace,
            worktree_id,
            &base.base_commit_sha,
            &branch_name,
            workspace_effective,
        )
        .await
        .map_err(|e| {
            tracing::warn!(
                task_id = %task.id.0,
                worktree_id = %worktree_id.0,
                "worktree provisioning failed: {e:#}"
            );
            TaskSessionCreateError::Internal(e)
        })?;

    let worktree = Worktree {
        id: worktree_id,
        workspace_id: task.workspace_id,
        root_path: wt_path.to_string_lossy().to_string(),
        base_commit_sha: base.base_commit_sha.clone(),
        git_branch: (base.vcs_kind == VcsKind::Git).then(|| branch_name.clone()),
        vcs_kind: Some(base.vcs_kind),
        base_revision: Some(base.base_commit_sha.clone()),
        vcs_ref: Some(branch_name.clone()),
        created_at: chrono::Utc::now(),
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
    };
    handles
        .admission
        .persist_provisioned_worktree(store, workspace, worktree, sandbox_binding)
        .await
        .map_err(TaskSessionCreateError::Internal)?;
    Ok(worktree_id)
}
