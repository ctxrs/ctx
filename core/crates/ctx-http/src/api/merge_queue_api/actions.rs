use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use ctx_daemon::daemon::MergeQueueApiHandle;
use ctx_route_contracts::merge_queue::{
    ListMergeQueueEntriesRouteRequest, MergeQueueEntryRouteError, MergeQueueEntryRouteErrorKind,
    MergeQueueEntryRouteParams, MergeQueueEntryRouteResponse,
};

use crate::api::errors::ApiErrorResp;

pub(in crate::api) async fn list_merge_queue_entries(
    State(state): State<MergeQueueApiHandle>,
    Query(params): Query<ListMergeQueueEntriesRouteRequest>,
) -> Result<Json<Vec<MergeQueueEntryRouteResponse>>, StatusCode> {
    let entries = state
        .list_merge_queue_entry_responses_for_route(params)
        .await
        .map_err(merge_queue_entry_status)?;
    Ok(Json(entries))
}

pub(in crate::api) async fn cancel_merge_queue_entry(
    State(state): State<MergeQueueApiHandle>,
    Path(params): Path<MergeQueueEntryRouteParams>,
) -> Result<Json<MergeQueueEntryRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let entry = state
        .cancel_merge_queue_entry_for_route(params)
        .await
        .map_err(merge_queue_entry_api_error)?;
    Ok(Json(entry))
}

pub(in crate::api) async fn retry_merge_queue_entry(
    State(state): State<MergeQueueApiHandle>,
    Path(params): Path<MergeQueueEntryRouteParams>,
) -> Result<Json<MergeQueueEntryRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let entry = state
        .retry_merge_queue_entry_for_route(params)
        .await
        .map_err(merge_queue_entry_api_error)?;
    Ok(Json(entry))
}

fn merge_queue_entry_status(error: MergeQueueEntryRouteError) -> StatusCode {
    match error.kind() {
        MergeQueueEntryRouteErrorKind::BadRequest => StatusCode::BAD_REQUEST,
        MergeQueueEntryRouteErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn merge_queue_entry_api_error(
    error: MergeQueueEntryRouteError,
) -> (StatusCode, Json<ApiErrorResp>) {
    (
        merge_queue_entry_status(error.clone()),
        Json(ApiErrorResp {
            error: error.message().to_string(),
        }),
    )
}
