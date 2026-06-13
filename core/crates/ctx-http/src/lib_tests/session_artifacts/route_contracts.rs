use super::*;

#[tokio::test]
async fn session_artifact_routes_reject_invalid_route_ids_before_store_lookup() {
    let fixture = build_session_artifact_fixture().await;

    let res = list_session_artifacts_raw(&fixture.app, "not-a-session").await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let res = post_session_artifacts_raw(&fixture.app, "not-a-session", json!([])).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
        json!({"error": "invalid session id"})
    );

    let res = get_session_artifact_raw(
        &fixture.app,
        "not-a-session",
        &ctx_core::ids::ArtifactId::new().0.to_string(),
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let res = get_session_artifact_raw(
        &fixture.app,
        &fixture.session.id.0.to_string(),
        "not-an-artifact",
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}
