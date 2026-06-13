use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::Response;
use ctx_daemon::daemon::MergeQueueApiHandle;
use ctx_route_contracts::downloads::TextRouteDownload;
use ctx_route_contracts::merge_queue::{
    MergeQueueEntryRouteParams, MergeQueueLogDownloadRouteError,
    MergeQueueLogDownloadRouteErrorKind,
};

pub(in crate::api) async fn get_merge_queue_entry_logs(
    State(state): State<MergeQueueApiHandle>,
    Path(params): Path<MergeQueueEntryRouteParams>,
) -> Result<Response, StatusCode> {
    let download = state
        .download_merge_queue_entry_logs_for_route_params(params)
        .await
        .map_err(merge_queue_log_download_status)?;
    Ok(text_download_response(download))
}

pub(in crate::api::merge_queue_api) fn text_download_response(
    download: TextRouteDownload,
) -> Response {
    let mut resp = Response::new(Body::from(download.bytes));
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    resp.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        header::HeaderValue::from_str(&format!("attachment; filename=\"{}\"", download.filename))
            .unwrap_or_else(|_| header::HeaderValue::from_static("attachment")),
    );
    resp
}

pub(in crate::api::merge_queue_api) fn merge_queue_log_download_status(
    error: MergeQueueLogDownloadRouteError,
) -> StatusCode {
    match error.kind() {
        MergeQueueLogDownloadRouteErrorKind::BadRequest => StatusCode::BAD_REQUEST,
        MergeQueueLogDownloadRouteErrorKind::NotFound => StatusCode::NOT_FOUND,
        MergeQueueLogDownloadRouteErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
