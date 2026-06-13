use super::*;
use ctx_daemon::daemon::WebSessionRouteHandle;
use ctx_route_contracts::web_sessions::{
    WebSessionCreateRouteRequest, WebSessionRouteError, WebSessionRouteErrorKind,
};

pub(in crate::api) async fn create_web_session(
    State(state): State<WebSessionRouteHandle>,
    Json(payload): Json<WebSessionCreateRouteRequest>,
) -> Result<Json<WebSessionInfo>, (StatusCode, Json<ApiErrorResp>)> {
    let info = state
        .create_web_session_for_route(payload)
        .await
        .map_err(web_session_route_api_error)?;
    Ok(Json(info))
}

fn web_session_route_api_error(error: WebSessionRouteError) -> (StatusCode, Json<ApiErrorResp>) {
    let status = match error.kind() {
        WebSessionRouteErrorKind::BadRequest => StatusCode::BAD_REQUEST,
        WebSessionRouteErrorKind::Forbidden => StatusCode::FORBIDDEN,
        WebSessionRouteErrorKind::NotFound => StatusCode::NOT_FOUND,
        WebSessionRouteErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(ApiErrorResp {
            error: error.message().to_string(),
        }),
    )
}
