use super::*;

pub(in crate::api) async fn list_workspace_attachments(
    State(attachments): State<WorkspaceAttachmentsHandle>,
    Path(id): Path<String>,
) -> Result<Json<Vec<WorkspaceAttachmentRouteResponse>>, StatusCode> {
    attachments
        .list_workspace_attachments_for_route_params(WorkspaceRouteParams::new(id))
        .await
        .map(Json)
        .map_err(|error| workspace_route_status(&error))
}

pub(in crate::api) async fn sync_workspace_attachments(
    State(attachments): State<WorkspaceAttachmentsHandle>,
    Path(id): Path<String>,
    Json(req): Json<SyncWorkspaceAttachmentsRouteRequest>,
) -> Result<Json<Vec<WorkspaceAttachmentRouteResponse>>, (StatusCode, Json<ApiErrorResp>)> {
    attachments
        .sync_workspace_attachments_for_route_params(WorkspaceRouteParams::new(id), req)
        .await
        .map(Json)
        .map_err(workspace_route_api_error)
}
