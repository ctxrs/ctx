use super::*;

pub(in crate::api) async fn repo_status(
    mobile_auth: Option<Extension<MobileAuthContext>>,
    State(repo_onboarding): State<RepoOnboardingHandle>,
    Json(req): Json<RepoStatusRouteRequest>,
) -> Result<Json<RepoStatusRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    reject_mobile_auth(mobile_auth)?;

    let response = repo_onboarding
        .inspect_repo_status_for_route(req)
        .await
        .map_err(repo_onboarding_error_response)?;
    Ok(Json(response))
}
