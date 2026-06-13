use super::super::*;

pub(in crate::api) async fn mark_task_read(
    State(tasks): State<TaskReadStateHandle>,
    Path(id): Path<String>,
) -> Result<Json<TaskRouteResponse>, StatusCode> {
    let task = tasks
        .mark_task_read_for_route(TaskRouteParams::new(id))
        .await
        .map_err(|error| task_route_status(&error))?;
    Ok(Json(task))
}

pub(in crate::api) async fn mark_task_unread(
    State(tasks): State<TaskReadStateHandle>,
    Path(id): Path<String>,
) -> Result<Json<TaskRouteResponse>, StatusCode> {
    let task = tasks
        .mark_task_unread_for_route(TaskRouteParams::new(id))
        .await
        .map_err(|error| task_route_status(&error))?;
    Ok(Json(task))
}
