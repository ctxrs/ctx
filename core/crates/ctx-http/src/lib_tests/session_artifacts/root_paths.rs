use super::*;

#[tokio::test]
async fn session_artifacts_reject_outside_root_paths_and_fail_closed_for_legacy_rows() {
    let fixture = build_session_artifact_fixture().await;
    let outside_dir = tempfile::tempdir().unwrap();
    let outside_path = outside_dir.path().join("outside.txt");
    std::fs::write(&outside_path, b"outside-body\n").unwrap();

    let res = post_session_artifacts(
        &fixture.app,
        fixture.session.id,
        json!([{
            "absolute_file_path": outside_path.to_string_lossy(),
            "name": "outside.txt",
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

    let legacy = fixture
        .daemon()
        .seed_legacy_session_artifact_by_path_for_test(
            &fixture.session,
            &outside_path,
            "outside.txt",
            "text/plain",
            13,
        )
        .await
        .unwrap();

    let session_state = get_session_state(&fixture.app, fixture.session.id).await;
    assert_eq!(
        session_state["artifacts"][0]["missing"].as_bool(),
        Some(true)
    );

    let res = get_session_artifact(&fixture.app, fixture.session.id, legacy.id).await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
