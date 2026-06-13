mod common;

use axum::body::Body;
use axum::http::Request;
use axum::http::{Method, StatusCode};
use ctx_core::ids::WorkspaceId;
use ctx_settings_model::{ExecutionMode, ExecutionSettings};
use ctx_workspace_config::{ExecutionConfigUpdate, ExecutionEnvironment};
use serde_json::Value;

async fn raw_json_body_request(
    app: &axum::Router,
    method: Method,
    uri: impl Into<String>,
    body: impl Into<String>,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri.into())
        .header("content-type", "application/json")
        .body(Body::from(body.into()))
        .unwrap();
    common::oneshot_json(app, req).await
}

#[tokio::test]
async fn workspace_registration_rejects_invalid_root_path() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();

    let (status, body): (StatusCode, Value) = common::json_request(
        &app,
        Method::POST,
        "/api/workspaces",
        Some(serde_json::json!({"root_path": " ", "name": "ws"})),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body:#?}");
    assert!(
        body.get("error")
            .and_then(Value::as_str)
            .is_some_and(|message| message.contains("root_path is required")),
        "invalid workspace root should surface registration guidance: {body:#?}"
    );
}

#[tokio::test]
async fn workspace_execution_config_fails_closed_on_invalid_runtime_settings() {
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let daemon = &fixture.daemon;
    let app = fixture.router();
    let workspace = common::create_workspace(&app, repo.path(), "ws").await;

    daemon
        .seed_invalid_workspace_runtime_settings_document_for_test(
            workspace.id,
            r#"{
  "execution": {
    "environment": 7
  }
}"#,
        )
        .await
        .expect("write malformed runtime settings");

    let (status, body): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        format!("/api/workspaces/{}/execution_config", workspace.id.0),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body:#?}");
    assert!(
        body.get("error")
            .and_then(Value::as_str)
            .is_some_and(|message| message.contains("workspace runtime settings")),
        "invalid workspace runtime settings should be surfaced instead of falling back: {body:#?}"
    );
}

#[tokio::test]
async fn workspace_execution_config_maps_persisted_policy_denial_to_forbidden() {
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let daemon = &fixture.daemon;
    daemon
        .save_execution_settings_for_test(ExecutionSettings {
            mode: ExecutionMode::Sandbox,
            ..ExecutionSettings::default()
        })
        .await
        .expect("save daemon execution settings");
    let app = fixture.router();
    let workspace = common::create_workspace(&app, repo.path(), "ws").await;
    daemon
        .seed_workspace_execution_config_for_test(
            workspace.id,
            ExecutionConfigUpdate {
                environment: ExecutionEnvironment::Host,
                network_mode: None,
                allowlist: None,
                image: None,
            },
        )
        .await
        .expect("write persisted host override");

    let (status, body): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        format!("/api/workspaces/{}/execution_config", workspace.id.0),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body:#?}");
    assert!(
        body.get("error")
            .and_then(Value::as_str)
            .is_some_and(|message| message.contains("cannot select host")),
        "policy denial should be surfaced: {body:#?}"
    );
}

#[tokio::test]
async fn workspace_execution_config_preserves_invalid_id_error_contract() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();

    let (get_status, get_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        "/api/workspaces/not-a-workspace/execution_config",
        None,
    )
    .await;
    assert_eq!(get_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        get_body.get("error").and_then(Value::as_str),
        Some("invalid workspace id")
    );

    let (post_status, post_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::POST,
        "/api/workspaces/not-a-workspace/execution_config",
        Some(serde_json::json!({
            "environment": "host"
        })),
    )
    .await;
    assert_eq!(post_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        post_body.get("error").and_then(Value::as_str),
        Some("invalid workspace id")
    );
}

#[tokio::test]
async fn workspace_execution_config_preserves_missing_workspace_precedence() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();
    let missing_workspace_id = WorkspaceId::new();

    let (get_status, get_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        format!(
            "/api/workspaces/{}/execution_config",
            missing_workspace_id.0
        ),
        None,
    )
    .await;
    assert_eq!(get_status, StatusCode::NOT_FOUND);
    assert_eq!(
        get_body.get("error").and_then(Value::as_str),
        Some("workspace not found")
    );

    let (post_status, post_body): (StatusCode, Value) = raw_json_body_request(
        &app,
        Method::POST,
        format!(
            "/api/workspaces/{}/execution_config",
            missing_workspace_id.0
        ),
        "{",
    )
    .await;
    assert_eq!(post_status, StatusCode::NOT_FOUND);
    assert_eq!(
        post_body.get("error").and_then(Value::as_str),
        Some("workspace not found")
    );
}

