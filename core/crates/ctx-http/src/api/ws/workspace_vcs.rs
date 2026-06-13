use super::*;

mod buffer;
mod metrics;
mod send_loop;
mod socket;
mod subscription;
#[cfg(test)]
#[path = "workspace_vcs/tests.rs"]
mod tests;

#[cfg(test)]
use self::buffer::VcsPendingBuffer;
#[cfg(test)]
use self::metrics::VcsStreamMetrics;
use self::socket::handle_workspace_vcs_ws;
use ctx_route_contracts::workspaces::{
    WorkspaceStreamRouteError, WorkspaceStreamRouteErrorKind, WorkspaceStreamRouteParams,
};

fn workspace_stream_route_status(error: WorkspaceStreamRouteError) -> StatusCode {
    match error.kind() {
        WorkspaceStreamRouteErrorKind::BadRequest => StatusCode::BAD_REQUEST,
        WorkspaceStreamRouteErrorKind::NotFound => StatusCode::NOT_FOUND,
        WorkspaceStreamRouteErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

pub(crate) async fn workspace_vcs_stream_ws(
    ws: WebSocketUpgrade,
    State(state): State<WorkspaceVcsStreamHandle>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let admission = match state
        .admit_workspace_vcs_stream_for_route(WorkspaceStreamRouteParams::new(id))
        .await
    {
        Ok(admission) => admission,
        Err(error) => return workspace_stream_route_status(error).into_response(),
    };
    let workspace_id = admission.workspace_id();
    ws.on_upgrade(move |socket| handle_workspace_vcs_ws(socket, state, workspace_id))
}
