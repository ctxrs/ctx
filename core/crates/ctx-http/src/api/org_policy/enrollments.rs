use super::common::policy_api_error;
use super::*;

pub(in crate::api) async fn list_daemon_enrollments(
    State(state): State<OrgPolicyHandle>,
) -> Result<Json<DaemonEnrollmentsRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    state
        .list_daemon_enrollments_for_route()
        .await
        .map(Json)
        .map_err(policy_api_error)
}

pub(in crate::api) async fn upsert_daemon_enrollment(
    State(state): State<OrgPolicyHandle>,
    Path(org_id): Path<String>,
    Json(enrollment): Json<UpsertDaemonEnrollmentRouteRequest>,
) -> Result<Json<DaemonEnrollmentRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    state
        .upsert_daemon_enrollment_for_route(OrgPolicyOrgRouteParams::new(org_id), enrollment)
        .await
        .map(Json)
        .map_err(policy_api_error)
}
