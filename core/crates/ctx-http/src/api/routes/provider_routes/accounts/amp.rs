use axum::routing::{delete, get, post, put};

use crate::api::providers::{
    delete_amp_account, get_amp_login, list_amp_accounts, set_amp_active_account, start_amp_login,
    upsert_amp_account,
};
use crate::api::router::RouteState;

pub(super) fn amp_account_routes() -> axum::Router<RouteState> {
    axum::Router::new()
        .route(
            "/api/providers/amp/accounts/login/start",
            post(start_amp_login),
        )
        .route("/api/providers/amp/accounts/login/:id", get(get_amp_login))
        .route(
            "/api/providers/amp/accounts",
            get(list_amp_accounts).post(upsert_amp_account),
        )
        .route(
            "/api/providers/amp/active-account",
            put(set_amp_active_account),
        )
        .route(
            "/api/providers/amp/accounts/:id",
            delete(delete_amp_account),
        )
}
