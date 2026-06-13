use super::*;

pub(in crate::api) async fn install_provider(
    State(providers): State<ProviderInstallHandle>,
    Path(id): Path<String>,
    Query(query): Query<RawInstallTargetQuery>,
) -> Result<Json<ProviderInstallStartRouteResponse>, (StatusCode, Json<serde_json::Value>)> {
    let response = providers
        .start_provider_install_for_route(&id, query.target.as_deref())
        .await
        .map_err(provider_install_error_response)?;

    Ok(Json(response))
}

pub(in crate::api) async fn install_all_providers(
    State(providers): State<ProviderInstallHandle>,
    Query(query): Query<RawInstallTargetQuery>,
) -> Result<Json<Vec<ProviderInstallStartRouteResponse>>, (StatusCode, Json<serde_json::Value>)> {
    let response = providers
        .start_all_provider_installs_for_route(query.target.as_deref())
        .await
        .map_err(provider_install_error_response)?;
    Ok(Json(response))
}
