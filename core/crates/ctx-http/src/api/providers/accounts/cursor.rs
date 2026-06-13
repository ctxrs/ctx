use super::common::provider_account_route_error;
use super::*;

pub(crate) async fn list_cursor_accounts(
    State(providers): State<ProviderAccountsHandle>,
) -> Result<Json<CursorAccountsResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let response = providers
        .cursor_accounts_for_route()
        .await
        .map_err(provider_account_route_error)?;
    Ok(Json(response))
}

pub(crate) async fn upsert_cursor_account(
    State(providers): State<ProviderAccountsHandle>,
    Json(req): Json<CursorAccountUpsertRouteRequest>,
) -> Result<Json<CursorAccountsResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let response = providers
        .upsert_cursor_account_for_route(req)
        .await
        .map_err(provider_account_route_error)?;
    Ok(Json(response))
}

pub(crate) async fn set_cursor_active_account(
    State(providers): State<ProviderAccountsHandle>,
    Json(req): Json<ProviderActiveAccountRouteRequest>,
) -> Result<Json<CursorAccountsResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let response = providers
        .set_active_cursor_account_for_route(req)
        .await
        .map_err(provider_account_route_error)?;
    Ok(Json(response))
}

pub(crate) async fn delete_cursor_account(
    State(providers): State<ProviderAccountsHandle>,
    Path(id): Path<String>,
) -> Result<Json<CursorAccountsResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let response = providers
        .delete_cursor_account_for_route(&id)
        .await
        .map_err(provider_account_route_error)?;
    Ok(Json(response))
}
