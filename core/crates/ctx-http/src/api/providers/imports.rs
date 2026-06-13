use super::*;
use ctx_provider_auth_import::{
    ProviderAuthImportCandidatesRouteResponse, ProviderAuthImportProfilesRouteResponse,
    ProviderAuthImportRouteError, ProviderAuthImportRouteRequest, ProviderAuthImportRouteResponse,
};

pub(crate) async fn list_provider_auth_import_candidates(
    State(providers): State<ProviderAuthImportHandle>,
) -> Result<Json<ProviderAuthImportCandidatesRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let response = providers
        .list_provider_auth_import_candidates_for_route()
        .await
        .map_err(provider_auth_import_error_response)?;
    Ok(Json(response))
}

pub(crate) async fn list_provider_auth_import_profiles(
    State(providers): State<ProviderAuthImportHandle>,
) -> Result<Json<ProviderAuthImportProfilesRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let response = providers
        .list_provider_auth_import_profiles_for_route()
        .await
        .map_err(provider_auth_import_error_response)?;
    Ok(Json(response))
}

pub(crate) async fn import_provider_auth_candidates(
    State(providers): State<ProviderAuthImportHandle>,
    Json(req): Json<ProviderAuthImportRouteRequest>,
) -> Result<Json<ProviderAuthImportRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let response = providers
        .import_provider_auth_candidates_for_route(req)
        .await
        .map_err(provider_auth_import_error_response)?;
    Ok(Json(response))
}

fn provider_auth_import_error_response(
    error: ProviderAuthImportRouteError,
) -> (StatusCode, Json<ApiErrorResp>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiErrorResp {
            error: error.message().to_string(),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_auth_import_error_response_preserves_redacted_candidate_errors() {
        let (status, Json(body)) = provider_auth_import_error_response(
            ProviderAuthImportRouteError::new("candidate scan failed: [REDACTED]"),
        );

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body.error, "candidate scan failed: [REDACTED]");
    }
}
