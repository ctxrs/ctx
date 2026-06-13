use axum::routing::{delete, get, post, put};

use crate::api::providers::{
    complete_codex_login, delete_codex_account, get_codex_accounts_usage, get_codex_login,
    import_host_codex_auth, list_codex_accounts, probe_host_codex_import, set_codex_active_account,
    start_codex_login,
};
use crate::api::router::RouteState;

pub(super) fn codex_account_routes() -> axum::Router<RouteState> {
    axum::Router::new()
        .route("/api/providers/codex/accounts", get(list_codex_accounts))
        .route(
            "/api/providers/codex/import/host",
            get(probe_host_codex_import).post(import_host_codex_auth),
        )
        .route(
            "/api/providers/codex/accounts/usage",
            get(get_codex_accounts_usage),
        )
        .route(
            "/api/providers/codex/accounts/login/start",
            post(start_codex_login),
        )
        .route(
            "/api/providers/codex/accounts/login/:id",
            get(get_codex_login).post(complete_codex_login),
        )
        .route(
            "/api/providers/codex/active-account",
            put(set_codex_active_account),
        )
        .route(
            "/api/providers/codex/accounts/:id",
            delete(delete_codex_account),
        )
}
