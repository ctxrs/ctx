use super::*;

async fn create_session_for_task_inner(
    tasks: &TaskSessionAdmissionHandle,
    task_id: String,
    headers: HeaderMap,
    req: CreateTaskSessionRouteRequest,
) -> Result<Json<SessionRouteResponse>, StatusCode> {
    let run_id_header = headers
        .get("x-ctx-run-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let session = tasks
        .create_session_for_task_route(TaskRouteParams::new(task_id), req, run_id_header)
        .await
        .map_err(|error| task_route_status(&error))?;
    Ok(Json(session))
}

pub(in crate::api) async fn create_session_for_task(
    State(tasks): State<TaskSessionAdmissionHandle>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<CreateTaskSessionRouteRequest>,
) -> Result<Json<SessionRouteResponse>, StatusCode> {
    create_session_for_task_inner(&tasks, id, headers, req).await
}
