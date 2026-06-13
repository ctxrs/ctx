use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;

use super::errors::ApiErrorResp;
use ctx_daemon::daemon::TerminalRouteHandle;
use ctx_route_contracts::terminals::{
    CreateTerminalRouteRequest, DeleteTerminalRouteParams, ListWorkspaceTerminalsRouteParams,
    MintTerminalStreamTokenRouteParams, TerminalRouteError, TerminalRouteErrorKind,
    TerminalSessionRouteResponse, TerminalStreamConnectRouteResponse,
};

pub(super) async fn list_workspace_terminals(
    State(state): State<TerminalRouteHandle>,
    Path(id): Path<String>,
) -> Result<Json<Vec<TerminalSessionRouteResponse>>, StatusCode> {
    state
        .list_workspace_terminal_responses_for_route(ListWorkspaceTerminalsRouteParams::new(id))
        .await
        .map(Json)
        .map_err(terminal_route_status)
}

pub(super) async fn create_workspace_terminal(
    State(state): State<TerminalRouteHandle>,
    Path(id): Path<String>,
    Json(req): Json<CreateTerminalRouteRequest>,
) -> Result<Json<TerminalSessionRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let session = state
        .create_workspace_terminal_for_route(&id, req)
        .await
        .map_err(terminal_route_error_response)?;

    Ok(Json(session))
}

fn terminal_route_error_response(error: TerminalRouteError) -> (StatusCode, Json<ApiErrorResp>) {
    let status = terminal_route_status_for_kind(error.kind());
    (
        status,
        Json(ApiErrorResp {
            error: error.message().to_string(),
        }),
    )
}

pub(super) async fn delete_terminal(
    State(state): State<TerminalRouteHandle>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    state
        .delete_terminal_for_route(DeleteTerminalRouteParams::new(id))
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(terminal_route_status)
}

pub(super) async fn mint_terminal_stream_token(
    State(state): State<TerminalRouteHandle>,
    Path(id): Path<String>,
) -> Result<Json<TerminalStreamConnectRouteResponse>, StatusCode> {
    state
        .mint_terminal_stream_token_for_route(MintTerminalStreamTokenRouteParams::new(id))
        .await
        .map(Json)
        .map_err(terminal_route_status)
}

fn terminal_route_status(error: TerminalRouteError) -> StatusCode {
    terminal_route_status_for_kind(error.kind())
}

fn terminal_route_status_for_kind(kind: TerminalRouteErrorKind) -> StatusCode {
    match kind {
        TerminalRouteErrorKind::BadRequest => StatusCode::BAD_REQUEST,
        TerminalRouteErrorKind::Unauthorized => StatusCode::UNAUTHORIZED,
        TerminalRouteErrorKind::NotFound => StatusCode::NOT_FOUND,
        TerminalRouteErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
