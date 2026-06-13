use super::*;

use ctx_provider_runtime::{
    ProvidersBootstrapResponse, ProvidersBootstrapRouteError, ProvidersBootstrapRouteErrorKind,
    ProvidersBootstrapRouteRequest,
};

pub(crate) async fn get_workspace_providers_bootstrap(
    State(providers): State<ProviderBootstrapHandle>,
    Path(workspace_id): Path<String>,
) -> Result<Json<ProvidersBootstrapResponse>, (StatusCode, Json<serde_json::Value>)> {
    providers
        .workspace_providers_bootstrap_for_route(ProvidersBootstrapRouteRequest::new(workspace_id))
        .await
        .map(Json)
        .map_err(provider_bootstrap_route_error_json)
}

fn provider_bootstrap_route_error_json(
    error: ProvidersBootstrapRouteError,
) -> (StatusCode, Json<serde_json::Value>) {
    let status = match error.kind() {
        ProvidersBootstrapRouteErrorKind::BadRequest => StatusCode::BAD_REQUEST,
        ProvidersBootstrapRouteErrorKind::NotFound => StatusCode::NOT_FOUND,
        ProvidersBootstrapRouteErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(error.body().clone()))
}
