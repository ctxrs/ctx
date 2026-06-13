use super::*;

#[tokio::test]
async fn session_artifacts_do_not_accept_other_session_spool_files() {
    let fixture = build_session_artifact_fixture().await;
    let other_session =
        create_subagent_session_via_api(&fixture.app, &fixture.task, fixture.session.id).await;

    let other_spool_dir = fixture
        .daemon()
        .tool_output_spool_dir()
        .join(other_session.id.0.to_string())
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&other_spool_dir).unwrap();
    let other_spool_path = other_spool_dir.join("foreign.txt");
    std::fs::write(&other_spool_path, b"other-session-spool\n").unwrap();

    let legacy = fixture
        .daemon()
        .seed_legacy_session_artifact_by_path_for_test(
            &fixture.session,
            &other_spool_path,
            "foreign.txt",
            "text/plain",
            20,
        )
        .await
        .unwrap();

    let res = post_session_artifacts(
        &fixture.app,
        fixture.session.id,
        json!([{
            "absolute_file_path": other_spool_path.to_string_lossy(),
            "name": "foreign.txt",
            "mime_type": "text/plain"
        }]),
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        payload["error"].as_str(),
        Some("artifact 1 absolute_file_path must stay inside the session worktree or tool-output spool")
    );

    let session_state = get_session_state(&fixture.app, fixture.session.id).await;
    assert_eq!(
        session_state["artifacts"][0]["missing"].as_bool(),
        Some(true)
    );

    let res = get_session_artifact(&fixture.app, fixture.session.id, legacy.id).await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