#[tokio::test]
async fn workspace_execution_config_treats_deleting_workspace_as_not_found() {
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();
    let workspace = common::create_workspace(&app, repo.path(), "ws").await;
    fixture
        .daemon
        .cache_rehydration_begin_workspace_delete_for_test(workspace.id)
        .await;

    let (get_status, get_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        format!("/api/workspaces/{}/execution_config", workspace.id.0),
        None,
    )
    .await;
    assert_eq!(get_status, StatusCode::NOT_FOUND);
    assert_eq!(
        get_body.get("error").and_then(Value::as_str),
        Some("workspace not found")
    );

    let (post_status, post_body): (StatusCode, Value) = raw_json_body_request(
        &app,
        Method::POST,
        format!("/api/workspaces/{}/execution_config", workspace.id.0),
        "{",
    )
    .await;
    assert_eq!(post_status, StatusCode::NOT_FOUND);
    assert_eq!(
        post_body.get("error").and_then(Value::as_str),
        Some("workspace not found")
    );
    fixture
        .daemon
        .cache_rehydration_finish_workspace_delete_for_test(workspace.id)
        .await;
}

#[tokio::test]
async fn workspace_execution_config_maps_unavailable_store_to_internal_error() {
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();
    let workspace = common::create_workspace(&app, repo.path(), "ws").await;
    fixture
        .daemon
        .cache_rehydration_make_workspace_store_unopenable_for_test(workspace.id)
        .await
        .expect("block workspace store");

    let (get_status, _get_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        format!("/api/workspaces/{}/execution_config", workspace.id.0),
        None,
    )
    .await;
    assert_eq!(get_status, StatusCode::INTERNAL_SERVER_ERROR);

    let (post_status, _post_body): (StatusCode, Value) = raw_json_body_request(
        &app,
        Method::POST,
        format!("/api/workspaces/{}/execution_config", workspace.id.0),
        "{",
    )
    .await;
    assert_eq!(post_status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn workspace_execution_config_rejects_invalid_request_values() {
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();
    let workspace = common::create_workspace(&app, repo.path(), "ws").await;

    let (status, body): (StatusCode, Value) = common::json_request(
        &app,
        Method::POST,
        format!("/api/workspaces/{}/execution_config", workspace.id.0),
        Some(serde_json::json!({
            "environment": "container",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body:#?}");
    assert!(
        body.get("error")
            .and_then(Value::as_str)
            .is_some_and(|message| message.contains("invalid environment")),
        "invalid environment should be rejected: {body:#?}"
    );

    let (status, body): (StatusCode, Value) = common::json_request(
        &app,
        Method::POST,
        format!("/api/workspaces/{}/execution_config", workspace.id.0),
        Some(serde_json::json!({
            "environment": "host",
            "network_mode": "public",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body:#?}");
    assert!(
        body.get("error")
            .and_then(Value::as_str)
            .is_some_and(|message| message.contains("invalid network_mode")),
        "invalid network mode should be rejected: {body:#?}"
    );
}

#[tokio::test]
async fn workspace_execution_config_maps_update_policy_denial_to_forbidden() {
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    fixture
        .daemon
        .save_execution_settings_for_test(ExecutionSettings {
            mode: ExecutionMode::Sandbox,
            ..ExecutionSettings::default()
        })
        .await
        .expect("save daemon execution settings");
    let app = fixture.router();
    let workspace = common::create_workspace(&app, repo.path(), "ws").await;

    let (status, body): (StatusCode, Value) = common::json_request(
        &app,
        Method::POST,
        format!("/api/workspaces/{}/execution_config", workspace.id.0),
        Some(serde_json::json!({
            "environment": "host",
        })),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body:#?}");
    assert!(
        body.get("error")
            .and_then(Value::as_str)
            .is_some_and(|message| message.contains("cannot select host")),
        "policy denial should be surfaced: {body:#?}"
    );
}
