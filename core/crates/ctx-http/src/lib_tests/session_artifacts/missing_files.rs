use super::*;

#[tokio::test]
async fn session_artifacts_report_deleted_in_root_files_as_missing() {
    let fixture = build_session_artifact_fixture().await;
    let worktree_root = fixture
        .daemon()
        .session_worktree_root_path_for_test(&fixture.session)
        .await
        .unwrap();
    let artifact_path = worktree_root.join("deleted-artifact.txt");
    std::fs::write(&artifact_path, b"hello\n").unwrap();

    let res = post_session_artifacts(
        &fixture.app,
        fixture.session.id,
        json!([{
            "absolute_file_path": artifact_path.to_string_lossy(),
            "name": "deleted-artifact.txt",
            "mime_type": "text/plain"
        }]),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let artifacts: Vec<ctx_core::models::Artifact> = serde_json::from_slice(&body).unwrap();
    let artifact_id = artifacts[0].id;

    std::fs::remove_file(&artifact_path).unwrap();

    let session_state = get_session_state(&fixture.app, fixture.session.id).await;
    assert_eq!(
        session_state["artifacts"][0]["missing"].as_bool(),
        Some(true)
    );

    let res = get_session_artifact(&fixture.app, fixture.session.id, artifact_id).await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
