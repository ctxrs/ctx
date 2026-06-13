use super::*;

pub(in crate::api) async fn update_task_title(
    State(tasks): State<TaskTitleHandle>,
    Path(id): Path<String>,
    Json(req): Json<UpdateTaskTitleRouteRequest>,
) -> Result<Json<TaskRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let task = tasks
        .update_task_title_for_route(TaskRouteParams::new(id), req)
        .await
        .map_err(task_route_api_error)?;
    Ok(Json(task))
}
