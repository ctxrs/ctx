use axum::routing::{delete, get, post, put};

use crate::api::providers::{
    delete_gemini_account, get_gemini_login, list_gemini_accounts, set_gemini_active_account,
    start_gemini_login, upsert_gemini_account,
};
use crate::api::router::RouteState;

pub(super) fn gemini_account_routes() -> axum::Router<RouteState> {
    axum::Router::new()
        .route(
            "/api/providers/gemini/accounts",
            get(list_gemini_accounts).post(upsert_gemini_account),
        )
        .route(
            "/api/providers/gemini/accounts/login/start",
            post(start_gemini_login),
        )
        .route(
            "/api/providers/gemini/accounts/login/:id",
            get(get_gemini_login),
        )
        .route(
            "/api/providers/gemini/active-account",
            put(set_gemini_active_account),
        )
        .route(
            "/api/providers/gemini/accounts/:id",
            delete(delete_gemini_account),
        )
}
