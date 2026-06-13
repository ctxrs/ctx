use serde::de::DeserializeOwned;

use super::*;

#[tokio::test]
async fn large_session_head_http_responses_are_bounded() {
    const SEEDED_TURNS: i64 = 11;
    const HEAD_LIMIT: i64 = 6;
    let step_timeout = std::time::Duration::from_secs(120);
    let _serial = home_env_test_lock().lock().await;

    let repo = setup_git_repo().await;
    let _projection_flush_ms = EnvVarGuard::set("CTX_ACTIVE_HEAD_PROJECTION_FLUSH_MS", "600000");
    let data_dir = tempfile::tempdir().unwrap();
    let fixture = test_daemon_fixture_with_fake_provider_for_test(data_dir.path(), None).await;
    let daemon = fixture.daemon();
    let app = fixture.router();

    let workspace = create_workspace_via_api(&app, &repo.path().to_string_lossy()).await;
    let (task_status, task): (StatusCode, ctx_core::models::Task) = json_request(
        &app,
        Method::POST,
        format!("/api/workspaces/{}/tasks", workspace.id.0),
        Some(json!({
            "title": "Large Head",
            "default_session": {
                "provider_id": "fake",
                "model_id": "fake-model"
            }
        })),
    )
    .await;
    assert_eq!(task_status, StatusCode::OK);
    let (session_status, sessions): (StatusCode, Vec<ctx_core::models::Session>) = json_request(
        &app,
        Method::GET,
        format!("/api/tasks/{}/sessions", task.id.0),
        None,
    )
    .await;
    assert_eq!(session_status, StatusCode::OK);
    let session = sessions
        .into_iter()
        .find(|session| Some(session.id) == task.primary_session_id)
        .expect("created task should list its default session");

    daemon
        .seed_large_session_head_fixture_for_test(
            workspace.id,
            session.id,
            task.id,
            SEEDED_TURNS,
            step_timeout,
        )
        .await
        .unwrap();

    let (heads_status, heads_body): (StatusCode, serde_json::Value) = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        json_request(
            &app,
            Method::GET,
            format!("/api/workspaces/{}/active_heads", workspace.id.0),
            None,
        ),
    )
    .await
    .unwrap_or_else(|_| panic!("timed out requesting workspace active heads"));
    assert_eq!(heads_status, StatusCode::OK, "{heads_body:#?}");
    let active_head = heads_body["heads"]
        .as_array()
        .expect("workspace active heads array")
        .iter()
        .find(|item| item["session"]["id"] == json!(session.id.0.to_string()))
        .expect("seeded session head present in workspace active heads");
    assert_eq!(
        active_head["turns"].as_array().map(Vec::len),
        Some(5),
        "workspace active heads should use the compact active-head window"
    );
    assert_eq!(active_head["has_more_turns"], json!(true));

    let (head_status, head_body): (StatusCode, serde_json::Value) = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        json_request(
            &app,
            Method::GET,
            format!(
                "/api/sessions/{}/head?limit={HEAD_LIMIT}&include_events=true",
                session.id.0,
            ),
            None,
        ),
    )
    .await
    .unwrap_or_else(|_| panic!("timed out requesting session head"));
    assert_eq!(head_status, StatusCode::OK, "{head_body:#?}");
    let head_limit = usize::try_from(HEAD_LIMIT).expect("head limit should fit usize");
    assert_eq!(
        head_body["turns"].as_array().map(Vec::len),
        Some(head_limit)
    );
    assert_eq!(
        head_body["messages"].as_array().map(Vec::len),
        Some(head_limit)
    );
    assert_eq!(
        head_body["tool_summaries"].as_array().map(Vec::len),
        Some(head_limit)
    );
    assert_eq!(head_body["has_more_turns"], json!(true));
    assert_eq!(
        head_body["messages"][0]["content"],
        json!(format!("answer {}", SEEDED_TURNS - HEAD_LIMIT))
    );
    assert_eq!(
        head_body["messages"][head_limit - 1]["content"],
        json!(format!("answer {}", SEEDED_TURNS - 1))
    );
}

async fn json_request<T: DeserializeOwned>(
    app: &axum::Router,
    method: Method,
    uri: impl Into<String>,
    body: Option<serde_json::Value>,
) -> (StatusCode, T) {
    let req = Request::builder()
        .method(method)
        .uri(uri.into())
        .header("content-type", "application/json")
        .body(Body::from(
            body.unwrap_or(serde_json::Value::Null).to_string(),
        ))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let parsed = serde_json::from_slice(&body).unwrap_or_else(|err| {
        panic!(
            "failed to parse JSON response (status {}): {}\nbody: {}",
            status,
            err,
            String::from_utf8_lossy(&body)
        )
    });
    (status, parsed)
}
