use super::common::provider_account_route_error;
use super::*;

pub(crate) async fn list_amp_accounts(
    State(providers): State<ProviderAccountsHandle>,
) -> Result<Json<AmpAccountsResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let response = providers
        .amp_accounts_for_route()
        .await
        .map_err(provider_account_route_error)?;
    Ok(Json(response))
}

pub(crate) async fn upsert_amp_account(
    State(providers): State<ProviderAccountsHandle>,
    Json(req): Json<AmpAccountUpsertRouteRequest>,
) -> Result<Json<AmpAccountsResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let response = providers
        .upsert_amp_account_for_route(req)
        .await
        .map_err(provider_account_route_error)?;
    Ok(Json(response))
}

pub(crate) async fn set_amp_active_account(
    State(providers): State<ProviderAccountsHandle>,
    Json(req): Json<ProviderActiveAccountRouteRequest>,
) -> Result<Json<AmpAccountsResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let response = providers
        .set_active_amp_account_for_route(req)
        .await
        .map_err(provider_account_route_error)?;
    Ok(Json(response))
}

pub(crate) async fn delete_amp_account(
    State(providers): State<ProviderAccountsHandle>,
    Path(id): Path<String>,
) -> Result<Json<AmpAccountsResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let response = providers
        .delete_amp_account_for_route(&id)
        .await
        .map_err(provider_account_route_error)?;
    Ok(Json(response))
}
