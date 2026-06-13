mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use ctx_core::ids::WorkspaceId;
use ctx_core::models::{WorkspaceAttachment, WorkspaceAttachmentKind, WorkspaceAttachmentStatus};
use ctx_settings_model::{
    ContainerExecutionSettings, ContainerRuntimeKind, ExecutionMode, ExecutionSettings, Settings,
};
use serde_json::json;

#[tokio::test]
async fn workspace_attachments_are_db_canonical_and_ignore_repo_file() {
    let _sandbox_cli_available = common::TestEnvGuard::set("CTX_TEST_SANDBOX_CLI_AVAILABLE", "0");
    let _sandbox_cli_path =
        common::TestEnvGuard::set("CTX_HARNESS_SANDBOX_CLI_PATH", "/no/such/sandbox-cli");
    let repo = common::init_git_repo(&[("README.md", "hello\n")]).await;
    let cfg_path = repo.path().join(".ctx").join("attachments.toml");
    tokio::fs::create_dir_all(cfg_path.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&cfg_path, "not-valid-toml = [")
        .await
        .unwrap();
    let before = tokio::fs::read_to_string(&cfg_path).await.unwrap();

    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let state = &fixture.daemon;
    state
        .settings_handle_for_test()
        .save_settings(&Settings {
            execution: Some(ExecutionSettings {
                mode: ExecutionMode::Host,
                container: ContainerExecutionSettings {
                    runtime: ContainerRuntimeKind::NativeContainer,
                    ..ContainerExecutionSettings::default()
                },
            }),
            ..Settings::default()
        })
        .await
        .unwrap();
    let app = fixture.router();

    let workspace = common::create_workspace(&app, repo.path(), "ws").await;

    let (create_status, created): (StatusCode, Vec<WorkspaceAttachment>) = common::json_request(
        &app,
        Method::POST,
        format!("/api/workspaces/{}/attachments", workspace.id.0),
        Some(json!({
            "kind": "reference_repo",
            "name": "ref-fixture",
            "source": repo.path().to_string_lossy().to_string()
        })),
    )
    .await;
    assert_eq!(create_status, StatusCode::OK);
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].kind, WorkspaceAttachmentKind::ReferenceRepo);
    assert_eq!(created[0].name, "ref-fixture");

    let after_create = tokio::fs::read_to_string(&cfg_path).await.unwrap();
    assert_eq!(after_create, before);

    let (sync_status, synced): (StatusCode, Vec<WorkspaceAttachment>) = common::json_request(
        &app,
        Method::POST,
        format!("/api/workspaces/{}/attachments/sync", workspace.id.0),
        Some(json!({ "refresh": true })),
    )
    .await;
    assert_eq!(sync_status, StatusCode::OK);
    assert_eq!(synced.len(), 1);
    assert_eq!(synced[0].name, "ref-fixture");

    let after_sync = tokio::fs::read_to_string(&cfg_path).await.unwrap();
    assert_eq!(after_sync, before);

    let (list_status, listed): (StatusCode, Vec<WorkspaceAttachment>) = common::json_request(
        &app,
        Method::GET,
        format!("/api/workspaces/{}/attachments", workspace.id.0),
        None,
    )
    .await;
    assert_eq!(list_status, StatusCode::OK);
    assert_eq!(listed.len(), 1);

    let (delete_status, remaining): (StatusCode, Vec<WorkspaceAttachment>) = common::json_request(
        &app,
        Method::DELETE,
        format!("/api/workspaces/{}/attachments", workspace.id.0),
        Some(json!({
            "kind": "reference_repo",
            "name": "ref-fixture"
        })),
    )
    .await;
    assert_eq!(delete_status, StatusCode::OK);
    assert!(remaining.is_empty());

    let after_delete = tokio::fs::read_to_string(&cfg_path).await.unwrap();
    assert_eq!(after_delete, before);
}

#[tokio::test]
async fn workspace_attachments_sync_heals_stale_pending_when_materialized_exists() {
    let repo = common::init_git_repo(&[("README.md", "hello\n")]).await;

    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let state = &fixture.daemon;
    let app = fixture.router();

    let workspace = common::create_workspace(&app, repo.path(), "ws").await;
    let (create_status, created): (StatusCode, Vec<WorkspaceAttachment>) = common::json_request(
        &app,
        Method::POST,
        format!("/api/workspaces/{}/attachments", workspace.id.0),
        Some(json!({
            "kind": "reference_repo",
            "name": "ref-fixture",
            "source": repo.path().to_string_lossy(),
            "mode": "ro",
            "update_policy": "manual"
        })),
    )
    .await;
    assert_eq!(create_status, StatusCode::OK);
    let attachment = created.into_iter().next().expect("created attachment");

    let materialized = fixture
        .data_dir
        .path()
        .join("attachments")
        .join("reference-repos")
        .join("checkouts")
        .join(attachment.id.0.to_string())
        .join("default");
    tokio::fs::create_dir_all(&materialized).await.unwrap();
    tokio::fs::write(materialized.join("README.md"), "cached\n")
        .await
        .unwrap();

    state
        .set_workspace_attachment_status_for_test(
            workspace.id,
            attachment.id,
            WorkspaceAttachmentStatus::Pending,
        )
        .await
        .unwrap();

    let (sync_status, synced): (StatusCode, Vec<WorkspaceAttachment>) = common::json_request(
        &app,
        Method::POST,
        format!("/api/workspaces/{}/attachments/sync", workspace.id.0),
        Some(json!({ "refresh": false })),
    )
    .await;
    assert_eq!(sync_status, StatusCode::OK);
    assert_eq!(synced.len(), 1);
    assert_eq!(synced[0].status, WorkspaceAttachmentStatus::Ready);
    assert!(synced[0].last_sync_at.is_some());
}

