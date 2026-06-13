use super::*;

#[tokio::test]
async fn post_message_fails_turn_start_when_agent_server_config_is_invalid() {
    let _serial = home_env_test_lock().lock().await;
    let git_repo = setup_git_repo().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());
    let data_dir = tempfile::tempdir().unwrap();
    let (fixture, app, session) =
        build_fake_app_with_session(data_dir.path(), &git_repo.path().to_string_lossy()).await;
    let daemon = fixture.daemon();
    write_invalid_agent_server_config(daemon.data_root());

    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/sessions/{}/messages", session.id.0))
        .header("content-type", "application/json")
        .body(Body::from(json!({"content":"hello"}).to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    daemon
        .wait_for_session_turn_failed_for_test(session.id, Duration::from_secs(10))
        .await
        .expect("turn did not fail when agent server config was invalid");
}

#[tokio::test]
async fn post_message_fails_turn_start_when_workspace_runtime_settings_are_invalid() {
    let _serial = home_env_test_lock().lock().await;
    let git_repo = setup_git_repo().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());
    let data_dir = tempfile::tempdir().unwrap();
    let (fixture, app, session) =
        build_fake_app_with_session(data_dir.path(), &git_repo.path().to_string_lossy()).await;
    let daemon = fixture.daemon();
    daemon
        .seed_invalid_workspace_runtime_settings_for_test(session.id, "{ not valid json")
        .await
        .unwrap();

    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/sessions/{}/messages", session.id.0))
        .header("content-type", "application/json")
        .body(Body::from(json!({"content":"hello"}).to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    daemon
        .wait_for_session_turn_failed_for_test(session.id, Duration::from_secs(10))
        .await
        .expect("turn did not fail when workspace runtime settings were invalid");
}
