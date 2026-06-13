use axum::routing::{delete, get, post, put};

use crate::api::providers::{
    delete_mistral_account, get_mistral_login, list_mistral_accounts, set_mistral_active_account,
    start_mistral_login, upsert_mistral_account,
};
use crate::api::router::RouteState;

pub(super) fn mistral_account_routes() -> axum::Router<RouteState> {
    axum::Router::new()
        .route(
            "/api/providers/mistral/accounts",
            get(list_mistral_accounts).post(upsert_mistral_account),
        )
        .route(
            "/api/providers/mistral/accounts/login/start",
            post(start_mistral_login),
        )
        .route(
            "/api/providers/mistral/accounts/login/:id",
            get(get_mistral_login),
        )
        .route(
            "/api/providers/mistral/active-account",
            put(set_mistral_active_account),
        )
        .route(
            "/api/providers/mistral/accounts/:id",
            delete(delete_mistral_account),
        )
}
