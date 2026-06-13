use super::common::provider_account_route_error;
use super::*;

pub(crate) async fn list_copilot_accounts(
    State(providers): State<ProviderAccountsHandle>,
) -> Result<Json<CopilotAccountsResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let response = providers
        .copilot_accounts_for_route()
        .await
        .map_err(provider_account_route_error)?;
    Ok(Json(response))
}

pub(crate) async fn upsert_copilot_account(
    State(providers): State<ProviderAccountsHandle>,
    Json(req): Json<CopilotAccountUpsertRouteRequest>,
) -> Result<Json<CopilotAccountsResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let response = providers
        .upsert_copilot_account_for_route(req)
        .await
        .map_err(provider_account_route_error)?;
    Ok(Json(response))
}

pub(crate) async fn set_copilot_active_account(
    State(providers): State<ProviderAccountsHandle>,
    Json(req): Json<ProviderActiveAccountRouteRequest>,
) -> Result<Json<CopilotAccountsResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let response = providers
        .set_active_copilot_account_for_route(req)
        .await
        .map_err(provider_account_route_error)?;
    Ok(Json(response))
}

pub(crate) async fn delete_copilot_account(
    State(providers): State<ProviderAccountsHandle>,
    Path(id): Path<String>,
) -> Result<Json<CopilotAccountsResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let response = providers
        .delete_copilot_account_for_route(&id)
        .await
        .map_err(provider_account_route_error)?;
    Ok(Json(response))
}
