use super::*;

pub(crate) async fn refresh_provider_matrix(
    State(providers): State<ProviderAdminHandle>,
) -> Result<Json<ProviderMatrixRefreshRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    providers
        .refresh_provider_matrix_for_route()
        .await
        .map(Json)
        .map_err(provider_admin_route_error)
}

fn provider_admin_route_error(error: ProviderAdminRouteError) -> (StatusCode, Json<ApiErrorResp>) {
    let status = match error.kind() {
        ProviderAdminRouteErrorKind::NotFound => StatusCode::NOT_FOUND,
        ProviderAdminRouteErrorKind::BadRequest => StatusCode::BAD_REQUEST,
        ProviderAdminRouteErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(ApiErrorResp {
            error: error.message().to_string(),
        }),
    )
}

pub(crate) async fn dev_restart_providers(
    State(providers): State<ProviderAdminHandle>,
    Json(req): Json<ProviderDevRestartRouteRequest>,
) -> Result<Json<ProviderDevRestartRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    providers
        .dev_restart_providers_for_route(req)
        .await
        .map(Json)
        .map_err(provider_admin_route_error)
}
