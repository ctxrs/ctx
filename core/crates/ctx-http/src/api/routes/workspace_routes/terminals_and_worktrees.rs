use super::*;

pub(super) fn workspace_terminal_and_worktree_routes() -> axum::Router<RouteState> {
    axum::Router::new()
        .route(
            "/api/workspaces/:id/terminals",
            get(list_workspace_terminals).post(create_workspace_terminal),
        )
        .route("/api/terminals/:id", delete(delete_terminal))
        .route(
            "/api/terminals/:id/stream_token",
            post(mint_terminal_stream_token),
        )
        .route("/api/terminals/:id/stream", get(terminal_stream_ws))
        .route("/api/worktrees/:id", get(get_worktree))
        .route(
            "/api/worktrees/:id/bootstrap/logs",
            get(get_worktree_bootstrap_logs),
        )
}
