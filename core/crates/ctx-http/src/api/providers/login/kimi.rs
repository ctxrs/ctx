use super::*;

pub(crate) async fn start_kimi_login(
    State(providers): State<ProviderAccountsHandle>,
    mobile_auth: Option<Extension<MobileAuthContext>>,
    Json(req): Json<ProviderLoginStartRouteRequest>,
) -> Result<Json<ProviderLoginStartRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    reject_mobile_auth(mobile_auth)?;
    providers
        .start_kimi_login_for_route(req)
        .await
        .map(Json)
        .map_err(provider_login_route_error)
}

pub(crate) async fn get_kimi_login(
    State(providers): State<ProviderAccountsHandle>,
    mobile_auth: Option<Extension<MobileAuthContext>>,
    Path(id): Path<String>,
) -> Result<Json<KimiLoginStatusRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    reject_mobile_auth(mobile_auth)?;
    providers
        .kimi_login_status_for_route(&id)
        .await
        .map(Json)
        .map_err(provider_login_route_error)
}
