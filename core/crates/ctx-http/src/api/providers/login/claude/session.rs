use super::*;

pub(crate) async fn start_claude_login(
    State(providers): State<ProviderAccountsHandle>,
    mobile_auth: Option<Extension<MobileAuthContext>>,
    Json(req): Json<ClaudeLoginStartRouteRequest>,
) -> Result<Json<ClaudeLoginStartRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    reject_mobile_auth(mobile_auth)?;
    providers
        .start_claude_login_for_route(req)
        .await
        .map(Json)
        .map_err(claude_login_route_error)
}

pub(crate) async fn get_claude_login(
    State(providers): State<ProviderAccountsHandle>,
    mobile_auth: Option<Extension<MobileAuthContext>>,
    Path(id): Path<String>,
) -> Result<Json<ClaudeLoginStatusRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    reject_mobile_auth(mobile_auth)?;
    providers
        .claude_login_status_for_route(&id)
        .await
        .map(Json)
        .map_err(claude_login_route_error)
}

fn claude_login_route_error(error: ClaudeLoginRouteError) -> (StatusCode, Json<ApiErrorResp>) {
    let status = match error.kind() {
        ClaudeLoginRouteErrorKind::BadRequest => StatusCode::BAD_REQUEST,
        ClaudeLoginRouteErrorKind::NotFound => StatusCode::NOT_FOUND,
        ClaudeLoginRouteErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(ApiErrorResp {
            error: error.message().to_string(),
        }),
    )
}
