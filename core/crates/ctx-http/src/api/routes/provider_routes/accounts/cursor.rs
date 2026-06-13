use axum::routing::{delete, get, post, put};

use crate::api::providers::{
    delete_cursor_account, get_cursor_login, list_cursor_accounts, set_cursor_active_account,
    start_cursor_login, upsert_cursor_account,
};
use crate::api::router::RouteState;

pub(super) fn cursor_account_routes() -> axum::Router<RouteState> {
    axum::Router::new()
        .route(
            "/api/providers/cursor/accounts",
            get(list_cursor_accounts).post(upsert_cursor_account),
        )
        .route(
            "/api/providers/cursor/accounts/login/start",
            post(start_cursor_login),
        )
        .route(
            "/api/providers/cursor/accounts/login/:id",
            get(get_cursor_login),
        )
        .route(
            "/api/providers/cursor/active-account",
            put(set_cursor_active_account),
        )
        .route(
            "/api/providers/cursor/accounts/:id",
            delete(delete_cursor_account),
        )
}
