use super::*;

pub(in crate::api) async fn get_install(
    State(providers): State<ProviderInstallHandle>,
    Path(install_id): Path<String>,
) -> Result<Json<ProviderInstallInfo>, StatusCode> {
    providers
        .get_provider_install_for_route(&install_id)
        .await
        .map(Json)
        .map_err(provider_install_status_only_error)
}

pub(in crate::api) async fn get_install_statuses(
    State(providers): State<ProviderInstallHandle>,
    Json(req): Json<ProviderInstallStatusesRouteRequest>,
) -> Result<Json<ProviderInstallStatusesRouteResponse>, (StatusCode, Json<serde_json::Value>)> {
    providers
        .get_provider_install_statuses_for_route(req)
        .await
        .map(Json)
        .map_err(provider_install_error_response)
}

pub(in crate::api) async fn cancel_install(
    State(providers): State<ProviderInstallHandle>,
    Path(install_id): Path<String>,
) -> Result<Json<ProviderInstallInfo>, StatusCode> {
    providers
        .cancel_provider_install_for_route(&install_id)
        .await
        .map(Json)
        .map_err(provider_install_status_only_error)
}

pub(in crate::api) async fn list_install_events(
    State(providers): State<ProviderInstallHandle>,
    Path(install_id): Path<String>,
) -> Result<Json<Vec<ProviderInstallProgressEvent>>, StatusCode> {
    providers
        .list_provider_install_events_for_route(&install_id)
        .await
        .map(Json)
        .map_err(provider_install_status_only_error)
}

pub(super) fn provider_install_status_only_error(
    error: ProviderInstallStatusOnlyRouteError,
) -> StatusCode {
    match error {
        ProviderInstallStatusOnlyRouteError::BadRequest => StatusCode::BAD_REQUEST,
        ProviderInstallStatusOnlyRouteError::NotFound => StatusCode::NOT_FOUND,
    }
}
