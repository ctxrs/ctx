use super::common::provider_account_route_error;
use super::*;

pub(crate) async fn list_qwen_accounts(
    State(providers): State<ProviderAccountsHandle>,
) -> Result<Json<QwenAccountsResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let response = providers
        .qwen_accounts_for_route()
        .await
        .map_err(provider_account_route_error)?;
    Ok(Json(response))
}

pub(crate) async fn upsert_qwen_account(
    State(providers): State<ProviderAccountsHandle>,
    Json(req): Json<QwenAccountUpsertRouteRequest>,
) -> Result<Json<QwenAccountsResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let response = providers
        .upsert_qwen_account_for_route(req)
        .await
        .map_err(provider_account_route_error)?;
    Ok(Json(response))
}

pub(crate) async fn set_qwen_active_account(
    State(providers): State<ProviderAccountsHandle>,
    Json(req): Json<ProviderActiveAccountRouteRequest>,
) -> Result<Json<QwenAccountsResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let response = providers
        .set_active_qwen_account_for_route(req)
        .await
        .map_err(provider_account_route_error)?;
    Ok(Json(response))
}

pub(crate) async fn delete_qwen_account(
    State(providers): State<ProviderAccountsHandle>,
    Path(id): Path<String>,
) -> Result<Json<QwenAccountsResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let response = providers
        .delete_qwen_account_for_route(&id)
        .await
        .map_err(provider_account_route_error)?;
    Ok(Json(response))
}
