use super::super::*;

pub(in crate::api) async fn unarchive_task(
    State(tasks): State<TaskLifecycleHandle>,
    Path(id): Path<String>,
) -> Result<Json<TaskRouteResponse>, StatusCode> {
    let task = tasks
        .unarchive_task_for_route(TaskRouteParams::new(id))
        .await
        .map_err(|error| task_route_status(&error))?;
    Ok(Json(task))
}
