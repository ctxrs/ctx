use axum::http::HeaderMap;
use ctx_core::ids::WorkRecordId;
use ctx_daemon::daemon::SessionArtifactsHandle;
use ctx_session_artifacts::route_contract::SessionArtifactRouteError;

use super::*;

pub(in crate::api) async fn list_workspace_work(
    State(workspaces): State<WorkspaceWorkHandle>,
    Path(id): Path<String>,
    Query(query): Query<WorkspaceWorkListRouteQuery>,
) -> Result<Json<WorkspaceWorkListRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    workspaces
        .list_workspace_work_for_route(WorkspaceRouteParams::new(id), query)
        .await
        .map(Json)
        .map_err(workspace_route_api_error)
}

pub(in crate::api) async fn get_workspace_work(
    State(workspaces): State<WorkspaceWorkHandle>,
    Path((id, work_id)): Path<(String, String)>,
) -> Result<Json<WorkspaceWorkDetailRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let work_id = normalize_work_id(work_id);
    workspaces
        .get_workspace_work_for_route(WorkspaceRouteParams::new(id), work_id.0)
        .await
        .map(Json)
        .map_err(workspace_route_api_error)
}

pub(in crate::api) async fn get_workspace_work_report(
    State(workspaces): State<WorkspaceWorkHandle>,
    Path((id, work_id)): Path<(String, String)>,
) -> Result<Json<WorkspaceWorkReportRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let work_id = normalize_work_id(work_id);
    workspaces
        .get_workspace_work_report_for_route(WorkspaceRouteParams::new(id), work_id.0)
        .await
        .map(Json)
        .map_err(workspace_route_api_error)
}

pub(in crate::api) async fn get_workspace_work_inspector(
    State(workspaces): State<WorkspaceWorkHandle>,
    Path((id, work_id)): Path<(String, String)>,
) -> Result<Json<WorkspaceWorkInspectorRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let work_id = normalize_work_id(work_id);
    workspaces
        .get_workspace_work_inspector_for_route(WorkspaceRouteParams::new(id), work_id.0)
        .await
        .map(Json)
        .map_err(workspace_route_api_error)
}

pub(in crate::api) async fn get_workspace_work_artifact(
    State(workspaces): State<WorkspaceWorkHandle>,
    State(session_artifacts): State<SessionArtifactsHandle>,
    Path((id, work_id, artifact_id)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let work_id = normalize_work_id(work_id);
    let target = workspaces
        .resolve_workspace_work_artifact_for_route(
            WorkspaceRouteParams::new(id),
            work_id.0,
            artifact_id,
        )
        .await
        .map_err(|error| workspace_route_status(&error))?;
    let download = session_artifacts
        .open_session_artifact_for_route(target.session_id, target.artifact_id)
        .await
        .map_err(session_artifact_status)?;
    let inline_preview_safe = work_artifact_mime_is_inline_preview_safe(&target.mime_type);
    let safe_mime_type = if inline_preview_safe {
        target.mime_type.as_str()
    } else {
        "application/octet-stream"
    };
    let mut response =
        crate::api::artifacts::download_response::build_session_artifact_download_response(
            headers,
            download.file,
            crate::api::artifacts::download_response::SessionArtifactDownloadMetadata {
                size: download.size,
                etag: download.etag.as_deref(),
                last_modified: download.last_modified.as_deref(),
                mime_type: safe_mime_type,
                name: download.name.as_deref(),
            },
        )
        .await?;
    response.headers_mut().insert(
        header::HeaderName::from_static("x-content-type-options"),
        header::HeaderValue::from_static("nosniff"),
    );
    if !inline_preview_safe {
        let disposition = target
            .name
            .as_deref()
            .or(download.name.as_deref())
            .map(|name| format!("attachment; filename=\"{}\"", name.replace('"', "")))
            .unwrap_or_else(|| "attachment".to_string());
        if let Ok(value) = header::HeaderValue::from_str(&disposition) {
            response
                .headers_mut()
                .insert(header::CONTENT_DISPOSITION, value);
        }
    }
    Ok(response)
}

pub(in crate::api) async fn get_workspace_work_context(
    State(workspaces): State<WorkspaceWorkHandle>,
    Path((id, work_id)): Path<(String, String)>,
    Query(query): Query<WorkspaceWorkContextRouteQuery>,
) -> Result<Json<WorkspaceWorkContextRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let work_id = normalize_work_id(work_id);
    workspaces
        .get_workspace_work_context_for_route(WorkspaceRouteParams::new(id), work_id.0, query)
        .await
        .map(Json)
        .map_err(workspace_route_api_error)
}

pub(in crate::api) async fn get_workspace_work_timeline(
    State(workspaces): State<WorkspaceWorkHandle>,
    Path((id, work_id)): Path<(String, String)>,
    Query(query): Query<WorkspaceWorkTimelineRouteQuery>,
) -> Result<Json<WorkspaceWorkTimelineRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let work_id = normalize_work_id(work_id);
    workspaces
        .get_workspace_work_timeline_for_route(WorkspaceRouteParams::new(id), work_id.0, query)
        .await
        .map(Json)
        .map_err(workspace_route_api_error)
}

pub(in crate::api) async fn get_workspace_work_evidence(
    State(workspaces): State<WorkspaceWorkHandle>,
    Path((id, work_id)): Path<(String, String)>,
) -> Result<Json<WorkspaceWorkEvidenceRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let work_id = normalize_work_id(work_id);
    workspaces
        .get_workspace_work_evidence_for_route(WorkspaceRouteParams::new(id), work_id.0)
        .await
        .map(Json)
        .map_err(workspace_route_api_error)
}

pub(in crate::api) async fn create_workspace_work_evidence(
    State(workspaces): State<WorkspaceWorkHandle>,
    Path((id, work_id)): Path<(String, String)>,
    Json(request): Json<WorkspaceWorkEvidenceCreateRouteRequest>,
) -> Result<Json<WorkspaceWorkEvidenceCreateRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let work_id = normalize_work_id(work_id);
    workspaces
        .create_workspace_work_evidence_for_route(WorkspaceRouteParams::new(id), work_id.0, request)
        .await
        .map(Json)
        .map_err(workspace_route_api_error)
}

pub(in crate::api) async fn create_workspace_work_summary(
    State(workspaces): State<WorkspaceWorkHandle>,
    Path((id, work_id)): Path<(String, String)>,
    Json(request): Json<WorkspaceWorkSummaryCreateRouteRequest>,
) -> Result<Json<WorkspaceWorkSummaryCreateRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let work_id = normalize_work_id(work_id);
    workspaces
        .create_workspace_work_summary_for_route(WorkspaceRouteParams::new(id), work_id.0, request)
        .await
        .map(Json)
        .map_err(workspace_route_api_error)
}

fn normalize_work_id(value: String) -> WorkRecordId {
    WorkRecordId::from_id(value)
}

fn session_artifact_status(error: SessionArtifactRouteError) -> StatusCode {
    match error {
        SessionArtifactRouteError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
        SessionArtifactRouteError::NotFound => StatusCode::NOT_FOUND,
        SessionArtifactRouteError::BadRequest(_) => StatusCode::BAD_REQUEST,
        SessionArtifactRouteError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn work_artifact_mime_is_inline_preview_safe(mime_type: &str) -> bool {
    let essence = mime_type.split(';').next().unwrap_or(mime_type).trim();
    matches!(
        essence.to_ascii_lowercase().as_str(),
        "image/png"
            | "image/jpeg"
            | "image/gif"
            | "image/webp"
            | "text/plain"
            | "text/markdown"
            | "application/json"
            | "text/csv"
    )
}
