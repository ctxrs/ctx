use super::*;

pub(crate) async fn start_codex_login(
    State(providers): State<ProviderAccountsHandle>,
    mobile_auth: Option<Extension<MobileAuthContext>>,
    Json(req): Json<CodexLoginStartRouteRequest>,
) -> Result<Json<CodexLoginStartRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    reject_mobile_auth(mobile_auth)?;
    providers
        .start_codex_login_for_route(req)
        .await
        .map(Json)
        .map_err(codex_login_route_error)
}

pub(crate) async fn get_codex_login(
    State(providers): State<ProviderAccountsHandle>,
    mobile_auth: Option<Extension<MobileAuthContext>>,
    Path(id): Path<String>,
) -> Result<Json<CodexLoginStatusRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    reject_mobile_auth(mobile_auth)?;
    providers
        .codex_login_status_for_route(&id)
        .await
        .map(Json)
        .map_err(codex_login_route_error)
}

pub(crate) async fn complete_codex_login(
    State(providers): State<ProviderAccountsHandle>,
    Path(id): Path<String>,
    mobile_auth: Option<Extension<MobileAuthContext>>,
    Json(req): Json<CodexLoginCompleteRouteRequest>,
) -> Result<Json<CodexLoginCompleteRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    reject_mobile_auth(mobile_auth)?;
    providers
        .complete_codex_login_for_route(&id, req)
        .await
        .map(Json)
        .map_err(codex_login_route_error)
}

fn codex_login_route_error(error: CodexLoginRouteError) -> (StatusCode, Json<ApiErrorResp>) {
    let status = match error.kind() {
        CodexLoginRouteErrorKind::BadRequest => StatusCode::BAD_REQUEST,
        CodexLoginRouteErrorKind::NotFound => StatusCode::NOT_FOUND,
        CodexLoginRouteErrorKind::Conflict => StatusCode::CONFLICT,
        CodexLoginRouteErrorKind::Unauthorized => StatusCode::UNAUTHORIZED,
        CodexLoginRouteErrorKind::BadGateway => StatusCode::BAD_GATEWAY,
        CodexLoginRouteErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(ApiErrorResp {
            error: error.message().to_string(),
        }),
    )
}

#[cfg(test)]
mod tests;
