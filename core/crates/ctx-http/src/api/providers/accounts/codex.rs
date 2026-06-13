use super::common::{internal_error, provider_account_route_error};
use super::*;

#[path = "codex/usage.rs"]
mod usage;

pub(crate) use usage::get_codex_accounts_usage;

pub(crate) async fn list_codex_accounts(
    State(providers): State<ProviderAccountsHandle>,
) -> Result<Json<CodexAccountsResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let response = providers
        .codex_accounts_for_route()
        .await
        .map_err(provider_account_route_error)?;
    Ok(Json(response))
}

pub(crate) async fn probe_host_codex_import(
    State(providers): State<ProviderAccountsHandle>,
) -> Result<Json<CodexHostImportProbeRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    Ok(Json(providers.codex_host_import_probe_for_route().await))
}

pub(crate) async fn import_host_codex_auth(
    State(providers): State<ProviderAccountsHandle>,
    Json(req): Json<CodexHostImportRouteRequest>,
) -> Result<Json<CodexAccountsResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let response = providers
        .import_host_codex_auth_for_route(req)
        .await
        .map_err(provider_account_route_error)?;
    Ok(Json(response))
}

pub(crate) async fn set_codex_active_account(
    State(providers): State<ProviderAccountsHandle>,
    Json(req): Json<ProviderActiveAccountRouteRequest>,
) -> Result<Json<CodexAccountsResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let response = providers
        .set_active_codex_account_for_route(req)
        .await
        .map_err(provider_account_route_error)?;
    Ok(Json(response))
}

pub(crate) async fn delete_codex_account(
    State(providers): State<ProviderAccountsHandle>,
    Path(id): Path<String>,
) -> Result<Json<CodexAccountsResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let response = providers
        .delete_codex_account_for_route(&id)
        .await
        .map_err(provider_account_route_error)?;
    Ok(Json(response))
}
