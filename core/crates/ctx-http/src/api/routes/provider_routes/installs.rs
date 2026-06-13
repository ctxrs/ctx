use axum::routing::{get, post};

use crate::api::provider_launch;
use crate::api::router::RouteState;

pub(super) fn provider_install_routes() -> axum::Router<RouteState> {
    axum::Router::new()
        .route(
            "/api/providers/install_all",
            post(provider_launch::install_all_providers),
        )
        .route(
            "/api/providers/:id/install",
            post(provider_launch::install_provider),
        )
        .route(
            "/api/providers/install/:install_id",
            get(provider_launch::get_install),
        )
        .route(
            "/api/providers/install/statuses",
            post(provider_launch::get_install_statuses),
        )
        .route(
            "/api/providers/install/:install_id/cancel",
            post(provider_launch::cancel_install),
        )
        .route(
            "/api/providers/install/:install_id/events",
            get(provider_launch::list_install_events),
        )
        .route(
            "/api/providers/install/:install_id/stream",
            get(provider_launch::install_stream_sse),
        )
}
