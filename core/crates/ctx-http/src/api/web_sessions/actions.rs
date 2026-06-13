use super::*;
use ctx_daemon::daemon::WebSessionRouteHandle;
use ctx_route_contracts::web_sessions::{
    WebSessionActionRouteRequest, WebSessionListRouteQuery, WebSessionRouteError,
    WebSessionRouteErrorKind,
};

fn web_session_route_status(error: WebSessionRouteError) -> StatusCode {
    match error.kind() {
        WebSessionRouteErrorKind::BadRequest => StatusCode::BAD_REQUEST,
        WebSessionRouteErrorKind::Forbidden => StatusCode::FORBIDDEN,
        WebSessionRouteErrorKind::NotFound => StatusCode::NOT_FOUND,
        WebSessionRouteErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

pub(in crate::api) async fn list_web_sessions(
    State(state): State<WebSessionRouteHandle>,
    Query(query): Query<WebSessionListRouteQuery>,
) -> Result<Json<Vec<WebSessionInfo>>, StatusCode> {
    let sessions = state
        .list_web_sessions_for_route(query)
        .await
        .map_err(web_session_route_status)?;
    Ok(Json(sessions))
}

pub(in crate::api) async fn get_web_session(
    State(state): State<WebSessionRouteHandle>,
    Path(id): Path<String>,
) -> Result<Json<WebSessionInfo>, StatusCode> {
    let info = state
        .get_web_session_for_route(&id)
        .await
        .map_err(web_session_route_status)?;
    Ok(Json(info))
}

pub(in crate::api) async fn run_web_session(
    State(state): State<WebSessionRouteHandle>,
    Path(id): Path<String>,
    Json(payload): Json<WebSessionActionRouteRequest>,
) -> Result<Json<WebSessionRunResponse>, StatusCode> {
    let resp = state
        .run_web_session_for_route(&id, payload)
        .await
        .map_err(web_session_route_status)?;
    Ok(Json(resp))
}

pub(in crate::api) async fn eval_web_session(
    State(state): State<WebSessionRouteHandle>,
    Path(id): Path<String>,
    Json(payload): Json<WebSessionActionRouteRequest>,
) -> Result<Json<WebSessionRunResponse>, StatusCode> {
    let resp = state
        .eval_web_session_for_route(&id, payload)
        .await
        .map_err(web_session_route_status)?;
    Ok(Json(resp))
}

pub(in crate::api) async fn close_web_session(
    State(state): State<WebSessionRouteHandle>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    state
        .close_web_session_for_route(&id)
        .await
        .map_err(web_session_route_status)?;
    Ok(StatusCode::NO_CONTENT)
}
