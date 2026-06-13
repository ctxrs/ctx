use super::*;
use ctx_daemon::daemon::RunArchiveHandle;
use ctx_route_contracts::run_archive::{
    AcknowledgeRunArchiveIngestBatchRouteBody, AcknowledgeRunArchiveIngestBatchRouteRequest,
    AcknowledgeRunArchiveIngestBatchRouteResponse, BuildRunArchiveIngestBatchRouteRequest,
    BuildRunArchiveIngestBatchRouteResponse, RunArchiveBatchRouteQuery, RunArchiveRouteError,
    RunArchiveRouteErrorKind, RunArchiveRouteParams,
};

pub(super) async fn build_workspace_run_archive_ingest_batch(
    State(state): State<RunArchiveHandle>,
    Path((workspace_id, run_id)): Path<(String, String)>,
    Query(query): Query<RunArchiveBatchRouteQuery>,
) -> Result<Json<BuildRunArchiveIngestBatchRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let params = RunArchiveRouteParams::new(workspace_id, run_id);
    let batch = state
        .build_run_archive_ingest_batch_for_route(BuildRunArchiveIngestBatchRouteRequest::new(
            params, query,
        ))
        .await
        .map_err(run_archive_route_error)?;

    Ok(Json(batch))
}

pub(super) async fn acknowledge_workspace_run_archive_ingest_batch(
    State(state): State<RunArchiveHandle>,
    Path((workspace_id, run_id)): Path<(String, String)>,
    Query(query): Query<RunArchiveBatchRouteQuery>,
    Json(body): Json<AcknowledgeRunArchiveIngestBatchRouteBody>,
) -> Result<Json<AcknowledgeRunArchiveIngestBatchRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let params = RunArchiveRouteParams::new(workspace_id, run_id);

    state
        .acknowledge_run_archive_ingest_batch_for_route(
            AcknowledgeRunArchiveIngestBatchRouteRequest::new(params, query, body),
        )
        .await
        .map(Json)
        .map_err(run_archive_route_error)
}

fn run_archive_route_error(error: RunArchiveRouteError) -> (StatusCode, Json<ApiErrorResp>) {
    let status = match error.kind() {
        RunArchiveRouteErrorKind::BadRequest => StatusCode::BAD_REQUEST,
        RunArchiveRouteErrorKind::NotFound => StatusCode::NOT_FOUND,
        RunArchiveRouteErrorKind::Conflict => StatusCode::CONFLICT,
        RunArchiveRouteErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    };
    run_archive_api_error(status, error.message())
}

fn run_archive_api_error(
    status: StatusCode,
    error: impl Into<String>,
) -> (StatusCode, Json<ApiErrorResp>) {
    (
        status,
        Json(ApiErrorResp {
            error: error.into(),
        }),
    )
}
