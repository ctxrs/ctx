use super::common::provider_account_route_error;
use super::*;

pub(crate) async fn list_gemini_accounts(
    State(providers): State<ProviderAccountsHandle>,
) -> Result<Json<GeminiAccountsResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let response = providers
        .gemini_accounts_for_route()
        .await
        .map_err(provider_account_route_error)?;
    Ok(Json(response))
}

pub(crate) async fn upsert_gemini_account(
    State(providers): State<ProviderAccountsHandle>,
    Json(req): Json<GeminiAccountUpsertRouteRequest>,
) -> Result<Json<GeminiAccountsResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let response = providers
        .upsert_gemini_account_for_route(req)
        .await
        .map_err(provider_account_route_error)?;
    Ok(Json(response))
}

pub(crate) async fn set_gemini_active_account(
    State(providers): State<ProviderAccountsHandle>,
    Json(req): Json<ProviderActiveAccountRouteRequest>,
) -> Result<Json<GeminiAccountsResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let response = providers
        .set_active_gemini_account_for_route(req)
        .await
        .map_err(provider_account_route_error)?;
    Ok(Json(response))
}

pub(crate) async fn delete_gemini_account(
    State(providers): State<ProviderAccountsHandle>,
    Path(id): Path<String>,
) -> Result<Json<GeminiAccountsResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let response = providers
        .delete_gemini_account_for_route(&id)
        .await
        .map_err(provider_account_route_error)?;
    Ok(Json(response))
}
