use super::*;

pub(in crate::api) async fn repo_clone(
    mobile_auth: Option<Extension<MobileAuthContext>>,
    State(repo_onboarding): State<RepoOnboardingHandle>,
    Json(req): Json<RepoCloneRouteRequest>,
) -> Result<Json<RepoPathRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    reject_mobile_auth(mobile_auth)?;
    let response = repo_onboarding
        .clone_repo_for_route(req)
        .await
        .map_err(repo_onboarding_error_response)?;

    Ok(Json(response))
}
