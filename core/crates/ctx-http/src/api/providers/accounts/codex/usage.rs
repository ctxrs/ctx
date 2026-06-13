use super::*;

pub(crate) async fn get_codex_accounts_usage(
    State(providers): State<ProviderUsageHandle>,
    Query(query): Query<ProviderUsageRouteQuery>,
) -> Result<Json<CodexAccountsUsageRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    providers
        .codex_accounts_usage_for_route(query)
        .await
        .map(Json)
        .map_err(internal_error)
}
