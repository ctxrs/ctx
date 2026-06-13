use super::*;

mod archive_and_merge_queue;
mod base;
mod providers_and_config;
mod tasks;
mod terminals_and_worktrees;

pub(super) fn workspace_routes() -> axum::Router<RouteState> {
    base::workspace_base_routes()
        .merge(providers_and_config::workspace_provider_and_config_routes())
        .merge(tasks::workspace_task_routes())
        .merge(terminals_and_worktrees::workspace_terminal_and_worktree_routes())
        .merge(archive_and_merge_queue::workspace_archive_and_merge_queue_routes())
}
