use super::*;

#[path = "workspace_active/send_loop.rs"]
mod send_loop;
#[path = "workspace_active/socket.rs"]
mod socket;

use ctx_route_contracts::workspaces::{
    WorkspaceStreamRouteError, WorkspaceStreamRouteErrorKind, WorkspaceStreamRouteParams,
};
use socket::handle_workspace_active_snapshot_ws;

fn workspace_stream_route_status(error: WorkspaceStreamRouteError) -> StatusCode {
    match error.kind() {
        WorkspaceStreamRouteErrorKind::BadRequest => StatusCode::BAD_REQUEST,
        WorkspaceStreamRouteErrorKind::NotFound => StatusCode::NOT_FOUND,
        WorkspaceStreamRouteErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

pub(crate) async fn workspace_active_snapshot_stream_ws(
    ws: WebSocketUpgrade,
    State(state): State<WorkspaceStreamHandle>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let admission = match state
        .admit_workspace_active_stream_for_route(WorkspaceStreamRouteParams::new(id))
        .await
    {
        Ok(admission) => admission,
        Err(error) => return workspace_stream_route_status(error).into_response(),
    };
    let workspace_id = admission.workspace_id();
    ws.on_upgrade(move |socket| handle_workspace_active_snapshot_ws(socket, state, workspace_id))
}
