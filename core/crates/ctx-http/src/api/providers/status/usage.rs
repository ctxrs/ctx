use super::*;

fn provider_usage_route_error(error: ProviderUsageRouteError) -> (StatusCode, Json<ApiErrorResp>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiErrorResp {
            error: error.message().to_string(),
        }),
    )
}

pub(crate) async fn get_provider_usage(
    State(providers): State<ProviderUsageHandle>,
    Path(id): Path<String>,
    Query(query): Query<ProviderUsageRouteQuery>,
) -> Result<Json<ProviderUsageRouteSnapshot>, (StatusCode, Json<ApiErrorResp>)> {
    let snapshot = providers
        .provider_usage_for_route(&id, query)
        .await
        .map_err(provider_usage_route_error)?;
    Ok(Json(snapshot))
}