#[tokio::test]
async fn workspace_attachments_reject_doc_mirror_local_script_sources() {
    let repo = common::init_git_repo(&[("README.md", "hello\n")]).await;
    let script_path = repo.path().join(".ctx").join("scripts").join("docs.py");
    tokio::fs::create_dir_all(script_path.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&script_path, "print('should not run')\n")
        .await
        .unwrap();

    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();

    let workspace = common::create_workspace(&app, repo.path(), "ws").await;

    let (create_status, body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        Method::POST,
        format!("/api/workspaces/{}/attachments", workspace.id.0),
        Some(json!({
            "kind": "doc_mirror",
            "name": "local-docs",
            "source": script_path.to_string_lossy().to_string()
        })),
    )
    .await;
    assert_eq!(create_status, StatusCode::BAD_REQUEST);
    let error = body["error"].as_str().unwrap_or_default();
    assert!(error.contains("http(s) URL"), "unexpected error: {error}");
    assert!(error.contains("not supported"), "unexpected error: {error}");
}

#[tokio::test]
async fn workspace_attachments_reject_doc_mirror_rw_mode() {
    let repo = common::init_git_repo(&[("README.md", "hello\n")]).await;

    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();

    let workspace = common::create_workspace(&app, repo.path(), "ws").await;

    let (create_status, body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        Method::POST,
        format!("/api/workspaces/{}/attachments", workspace.id.0),
        Some(json!({
            "kind": "doc_mirror",
            "name": "docs",
            "source": "https://example.com/docs",
            "mode": "rw"
        })),
    )
    .await;
    assert_eq!(create_status, StatusCode::BAD_REQUEST);
    let error = body["error"].as_str().unwrap_or_default();
    assert!(error.contains("read-only"), "unexpected error: {error}");
}

#[tokio::test]
async fn workspace_attachments_preserve_invalid_id_response_contracts() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();

    let list_req = Request::builder()
        .method(Method::GET)
        .uri("/api/workspaces/not-a-workspace/attachments")
        .body(Body::empty())
        .unwrap();
    let (list_status, list_body) = common::oneshot_bytes(&app, list_req).await;
    assert_eq!(list_status, StatusCode::BAD_REQUEST);
    assert!(
        list_body.is_empty(),
        "list invalid-id response should stay bodyless: {}",
        String::from_utf8_lossy(&list_body)
    );

    let (sync_status, sync_body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        Method::POST,
        "/api/workspaces/not-a-workspace/attachments/sync",
        Some(json!({ "refresh": true })),
    )
    .await;
    assert_eq!(sync_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        sync_body.get("error").and_then(serde_json::Value::as_str),
        Some("invalid workspace id")
    );

    let (create_status, create_body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        Method::POST,
        "/api/workspaces/not-a-workspace/attachments",
        Some(json!({
            "kind": "reference_repo",
            "name": "ref-fixture",
            "source": "/tmp/ref-fixture"
        })),
    )
    .await;
    assert_eq!(create_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        create_body.get("error").and_then(serde_json::Value::as_str),
        Some("invalid workspace id")
    );

    let (delete_status, delete_body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        Method::DELETE,
        "/api/workspaces/not-a-workspace/attachments",
        Some(json!({
            "kind": "reference_repo",
            "name": "ref-fixture"
        })),
    )
    .await;
    assert_eq!(delete_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        delete_body.get("error").and_then(serde_json::Value::as_str),
        Some("invalid workspace id")
    );
}

#[tokio::test]
async fn workspace_attachments_validate_payload_before_workspace_lookup() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();
    let missing_workspace_id = WorkspaceId::new();

    let (create_status, create_body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        Method::POST,
        format!("/api/workspaces/{}/attachments", missing_workspace_id.0),
        Some(json!({
            "kind": "reference_repo",
            "name": "",
            "source": ""
        })),
    )
    .await;
    assert_eq!(create_status, StatusCode::BAD_REQUEST);
    let create_error = create_body["error"].as_str().unwrap_or_default();
    assert!(
        create_error.contains("name and source are required"),
        "unexpected error: {create_error}"
    );

    let (delete_status, delete_body): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        Method::DELETE,
        format!("/api/workspaces/{}/attachments", missing_workspace_id.0),
        Some(json!({
            "kind": "reference_repo",
            "name": ""
        })),
    )
    .await;
    assert_eq!(delete_status, StatusCode::BAD_REQUEST);
    let delete_error = delete_body["error"].as_str().unwrap_or_default();
    assert!(
        delete_error.contains("name is required"),
        "unexpected error: {delete_error}"
    );
}
