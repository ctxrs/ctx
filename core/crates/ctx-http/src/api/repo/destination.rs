use super::*;

pub(in crate::api) async fn repo_validate_destination(
    mobile_auth: Option<Extension<MobileAuthContext>>,
    State(repo_onboarding): State<RepoOnboardingHandle>,
    Json(req): Json<RepoValidateDestinationRouteRequest>,
) -> Result<Json<RepoPathRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    reject_mobile_auth(mobile_auth)?;
    validate_destination(repo_onboarding, req).await
}

pub(in crate::api) async fn repo_validate_destination_get(
    mobile_auth: Option<Extension<MobileAuthContext>>,
    State(repo_onboarding): State<RepoOnboardingHandle>,
    Query(req): Query<RepoValidateDestinationRouteRequest>,
) -> Result<Json<RepoPathRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    reject_mobile_auth(mobile_auth)?;
    validate_destination(repo_onboarding, req).await
}

async fn validate_destination(
    repo_onboarding: RepoOnboardingHandle,
    req: RepoValidateDestinationRouteRequest,
) -> Result<Json<RepoPathRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let response = repo_onboarding
        .validate_repo_destination_for_route(req)
        .await
        .map_err(repo_onboarding_error_response)?;

    Ok(Json(response))
}

/// Returns a unique staging path under data_root/workspaces/staging/<uuid>.
/// Used for disk-isolated clone/new: the daemon manages the path so the wizard
/// doesn't need to ask the user for a host destination.
pub(in crate::api) async fn repo_staging_path(
    mobile_auth: Option<Extension<MobileAuthContext>>,
    State(repo_onboarding): State<RepoOnboardingHandle>,
) -> Result<Json<RepoPathRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    reject_mobile_auth(mobile_auth)?;
    let response = repo_onboarding
        .create_repo_staging_path_for_route()
        .await
        .map_err(repo_onboarding_error_response)?;

    Ok(Json(response))
}
