use super::*;

#[tokio::test]
async fn create_session_rejects_unknown_provider_id() {
    let _serial = home_env_test_lock().lock().await;
    let git_repo = setup_git_repo().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());
    let data_dir = tempfile::tempdir().unwrap();
    let (_fixture, app, primary_session) =
        build_fake_app_with_session(data_dir.path(), &git_repo.path().to_string_lossy()).await;
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/tasks/{}/sessions", primary_session.task_id.0))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "provider_id": "not-a-provider",
                "model_id": "fake-model",
                "parent_session_id": primary_session.id.0.to_string(),
                "relationship": "sub_agent"
            })
            .to_string(),
        ))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}
