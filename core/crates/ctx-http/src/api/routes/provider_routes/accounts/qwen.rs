use axum::routing::{delete, get, post, put};

use crate::api::providers::{
    delete_qwen_account, get_qwen_login, list_qwen_accounts, set_qwen_active_account,
    start_qwen_login, upsert_qwen_account,
};
use crate::api::router::RouteState;

pub(super) fn qwen_account_routes() -> axum::Router<RouteState> {
    axum::Router::new()
        .route(
            "/api/providers/qwen/accounts",
            get(list_qwen_accounts).post(upsert_qwen_account),
        )
        .route(
            "/api/providers/qwen/accounts/login/start",
            post(start_qwen_login),
        )
        .route(
            "/api/providers/qwen/accounts/login/:id",
            get(get_qwen_login),
        )
        .route(
            "/api/providers/qwen/active-account",
            put(set_qwen_active_account),
        )
        .route(
            "/api/providers/qwen/accounts/:id",
            delete(delete_qwen_account),
        )
}
