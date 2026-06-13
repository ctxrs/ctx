use super::*;

pub(in crate::api) async fn delete_task(
    State(tasks): State<TaskLifecycleHandle>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    tasks
        .delete_task_for_route(TaskRouteParams::new(id))
        .await
        .map_err(|error| task_route_status(&error))?;
    Ok(StatusCode::NO_CONTENT)
}
