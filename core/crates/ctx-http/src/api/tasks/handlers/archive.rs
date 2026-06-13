use super::super::*;

pub(in crate::api) async fn archive_task(
    State(tasks): State<TaskLifecycleHandle>,
    Path(id): Path<String>,
) -> Result<Json<ArchiveTaskRouteResponse>, StatusCode> {
    let outcome = tasks
        .archive_task_for_route(TaskRouteParams::new(id))
        .await
        .map_err(|error| task_route_status(&error))?;
    Ok(Json(outcome))
}
