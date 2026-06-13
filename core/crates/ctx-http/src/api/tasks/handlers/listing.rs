use super::super::*;

pub(in crate::api) async fn list_workspace_tasks(
    State(tasks): State<TaskListingHandle>,
    Path(id): Path<String>,
) -> Result<Json<Vec<TaskRouteResponse>>, StatusCode> {
    let tasks = tasks
        .list_workspace_tasks_for_route(ListWorkspaceTasksRouteParams::new(id))
        .await
        .map_err(|error| task_route_status(&error))?;
    Ok(Json(tasks))
}

pub(in crate::api) async fn list_workspace_archived_task_summaries(
    State(tasks): State<TaskListingHandle>,
    Path(id): Path<String>,
    Query(query): Query<ListWorkspaceArchivedTasksRouteRequest>,
) -> Result<Json<WorkspaceArchivedPageRouteResponse>, StatusCode> {
    let page = tasks
        .list_workspace_archived_page_for_route(ListWorkspaceArchivedTasksRouteParams::new(
            id, query,
        ))
        .await
        .map_err(|error| task_route_status(&error))?;
    Ok(Json(page))
}

pub(in crate::api) async fn list_task_sessions(
    State(tasks): State<TaskSessionListingHandle>,
    Path(id): Path<String>,
) -> Result<Json<Vec<SessionRouteResponse>>, StatusCode> {
    let sessions = tasks
        .list_task_sessions_for_route(TaskRouteParams::new(id))
        .await
        .map_err(|error| task_route_status(&error))?;
    Ok(Json(sessions))
}
