use super::*;

pub(in crate::api) async fn get_workspace_provider_model_preference(
    State(preferences): State<WorkspaceProviderModelPreferenceHandle>,
    Path((id, provider_id)): Path<(String, String)>,
) -> Result<Json<WorkspaceProviderModelPreferenceRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    preferences
        .workspace_provider_model_preference_for_route(
            WorkspaceProviderModelPreferenceRouteParams::new(id, provider_id),
        )
        .await
        .map_err(workspace_route_api_error)
        .map(Json)
}

pub(in crate::api) async fn update_workspace_provider_model_preference(
    State(preferences): State<WorkspaceProviderModelPreferenceHandle>,
    Path((id, provider_id)): Path<(String, String)>,
    Json(req): Json<UpdateWorkspaceProviderModelPreferenceRouteRequest>,
) -> Result<Json<WorkspaceProviderModelPreferenceRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    preferences
        .update_workspace_provider_model_preference_for_route(
            WorkspaceProviderModelPreferenceRouteParams::new(id, provider_id),
            req,
        )
        .await
        .map_err(workspace_route_api_error)
        .map(Json)
}
