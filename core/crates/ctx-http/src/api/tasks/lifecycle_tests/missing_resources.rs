use super::*;

#[tokio::test]
async fn task_mutations_return_not_found_for_unknown_task() {
    let temp = tempfile::tempdir().expect("tempdir");
    let fixture = test_state(temp.path()).await;
    let state = fixture.daemon();
    let missing_task_id = TaskId::new();
    assert_task_mutations_return_not_found(state, missing_task_id).await;
}

#[tokio::test]
async fn task_mutations_return_not_found_for_stale_task_index() {
    let temp = tempfile::tempdir().expect("tempdir");
    let fixture = test_state(temp.path()).await;
    let state = fixture.daemon();
    let stale_task_id = TaskId::new();
    state
        .seed_task_lifecycle_stale_task_index_for_test(stale_task_id, WorkspaceId::new())
        .await
        .expect("seed stale task index");

    assert_task_mutations_return_not_found(state, stale_task_id).await;
}

async fn assert_task_mutations_return_not_found(state: &TestDaemon, missing_task_id: TaskId) {
    let task_sessions = task_api_task_session_listing_state(state);
    let task_read = task_api_task_read_state_state(state);
    let task_title = task_api_task_title_state(state);
    let lifecycle = task_api_lifecycle_state(state);

    let task_sessions_status =
        list_task_sessions(task_sessions.clone(), Path(missing_task_id.0.to_string()))
            .await
            .expect_err("missing task sessions should fail");
    assert_eq!(task_sessions_status, StatusCode::NOT_FOUND);

    let read_status = mark_task_read(task_read.clone(), Path(missing_task_id.0.to_string()))
        .await
        .expect_err("missing task read should fail");
    assert_eq!(read_status, StatusCode::NOT_FOUND);

    let unread_status = mark_task_unread(task_read.clone(), Path(missing_task_id.0.to_string()))
        .await
        .expect_err("missing task unread should fail");
    assert_eq!(unread_status, StatusCode::NOT_FOUND);

    let title_req: UpdateTaskTitleRouteRequest =
        serde_json::from_value(serde_json::json!({"title": "renamed"})).expect("title request");
    let (title_status, Json(title_body)) = update_task_title(
        task_title.clone(),
        Path(missing_task_id.0.to_string()),
        Json(title_req),
    )
    .await
    .expect_err("missing task title update should fail");
    assert_eq!(title_status, StatusCode::NOT_FOUND);
    assert_eq!(title_body.error, "task not found");

    let archive_status = archive_task(lifecycle.clone(), Path(missing_task_id.0.to_string()))
        .await
        .expect_err("missing task archive should fail");
    assert_eq!(archive_status, StatusCode::NOT_FOUND);

    let unarchive_status = unarchive_task(lifecycle, Path(missing_task_id.0.to_string()))
        .await
        .expect_err("missing task unarchive should fail");
    assert_eq!(unarchive_status, StatusCode::NOT_FOUND);
}
