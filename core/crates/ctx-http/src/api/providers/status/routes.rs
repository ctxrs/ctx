use super::*;
use ctx_provider_runtime::{
    ProviderStatusListRouteError, ProviderStatusRouteError, ProviderStatusRouteErrorKind,
};

pub(crate) async fn list_providers(
    State(providers): State<ProviderStatusHandle>,
    Query(query): Query<ProviderStatusRouteQuery>,
) -> Result<Json<Vec<ProviderStatus>>, StatusCode> {
    providers
        .providers_statuses_for_route(query)
        .await
        .map(Json)
        .map_err(provider_status_list_error)
}

pub(crate) async fn get_provider(
    State(providers): State<ProviderStatusHandle>,
    Path(id): Path<String>,
    Query(query): Query<ProviderStatusRouteQuery>,
) -> Result<Json<ProviderStatus>, (StatusCode, Json<serde_json::Value>)> {
    providers
        .provider_status_for_route(&id, query)
        .await
        .map(Json)
        .map_err(provider_status_route_error)
}

fn provider_status_list_error(_error: ProviderStatusListRouteError) -> StatusCode {
    StatusCode::BAD_REQUEST
}

fn provider_status_route_error(
    error: ProviderStatusRouteError,
) -> (StatusCode, Json<serde_json::Value>) {
    let status = match error.kind() {
        ProviderStatusRouteErrorKind::BadRequest => StatusCode::BAD_REQUEST,
        ProviderStatusRouteErrorKind::NotFound => StatusCode::NOT_FOUND,
    };
    (status, Json(error.body().clone()))
}
