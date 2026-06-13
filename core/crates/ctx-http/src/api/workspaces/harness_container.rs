use super::*;

pub(in crate::api) async fn get_workspace_harness_container(
    State(harness_container): State<WorkspaceHarnessContainerHandle>,
    Path(id): Path<String>,
) -> Result<Json<Option<WorkspaceHarnessContainerStatusRouteResponse>>, StatusCode> {
    let status = harness_container
        .workspace_harness_container_status_for_route_params(WorkspaceRouteParams::new(id))
        .await
        .map_err(|error| workspace_route_status(&error))?;
    Ok(Json(status))
}

pub(in crate::api) async fn stop_workspace_harness_container(
    State(harness_container): State<WorkspaceHarnessContainerHandle>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    harness_container
        .stop_workspace_harness_container_for_route(WorkspaceRouteParams::new(id))
        .await
        .map_err(|error| workspace_route_status(&error))?;
    Ok(StatusCode::NO_CONTENT)
}

pub(in crate::api) async fn ensure_workspace_harness_container(
    State(harness_container): State<WorkspaceHarnessContainerHandle>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ApiErrorResp>)> {
    harness_container
        .ensure_workspace_harness_container_for_route(WorkspaceRouteParams::new(id))
        .await
        .map_err(workspace_route_api_error)?;

    Ok(StatusCode::NO_CONTENT)
}
