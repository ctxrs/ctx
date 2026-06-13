use super::*;

mod agent_server_config_errors;
mod golden_path;
mod messages;
mod session_validation;
mod streaming;
fn write_invalid_agent_server_config(data_root: &std::path::Path) {
    let path = data_root
        .join("providers")
        .join("agent-servers")
        .join("agent_servers.json");
    std::fs::create_dir_all(path.parent().expect("agent server config parent")).unwrap();
    std::fs::write(path, "{ not valid json").unwrap();
}

fn fake_default_session_payload() -> serde_json::Value {
    json!({
        "provider_id": "fake",
        "model_id": "fake-model",
        "execution_environment": "host",
    })
}

async fn create_fake_session_via_api(
    app: &axum::Router,
    git_repo_path: &str,
) -> ctx_core::models::Session {
    let req = Request::builder()
        .method("POST")
        .uri("/api/workspaces")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "root_path": git_repo_path,
                "name": "ws"
            })
            .to_string(),
        ))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let ws: ctx_core::models::Workspace = serde_json::from_slice(&body).unwrap();

    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/workspaces/{}/tasks", ws.id.0))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "title": "t1",
                "description": null,
                "default_session": fake_default_session_payload(),
            })
            .to_string(),
        ))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let task: ctx_core::models::Task = serde_json::from_slice(&body).unwrap();

    load_primary_session_via_api(app, &task).await
}

async fn build_fake_app_with_session(
    data_dir: &Path,
    git_repo_path: &str,
) -> (
    DataRootTestDaemonFixture,
    axum::Router,
    ctx_core::models::Session,
) {
    let fixture = test_daemon_fixture_with_fake_provider_for_test(data_dir, None).await;
    let app = fixture.router();
    let session = create_fake_session_via_api(&app, git_repo_path).await;
    (fixture, app, session)
}

async fn post_session_message_json(
    app: &axum::Router,
    session_id: ctx_core::ids::SessionId,
    payload: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/sessions/{}/messages", session_id.0))
        .header("content-type", "application/json")
        .body(Body::from(payload.to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let body = if body.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&body).unwrap()
    };
    (status, body)
}
