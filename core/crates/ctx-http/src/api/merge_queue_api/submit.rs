use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::Json;
use ctx_daemon::daemon::MergeQueueApiHandle;
use ctx_route_contracts::merge_queue::{
    MergeQueueEntryRouteResponse, MergeQueueSubmitRouteError, MergeQueueSubmitRouteErrorKind,
    SubmitMergeQueueEntryRouteRequest,
};

use crate::api::errors::ApiErrorResp;

pub(in crate::api) async fn submit_merge_queue_entry(
    State(merge_queue): State<MergeQueueApiHandle>,
    mcp_auth: Option<Extension<ctx_mcp_auth::McpAuthContext>>,
    Json(req): Json<SubmitMergeQueueEntryRouteRequest>,
) -> Result<Json<MergeQueueEntryRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let entry = merge_queue
        .submit_merge_queue_entry_for_route(req, mcp_auth.map(|Extension(auth)| auth))
        .await
        .map_err(merge_queue_submit_route_error)?;
    Ok(Json(entry))
}

fn merge_queue_submit_route_error(
    error: MergeQueueSubmitRouteError,
) -> (StatusCode, Json<ApiErrorResp>) {
    let status = match error.kind() {
        MergeQueueSubmitRouteErrorKind::BadRequest => StatusCode::BAD_REQUEST,
        MergeQueueSubmitRouteErrorKind::Unauthorized => StatusCode::UNAUTHORIZED,
        MergeQueueSubmitRouteErrorKind::NotFound => StatusCode::NOT_FOUND,
        MergeQueueSubmitRouteErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(ApiErrorResp {
            error: error.message().to_string(),
        }),
    )
}
