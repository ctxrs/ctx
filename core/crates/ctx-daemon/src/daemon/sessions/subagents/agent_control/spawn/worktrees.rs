use ctx_core::ids::{TaskId, WorktreeId};
use ctx_core::models::{VcsKind, Workspace, Worktree};
use ctx_settings_model::ExecutionSettings;
use ctx_store::Store;
use ctx_subagent_service::SubagentWorktreeSelection;

use super::SubagentSpawnHost;
use crate::daemon::sessions::subagents::errors::ApiResult;
use crate::daemon::sessions::subagents::{api_error, internal_api_error, SubagentErrorKind};

impl SubagentSpawnHost {
    pub(in crate::daemon) async fn resolve_existing_worktree_execution(
        &self,
        store: &Store,
        workspace: &Workspace,
        worktree_id: WorktreeId,
    ) -> ApiResult<crate::daemon::workspaces::ResolvedExistingWorktreeExecution> {
        self.worktrees
            .resolve_existing_worktree_execution(store, workspace, worktree_id)
            .await
            .map_err(internal_api_error)
    }

    pub(in crate::daemon) async fn plan_subagent_worktree_creation(
        &self,
        parent_worktree: &Worktree,
        selection: SubagentWorktreeSelection,
    ) -> ApiResult<Option<(VcsKind, String)>> {
        if selection != SubagentWorktreeSelection::New {
            return Ok(None);
        }

        let base_commit_sha =
            self.session_vcs
                .resolve_worktree_commit(parent_worktree, "HEAD")
                .await
                .map_err(|error| {
                    let msg = error.to_string().to_lowercase();
                    if msg.contains("ambiguous argument 'head'")
                        || msg.contains("unknown revision or path not in the working tree")
                    {
                        return api_error(
                            SubagentErrorKind::BadRequest,
                            "git repo has no commits; create an initial commit before creating a worktree",
                        );
                    }
                    internal_api_error(error)
                })?;
        let vcs_kind =
            ctx_worktree_vcs_service::effective_worktree_vcs_kind(parent_worktree.vcs_kind.clone());
        let dirty_counts = self
            .session_vcs
            .diff_worktree_summary_for_session(parent_worktree, &base_commit_sha)
            .await
            .map_err(internal_api_error)?;
        if dirty_counts.file_count > 0
            || dirty_counts.line_additions > 0
            || dirty_counts.line_deletions > 0
        {
            return Err(api_error(
                SubagentErrorKind::BadRequest,
                "Your worktree has uncommitted changes. Before starting new subagents in new worktree mode, you must commit or stash your changes to be explicit about whether subagents should inherit these diffs.",
            ));
        }

        Ok(Some((vcs_kind, base_commit_sha)))
    }

    pub(in crate::daemon) async fn create_subagent_worktree(
        &self,
        store: &Store,
        workspace: &Workspace,
        task_id: TaskId,
        base_commit_sha: &str,
        vcs_kind: VcsKind,
        effective: &ExecutionSettings,
    ) -> ApiResult<Worktree> {
        let worktree_id = WorktreeId::new();
        let branch_name = format!("ctx/{}/{}", task_id.0, worktree_id.0);
        let (wt_path, sandbox_binding) = self
            .worktrees
            .provision_worktree_for_execution(
                workspace,
                worktree_id,
                base_commit_sha,
                &branch_name,
                effective,
            )
            .await
            .map_err(
                crate::daemon::sessions::subagents::errors::internal_request_or_policy_error,
            )?;

        let worktree = Worktree {
            id: worktree_id,
            workspace_id: workspace.id,
            root_path: wt_path.to_string_lossy().to_string(),
            base_commit_sha: base_commit_sha.to_string(),
            git_branch: (vcs_kind == VcsKind::Git).then(|| branch_name.clone()),
            vcs_kind: Some(vcs_kind),
            base_revision: Some(base_commit_sha.to_string()),
            vcs_ref: Some(branch_name),
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

        let worktree = self
            .worktrees
            .persist_provisioned_worktree(store, workspace, worktree, sandbox_binding)
            .await
            .map_err(internal_api_error)?;
        if let Err(error) = self
            .worktrees
            .ensure_task_commit_hook(workspace, &worktree, task_id)
            .await
        {
            tracing::warn!(
                task_id = %task_id.0,
                worktree_id = %worktree.id.0,
                "failed to configure vcs hooks for subagent worktree: {error:#}"
            );
        }
        Ok(worktree)
    }
}
