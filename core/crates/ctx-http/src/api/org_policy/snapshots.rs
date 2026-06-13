use super::common::policy_api_error;
use super::*;

pub(in crate::api) async fn cache_org_policy_snapshot(
    State(state): State<OrgPolicyHandle>,
    Path(org_id): Path<String>,
    Json(snapshot): Json<CacheOrgPolicySnapshotRouteRequest>,
) -> Result<Json<OrgPolicySnapshotRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    state
        .cache_org_policy_snapshot_for_route(OrgPolicyOrgRouteParams::new(org_id), snapshot)
        .await
        .map(Json)
        .map_err(policy_api_error)
}
