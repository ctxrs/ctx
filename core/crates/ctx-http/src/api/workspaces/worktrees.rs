use super::*;

pub(in crate::api) async fn get_worktree(
    State(worktrees): State<WorkspaceWorktreeHandle>,
    Path(id): Path<String>,
) -> Result<Json<WorktreeRouteResponse>, StatusCode> {
    worktrees
        .get_worktree_for_route_params(WorktreeRouteParams::new(id))
        .await
        .map(Json)
        .map_err(|error| workspace_route_status(&error))
}

pub(in crate::api) async fn get_worktree_bootstrap_logs(
    State(worktrees): State<WorkspaceWorktreeHandle>,
    Path(id): Path<String>,
) -> Result<Response, StatusCode> {
    let download = worktrees
        .download_worktree_bootstrap_logs_for_route_params(WorktreeRouteParams::new(id))
        .await
        .map_err(|error| workspace_route_status(&error))?;
    let mut resp = Response::new(Body::from(download.bytes));
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    resp.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        header::HeaderValue::from_str(&format!("attachment; filename=\"{}\"", download.filename))
            .unwrap_or_else(|_| header::HeaderValue::from_static("attachment")),
    );
    Ok(resp)
}
