use axum::routing::{get, post};

use crate::api::providers::{get_provider, get_provider_usage, list_providers};
use crate::api::refresh_provider_matrix;
use crate::api::router::RouteState;

pub(super) fn provider_base_routes() -> axum::Router<RouteState> {
    axum::Router::new()
        .route("/api/providers", get(list_providers))
        .route(
            "/api/providers/matrix/refresh",
            post(refresh_provider_matrix),
        )
        .route("/api/providers/:id", get(get_provider))
        .route("/api/providers/:id/usage", get(get_provider_usage))
}
