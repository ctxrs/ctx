use axum::routing::{delete, get, put};

use crate::api::providers::{
    delete_copilot_account, list_copilot_accounts, set_copilot_active_account,
    upsert_copilot_account,
};
use crate::api::router::RouteState;

pub(super) fn copilot_account_routes() -> axum::Router<RouteState> {
    axum::Router::new()
        .route(
            "/api/providers/copilot/accounts",
            get(list_copilot_accounts).post(upsert_copilot_account),
        )
        .route(
            "/api/providers/copilot/active-account",
            put(set_copilot_active_account),
        )
        .route(
            "/api/providers/copilot/accounts/:id",
            delete(delete_copilot_account),
        )
}
