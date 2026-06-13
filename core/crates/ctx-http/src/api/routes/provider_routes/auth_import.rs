use axum::routing::{get, post};

use crate::api::providers::{
    import_provider_auth_candidates, list_provider_auth_import_candidates,
    list_provider_auth_import_profiles,
};
use crate::api::router::RouteState;

pub(super) fn provider_auth_import_routes() -> axum::Router<RouteState> {
    axum::Router::new()
        .route(
            "/api/providers/auth/import/candidates",
            get(list_provider_auth_import_candidates),
        )
        .route(
            "/api/providers/auth/import/profiles",
            get(list_provider_auth_import_profiles),
        )
        .route(
            "/api/providers/auth/import",
            post(import_provider_auth_candidates),
        )
}
