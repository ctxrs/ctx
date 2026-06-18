use super::*;

pub(in crate::api) async fn get_workspace_agent_work(
    State(workspaces): State<WorkspaceAgentWorkHandle>,
    Path(id): Path<String>,
    Query(query): Query<WorkspaceAgentWorkRouteQuery>,
) -> Result<Json<WorkspaceAgentWorkRouteResponse>, StatusCode> {
    let graph = workspaces
        .list_workspace_agent_work_for_route(WorkspaceRouteParams::new(id), query)
        .await
        .map_err(|error| workspace_route_status(&error))?;
    Ok(Json(graph))
}
