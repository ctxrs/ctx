use super::common::provider_account_route_error;
use super::*;

pub(crate) async fn list_mistral_accounts(
    State(providers): State<ProviderAccountsHandle>,
) -> Result<Json<MistralAccountsResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let response = providers
        .mistral_accounts_for_route()
        .await
        .map_err(provider_account_route_error)?;
    Ok(Json(response))
}

pub(crate) async fn upsert_mistral_account(
    State(providers): State<ProviderAccountsHandle>,
    Json(req): Json<MistralAccountUpsertRouteRequest>,
) -> Result<Json<MistralAccountsResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let response = providers
        .upsert_mistral_account_for_route(req)
        .await
        .map_err(provider_account_route_error)?;
    Ok(Json(response))
}

pub(crate) async fn set_mistral_active_account(
    State(providers): State<ProviderAccountsHandle>,
    Json(req): Json<ProviderActiveAccountRouteRequest>,
) -> Result<Json<MistralAccountsResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let response = providers
        .set_active_mistral_account_for_route(req)
        .await
        .map_err(provider_account_route_error)?;
    Ok(Json(response))
}

pub(crate) async fn delete_mistral_account(
    State(providers): State<ProviderAccountsHandle>,
    Path(id): Path<String>,
) -> Result<Json<MistralAccountsResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let response = providers
        .delete_mistral_account_for_route(&id)
        .await
        .map_err(provider_account_route_error)?;
    Ok(Json(response))
}
