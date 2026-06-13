use axum::routing::{delete, get, post, put};

use crate::api::providers::{
    delete_kimi_account, get_kimi_login, list_kimi_accounts, set_kimi_active_account,
    start_kimi_login, upsert_kimi_account,
};
use crate::api::router::RouteState;

pub(super) fn kimi_account_routes() -> axum::Router<RouteState> {
    axum::Router::new()
        .route(
            "/api/providers/kimi/accounts/login/start",
            post(start_kimi_login),
        )
        .route(
            "/api/providers/kimi/accounts/login/:id",
            get(get_kimi_login),
        )
        .route(
            "/api/providers/kimi/accounts",
            get(list_kimi_accounts).post(upsert_kimi_account),
        )
        .route(
            "/api/providers/kimi/active-account",
            put(set_kimi_active_account),
        )
        .route(
            "/api/providers/kimi/accounts/:id",
            delete(delete_kimi_account),
        )
}
