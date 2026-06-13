use ctx_subagent_service::SubagentWorktreeSelection;

use super::SubagentChildInit;
use crate::daemon::sessions::subagents::errors::{api_error, ApiResult, SubagentErrorKind};

pub(super) async fn resolve_child_worktree(
    init: &SubagentChildInit,
    store: &ctx_store::Store,
) -> ApiResult<(ctx_core::ids::WorktreeId, Option<String>)> {
    match init.worktree_selection {
        SubagentWorktreeSelection::Inherit => Ok((init.parent.worktree_id, None)),
        SubagentWorktreeSelection::New => {
            let (vcs_kind, base_commit_sha) = init
                .worktree_plan
                .clone()
                .ok_or_else(|| api_error(SubagentErrorKind::Internal, "worktree plan missing"))?;
            let worktree = init
                .host
                .create_subagent_worktree(
                    store,
                    &init.workspace,
                    init.parent.task_id,
                    &base_commit_sha,
                    vcs_kind,
                    &init.parent_effective,
                )
                .await?;
            Ok((worktree.id, Some(worktree.root_path)))
        }
    }
}
