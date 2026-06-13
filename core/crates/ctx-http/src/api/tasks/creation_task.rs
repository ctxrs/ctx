use super::*;

type CreateTaskApiError = (StatusCode, Json<ApiErrorResp>);

pub(in crate::api) async fn create_task(
    State(tasks): State<TaskCreationHandle>,
    Path(id): Path<String>,
    Json(req): Json<CreateTaskRouteRequest>,
) -> Result<Json<TaskRouteResponse>, CreateTaskApiError> {
    let task = tasks
        .create_task_for_route(&id, req)
        .await
        .map_err(task_route_api_error)?;
    Ok(Json(task))
}
