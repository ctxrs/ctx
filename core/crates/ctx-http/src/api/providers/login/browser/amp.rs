use super::*;

pub(crate) async fn start_amp_login(
    State(providers): State<ProviderAccountsHandle>,
    mobile_auth: Option<Extension<MobileAuthContext>>,
    Json(req): Json<ProviderLoginStartRouteRequest>,
) -> Result<Json<ProviderLoginStartRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    reject_mobile_auth(mobile_auth)?;
    Ok(Json(providers.start_amp_login_for_route(req).await))
}

pub(crate) async fn get_amp_login(
    State(providers): State<ProviderAccountsHandle>,
    mobile_auth: Option<Extension<MobileAuthContext>>,
    Path(id): Path<String>,
) -> Result<Json<AmpLoginStatusRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    reject_mobile_auth(mobile_auth)?;
    providers
        .amp_login_status_for_route(&id)
        .await
        .map(Json)
        .map_err(provider_login_route_error)
}
