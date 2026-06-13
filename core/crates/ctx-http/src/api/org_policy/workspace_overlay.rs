use super::common::policy_api_error;
use super::*;

pub(in crate::api) async fn get_workspace_org_policy(
    State(state): State<WorkspaceOrgPolicyHandle>,
    Path(id): Path<String>,
) -> Result<Json<WorkspacePolicyOverlayOptionalRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    state
        .get_workspace_policy_overlay_for_route(OrgPolicyWorkspaceRouteParams::new(id))
        .await
        .map(Json)
        .map_err(policy_api_error)
}

pub(in crate::api) async fn upsert_workspace_org_policy(
    State(state): State<WorkspaceOrgPolicyHandle>,
    Path(id): Path<String>,
    Json(overlay): Json<UpsertWorkspacePolicyOverlayRouteRequest>,
) -> Result<Json<WorkspacePolicyOverlayRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    state
        .upsert_workspace_policy_overlay_for_route(OrgPolicyWorkspaceRouteParams::new(id), overlay)
        .await
        .map(Json)
        .map_err(policy_api_error)
}
