use super::login::reject_mobile_auth;
use super::*;
use crate::api::MobileAuthContext;
use axum::Extension;

pub(crate) async fn start_cursor_login(
    State(providers): State<ProviderAccountsHandle>,
    mobile_auth: Option<Extension<MobileAuthContext>>,
    Json(req): Json<CursorLoginStartRouteRequest>,
) -> Result<Json<CursorLoginStartRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    reject_mobile_auth(mobile_auth)?;
    providers
        .start_cursor_login_for_route(req)
        .await
        .map(Json)
        .map_err(cursor_login_route_error)
}

pub(crate) async fn get_cursor_login(
    State(providers): State<ProviderAccountsHandle>,
    mobile_auth: Option<Extension<MobileAuthContext>>,
    Path(id): Path<String>,
) -> Result<Json<CursorLoginStatusRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    reject_mobile_auth(mobile_auth)?;
    providers
        .cursor_login_status_for_route(&id)
        .await
        .map(Json)
        .map_err(cursor_login_route_error)
}

fn cursor_login_route_error(error: CursorLoginRouteError) -> (StatusCode, Json<ApiErrorResp>) {
    let status = match error.kind() {
        CursorLoginRouteErrorKind::BadRequest => StatusCode::BAD_REQUEST,
        CursorLoginRouteErrorKind::NotFound => StatusCode::NOT_FOUND,
        CursorLoginRouteErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(ApiErrorResp {
            error: error.message().to_string(),
        }),
    )
}
