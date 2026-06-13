use axum::routing::{delete, get, post, put};

use crate::api::providers::{
    delete_claude_account, get_claude_login, list_claude_accounts, set_claude_active_account,
    start_claude_login, upsert_claude_account,
};
use crate::api::router::RouteState;

pub(super) fn claude_account_routes() -> axum::Router<RouteState> {
    axum::Router::new()
        .route(
            "/api/providers/claude-crp/accounts",
            get(list_claude_accounts).post(upsert_claude_account),
        )
        .route(
            "/api/providers/claude-crp/accounts/login/start",
            post(start_claude_login),
        )
        .route(
            "/api/providers/claude-crp/accounts/login/:id",
            get(get_claude_login),
        )
        .route(
            "/api/providers/claude-crp/active-account",
            put(set_claude_active_account),
        )
        .route(
            "/api/providers/claude-crp/accounts/:id",
            delete(delete_claude_account),
        )
}
