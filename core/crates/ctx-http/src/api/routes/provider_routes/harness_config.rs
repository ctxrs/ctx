use axum::routing::{delete, get, post, put};

use crate::api::providers::{
    delete_provider_harness_endpoint, get_provider_harness_config,
    refresh_provider_harness_endpoint_models, select_provider_harness_source,
    set_provider_harness_endpoint_manual_models, upsert_provider_harness_endpoint,
};
use crate::api::router::RouteState;

pub(super) fn provider_harness_config_routes() -> axum::Router<RouteState> {
    axum::Router::new()
        .route(
            "/api/providers/:id/harness_config",
            get(get_provider_harness_config),
        )
        .route(
            "/api/providers/:id/harness_config/select",
            post(select_provider_harness_source),
        )
        .route(
            "/api/providers/:id/harness_config/endpoints",
            post(upsert_provider_harness_endpoint),
        )
        .route(
            "/api/providers/:id/harness_config/endpoints/:endpoint_id",
            delete(delete_provider_harness_endpoint),
        )
        .route(
            "/api/providers/:id/harness_config/endpoints/:endpoint_id/models/refresh",
            post(refresh_provider_harness_endpoint_models),
        )
        .route(
            "/api/providers/:id/harness_config/endpoints/:endpoint_id/models/manual",
            put(set_provider_harness_endpoint_manual_models),
        )
}
