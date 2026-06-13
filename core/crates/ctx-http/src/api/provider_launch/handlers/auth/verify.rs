use super::*;

pub(in crate::api) async fn verify_provider_for_workspace(
    State(provider_workspace_auth): State<ProviderWorkspaceAuthHandle>,
    Path((ws_id, provider_id)): Path<(String, String)>,
) -> Result<Json<ProviderAuthCheckRouteResponse>, (StatusCode, Json<serde_json::Value>)> {
    provider_workspace_auth
        .verify_provider_for_workspace_for_route(VerifyProviderForWorkspaceRouteRequest::new(
            ws_id,
            provider_id,
        ))
        .await
        .map(Json)
        .map_err(provider_auth_check_route_error)
}
