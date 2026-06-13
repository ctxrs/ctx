use super::*;

#[tokio::test]
async fn desktop_browser_query_secret_authorizes_core_desktop_mutations() {
    let fixture = AuthBoundaryFixture::new().await;
    let git_repo = setup_git_repo().await;

    let app = fixture.app();
    let browser_secret = derive_browser_query_secret("daemon-secret");
    let workspace_id = "11111111-1111-1111-1111-111111111111";

    let cases = [
        (
            "POST",
            "/api/workspaces".to_string(),
            json!({
                "root_path": git_repo.path().to_string_lossy(),
                "name": "desktop-browser-secret-workspace"
            }),
        ),
        (
            "POST",
            format!("/api/workspaces/{workspace_id}/tasks"),
            json!({"title": "task"}),
        ),
        (
            "POST",
            format!("/api/workspaces/{workspace_id}/terminals"),
            json!({}),
        ),
        (
            "PUT",
            "/api/providers/codex/active-account".to_string(),
            json!({"account_id": "default"}),
        ),
        (
            "DELETE",
            "/api/providers/codex/accounts/11111111-1111-1111-1111-111111111111".to_string(),
            json!({}),
        ),
        (
            "DELETE",
            format!("/api/workspaces/{workspace_id}"),
            json!({}),
        ),
        (
            "DELETE",
            "/api/tasks/11111111-1111-1111-1111-111111111111".to_string(),
            json!({}),
        ),
    ];

    for (method, uri, body) in cases {
        let req = Request::builder()
            .method(method)
            .uri(uri.as_str())
            .header("authorization", format!("Bearer {browser_secret}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_ne!(res.status(), StatusCode::UNAUTHORIZED, "{method} {uri}");
    }
}
