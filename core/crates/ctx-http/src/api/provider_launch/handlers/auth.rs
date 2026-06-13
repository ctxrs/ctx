use super::*;

mod verify;

pub(in crate::api) use verify::verify_provider_for_workspace;

pub(in crate::api) async fn authenticate_provider_for_workspace(
    State(provider_workspace_auth): State<ProviderWorkspaceAuthHandle>,
    Path((ws_id, provider_id)): Path<(String, String)>,
    req: Option<Json<AuthenticateProviderForWorkspaceRouteBody>>,
) -> Result<Json<ProviderAuthCheckRouteResponse>, (StatusCode, Json<serde_json::Value>)> {
    let method_id = req.and_then(|value| value.0.into_method_id());
    provider_workspace_auth
        .authenticate_provider_for_workspace_for_route(
            AuthenticateProviderForWorkspaceRouteRequest::new(ws_id, provider_id, method_id),
        )
        .await
        .map(Json)
        .map_err(provider_auth_check_route_error)
}

pub(super) fn provider_auth_check_route_error(
    error: ProviderAuthCheckRouteError,
) -> (StatusCode, Json<serde_json::Value>) {
    let status = match error.status() {
        ProviderAuthCheckRouteErrorStatus::BadRequest => StatusCode::BAD_REQUEST,
        ProviderAuthCheckRouteErrorStatus::NotFound => StatusCode::NOT_FOUND,
        ProviderAuthCheckRouteErrorStatus::InternalServerError => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(error.body().clone()))
}
