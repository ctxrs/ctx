use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;

use ctx_daemon::daemon::SessionArtifactsHandle;
use ctx_session_artifacts::route_contract::SessionArtifactDownloadRouteParams;

#[path = "download/response.rs"]
mod response;

pub(in crate::api) async fn get_session_artifact(
    State(state): State<SessionArtifactsHandle>,
    Path((session_id, artifact_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let download = state
        .open_session_artifact_for_route_params(SessionArtifactDownloadRouteParams::new(
            session_id,
            artifact_id,
        ))
        .await
        .map_err(super::session::session_artifact_status)?;
    response::build_session_artifact_download_response(
        headers,
        download.file,
        response::SessionArtifactDownloadMetadata {
            size: download.size,
            etag: download.etag.as_deref(),
            last_modified: download.last_modified.as_deref(),
            mime_type: &download.mime_type,
            name: download.name.as_deref(),
        },
    )
    .await
}
