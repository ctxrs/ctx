use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;

use ctx_daemon::daemon::{MobileStoreHandle, WorkspaceStreamHandle};
use ctx_mobile_access_service::route_contract::{
    MobileAccessRouteError, MobileAccessRouteErrorKind, MobileSecureWorkspaceStreamRouteParams,
};

#[path = "secure_mobile/context.rs"]
mod context;
#[path = "secure_mobile/send_loop.rs"]
mod send_loop;
#[path = "secure_mobile/socket.rs"]
mod socket;

use super::super::MobileSecureStreamQuery;
use socket::handle_mobile_secure_ws;

pub(in crate::api) async fn mobile_secure_workspace_stream_ws(
    ws: WebSocketUpgrade,
    State(mobile_store): State<MobileStoreHandle>,
    State(workspace_stream): State<WorkspaceStreamHandle>,
    Path(id): Path<String>,
    Query(query): Query<MobileSecureStreamQuery>,
) -> impl IntoResponse {
    let admission = match mobile_store
        .admit_mobile_secure_workspace_stream_for_route(
            MobileSecureWorkspaceStreamRouteParams::new(id, query.device_id, query.token),
        )
        .await
    {
        Ok(admission) => admission,
        Err(error) => return mobile_secure_stream_route_status(error).into_response(),
    };
    let workspace_id = admission.workspace_id;
    let stream_context = admission.context;
    ws.on_upgrade(move |socket| async move {
        if let Err(err) =
            handle_mobile_secure_ws(socket, workspace_stream, workspace_id, stream_context).await
        {
            tracing::warn!("secure mobile ws ended: {err:#}");
        }
    })
}

fn mobile_secure_stream_route_status(error: MobileAccessRouteError) -> StatusCode {
    match error.kind() {
        MobileAccessRouteErrorKind::BadRequest => StatusCode::BAD_REQUEST,
        MobileAccessRouteErrorKind::Unauthorized => StatusCode::UNAUTHORIZED,
        MobileAccessRouteErrorKind::NotFound => StatusCode::NOT_FOUND,
        MobileAccessRouteErrorKind::Forbidden => StatusCode::FORBIDDEN,
        MobileAccessRouteErrorKind::Conflict => StatusCode::CONFLICT,
        MobileAccessRouteErrorKind::BadGateway => StatusCode::BAD_GATEWAY,
        MobileAccessRouteErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
