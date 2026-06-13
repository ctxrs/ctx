use super::*;

pub(in crate::api) async fn create_workspace_attachment(
    State(attachments): State<WorkspaceAttachmentsHandle>,
    Path(id): Path<String>,
    Json(req): Json<CreateWorkspaceAttachmentRouteRequest>,
) -> Result<Json<Vec<WorkspaceAttachmentRouteResponse>>, (StatusCode, Json<ApiErrorResp>)> {
    attachments
        .create_and_sync_workspace_attachment_for_route_params(WorkspaceRouteParams::new(id), req)
        .await
        .map_err(workspace_route_api_error)
        .map(Json)
}

pub(in crate::api) async fn delete_workspace_attachment(
    State(attachments): State<WorkspaceAttachmentsHandle>,
    Path(id): Path<String>,
    Json(req): Json<DeleteWorkspaceAttachmentRouteRequest>,
) -> Result<Json<Vec<WorkspaceAttachmentRouteResponse>>, (StatusCode, Json<ApiErrorResp>)> {
    attachments
        .delete_and_sync_workspace_attachment_for_route_params(WorkspaceRouteParams::new(id), req)
        .await
        .map_err(workspace_route_api_error)
        .map(Json)
}
