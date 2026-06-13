use super::*;

#[tokio::test]
async fn post_message_route_rejects_queueing_when_feature_flag_is_disabled() {
    let _serial = home_env_test_lock().lock().await;
    let _queueing = EnvVarGuard::set("CTX_QUEUED_MESSAGES_ENABLED", "0");
    let git_repo = setup_git_repo().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());
    let data_dir = tempfile::tempdir().unwrap();
    let (fixture, app, session) =
        build_fake_app_with_session(data_dir.path(), &git_repo.path().to_string_lossy()).await;
    let daemon = fixture.daemon();

    daemon.set_session_running(session.id, true).await;
    let (status, body) = post_session_message_json(
        &app,
        session.id,
        json!({ "content": "implicit delivery while running" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        body.get("error").and_then(|value| value.as_str()),
        Some("A turn is already running. Stop it or wait for it to finish.")
    );

    daemon.set_session_running(session.id, true).await;
    let (status, body) = post_session_message_json(
        &app,
        session.id,
        json!({ "content": "explicit queued delivery", "delivery": "queued" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body.get("error").and_then(|value| value.as_str()),
        Some("Queued messages are disabled.")
    );

    assert!(
        daemon
            .session_has_no_persisted_messages_for_test(session.id)
            .await
            .unwrap(),
        "rejected queue attempts must not persist messages"
    );
}

#[tokio::test]
async fn post_message_route_allows_queueing_when_feature_flag_is_enabled() {
    let _serial = home_env_test_lock().lock().await;
    let _queueing = EnvVarGuard::set("CTX_QUEUED_MESSAGES_ENABLED", "1");
    let git_repo = setup_git_repo().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());
    let data_dir = tempfile::tempdir().unwrap();
    let (fixture, app, session) =
        build_fake_app_with_session(data_dir.path(), &git_repo.path().to_string_lossy()).await;
    let daemon = fixture.daemon();

    daemon.set_session_running(session.id, true).await;
    let (status, body) = post_session_message_json(
        &app,
        session.id,
        json!({ "content": "implicit queued delivery" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.get("delivery").and_then(|value| value.as_str()),
        Some("queued")
    );

    daemon.set_session_running(session.id, true).await;
    let (status, body) = post_session_message_json(
        &app,
        session.id,
        json!({ "content": "explicit queued delivery", "delivery": "queued" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.get("delivery").and_then(|value| value.as_str()),
        Some("queued")
    );
}
