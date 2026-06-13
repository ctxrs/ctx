mod common;

use std::collections::HashMap;
use std::sync::Arc;

use axum::http::{Method, StatusCode};
use ctx_core::ids::WorkspaceId;
use ctx_providers::adapters::{
    ProviderAdapter, ProviderRecommendedAction, ProviderUsability, ProviderUsabilityStatus,
};
use ctx_providers::fake::FakeProviderAdapter;
use serde_json::Value;

fn fake_codex_providers() -> HashMap<String, Arc<dyn ProviderAdapter>> {
    let mut providers = common::fake_providers();
    providers.insert("codex".into(), Arc::new(FakeProviderAdapter::new()));
    providers
}

async fn fake_codex_fixture() -> common::FakeDaemonFixture {
    let fixture =
        common::fake_daemon_fixture_with_providers(fake_codex_providers(), "http://127.0.0.1:0")
            .await;
    let mut status = FakeProviderAdapter::new()
        .inspect()
        .await
        .expect("inspect fake codex provider");
    status.provider_id = "codex".into();
    status.usability = ProviderUsability {
        usable: true,
        status: ProviderUsabilityStatus::Ready,
        reason_code: None,
        reason: None,
        blocking_provider_ids: Vec::new(),
        recommended_action: ProviderRecommendedAction::None,
    };
    fixture
        .daemon
        .upsert_provider_status("codex".into(), status)
        .await;
    fixture
}

#[tokio::test]
async fn workspace_provider_model_preference_preserves_id_error_statuses() {
    let fixture = fake_codex_fixture().await;
    let app = fixture.router();

    for method in [Method::GET, Method::POST] {
        let (status, body): (StatusCode, Value) = common::json_request(
            &app,
            method.clone(),
            "/api/workspaces/not-a-workspace/provider_model_preferences/codex",
            if method == Method::POST {
                Some(serde_json::json!({"preferred_model_id": "gpt-5.4/xhigh"}))
            } else {
                None
            },
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            body.get("error").and_then(Value::as_str),
            Some("invalid workspace id")
        );
    }

    let missing_workspace_id = WorkspaceId::new();
    for method in [Method::GET, Method::POST] {
        let (status, body): (StatusCode, Value) = common::json_request(
            &app,
            method.clone(),
            format!(
                "/api/workspaces/{}/provider_model_preferences/codex",
                missing_workspace_id.0
            ),
            if method == Method::POST {
                Some(serde_json::json!({"preferred_model_id": "gpt-5.4/xhigh"}))
            } else {
                None
            },
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            body.get("error").and_then(Value::as_str),
            Some("workspace not found")
        );
    }
}

#[tokio::test]
async fn workspace_provider_model_preference_endpoint_round_trips() {
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let fixture = fake_codex_fixture().await;
    let app = fixture.router();
    let workspace = common::create_workspace(&app, repo.path(), "ws").await;

    let (initial_status, initial_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        format!(
            "/api/workspaces/{}/provider_model_preferences/codex",
            workspace.id.0
        ),
        None,
    )
    .await;
    assert_eq!(initial_status, StatusCode::OK);
    assert_eq!(
        initial_body
            .get("preferred_model_id")
            .and_then(Value::as_str),
        None
    );

    let (set_status, set_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::POST,
        format!(
            "/api/workspaces/{}/provider_model_preferences/codex",
            workspace.id.0
        ),
        Some(serde_json::json!({
            "preferred_model_id": " gpt-5.4/xhigh "
        })),
    )
    .await;
    assert_eq!(set_status, StatusCode::OK);
    assert_eq!(
        set_body.get("preferred_model_id").and_then(Value::as_str),
        Some("gpt-5.4/xhigh")
    );

    let (clear_status, clear_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::POST,
        format!(
            "/api/workspaces/{}/provider_model_preferences/codex",
            workspace.id.0
        ),
        Some(serde_json::json!({
            "preferred_model_id": "   "
        })),
    )
    .await;
    assert_eq!(clear_status, StatusCode::OK);
    assert_eq!(
        clear_body.get("preferred_model_id").and_then(Value::as_str),
        None
    );
}

#[tokio::test]
async fn workspace_provider_model_preference_rejects_unknown_provider_ids() {
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let fixture = fake_codex_fixture().await;
    let app = fixture.router();
    let workspace = common::create_workspace(&app, repo.path(), "ws").await;

    let (status, body): (StatusCode, Value) = common::json_request(
        &app,
        Method::POST,
        format!(
            "/api/workspaces/{}/provider_model_preferences/missing-provider",
            workspace.id.0
        ),
        Some(serde_json::json!({
            "preferred_model_id": "gpt-5.4/xhigh"
        })),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        body.get("error").and_then(Value::as_str),
        Some("provider not found: missing-provider")
    );
}

#[tokio::test]
async fn provider_options_and_bootstrap_only_surface_valid_preferred_model_ids() {
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let fixture = fake_codex_fixture().await;
    let app = fixture.router();
    let workspace = common::create_workspace(&app, repo.path(), "ws").await;

    let (_set_status, _set_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::POST,
        format!(
            "/api/workspaces/{}/provider_model_preferences/codex",
            workspace.id.0
        ),
        Some(serde_json::json!({
            "preferred_model_id": "gpt-5.4/xhigh"
        })),
    )
    .await;

    let (bootstrap_status, bootstrap_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        format!("/api/workspaces/{}/providers/bootstrap", workspace.id.0),
        None,
    )
    .await;
    assert_eq!(bootstrap_status, StatusCode::OK);
    assert_eq!(
        bootstrap_body
            .pointer("/provider_options/codex/preferred_model_id")
            .and_then(Value::as_str),
        Some("gpt-5.4/xhigh")
    );

    let (_invalid_status, _invalid_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::POST,
        format!(
            "/api/workspaces/{}/provider_model_preferences/codex",
            workspace.id.0
        ),
        Some(serde_json::json!({
            "preferred_model_id": "missing-model"
        })),
    )
    .await;
    let (stored_pref_status, stored_pref_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        format!(
            "/api/workspaces/{}/provider_model_preferences/codex",
            workspace.id.0
        ),
        None,
    )
    .await;
    assert_eq!(stored_pref_status, StatusCode::OK);
    assert_eq!(
        stored_pref_body
            .get("preferred_model_id")
            .and_then(Value::as_str),
        None
    );

    let (invalid_bootstrap_status, invalid_bootstrap_body): (StatusCode, Value) =
        common::json_request(
            &app,
            Method::GET,
            format!("/api/workspaces/{}/providers/bootstrap", workspace.id.0),
            None,
        )
        .await;
    assert_eq!(invalid_bootstrap_status, StatusCode::OK);
    assert_eq!(
        invalid_bootstrap_body
            .pointer("/provider_options/codex/preferred_model_id")
            .and_then(Value::as_str),
        None
    );
    assert_eq!(
        invalid_bootstrap_body
            .pointer("/provider_options/codex/models/current_model_id")
            .and_then(Value::as_str),
        Some("gpt-5.4/medium")
    );
}

#[tokio::test]
async fn provider_options_cache_is_invalidated_when_preference_changes() {
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let fixture = fake_codex_fixture().await;
    let app = fixture.router();
    let workspace = common::create_workspace(&app, repo.path(), "ws").await;

    let (_set_initial_status, _set_initial_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::POST,
        format!(
            "/api/workspaces/{}/provider_model_preferences/codex",
            workspace.id.0
        ),
        Some(serde_json::json!({
            "preferred_model_id": "gpt-5.4/xhigh"
        })),
    )
    .await;

    let (initial_options_status, initial_options_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        format!("/api/workspaces/{}/providers/codex/options", workspace.id.0),
        None,
    )
    .await;
    assert_eq!(initial_options_status, StatusCode::OK);
    assert_eq!(
        initial_options_body
            .get("preferred_model_id")
            .and_then(Value::as_str),
        Some("gpt-5.4/xhigh")
    );

    let (_set_updated_status, _set_updated_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::POST,
        format!(
            "/api/workspaces/{}/provider_model_preferences/codex",
            workspace.id.0
        ),
        Some(serde_json::json!({
            "preferred_model_id": "gpt-5.4/medium"
        })),
    )
    .await;

    let (updated_options_status, updated_options_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        format!("/api/workspaces/{}/providers/codex/options", workspace.id.0),
        None,
    )
    .await;
    assert_eq!(updated_options_status, StatusCode::OK);
    assert_eq!(
        updated_options_body
            .get("preferred_model_id")
            .and_then(Value::as_str),
        Some("gpt-5.4/medium")
    );
}

#[tokio::test]
async fn malformed_workspace_model_preferences_do_not_break_bootstrap() {
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let fixture = fake_codex_fixture().await;
    let app = fixture.router();
    let workspace = common::create_workspace(&app, repo.path(), "ws").await;
    fixture
        .daemon
        .seed_invalid_workspace_runtime_settings_document_for_test(
            workspace.id,
            r#"{
  "new_session": {
    "preferred_model_by_provider": {
      "codex": 7
    }
  }
}"#,
        )
        .await
        .expect("write malformed runtime settings");

    let (bootstrap_status, bootstrap_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        format!("/api/workspaces/{}/providers/bootstrap", workspace.id.0),
        None,
    )
    .await;
    assert_eq!(
        bootstrap_status,
        StatusCode::OK,
        "bootstrap failed: {bootstrap_body:#?}"
    );
    assert_eq!(
        bootstrap_body
            .pointer("/provider_options/codex/preferred_model_id")
            .and_then(Value::as_str),
        None
    );

    let (options_status, options_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        format!("/api/workspaces/{}/providers/codex/options", workspace.id.0),
        None,
    )
    .await;
    assert_eq!(
        options_status,
        StatusCode::OK,
        "provider options failed: {options_body:#?}"
    );
    assert_eq!(
        options_body
            .get("preferred_model_id")
            .and_then(Value::as_str),
        None
    );
}

#[tokio::test]
async fn session_creation_and_model_switch_persist_workspace_provider_preference() {
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let fixture = fake_codex_fixture().await;
    let app = fixture.router();
    let workspace = common::create_workspace(&app, repo.path(), "ws").await;
    let session_id = uuid::Uuid::new_v4();

    let (create_status, task): (StatusCode, ctx_core::models::Task) = common::json_request(
        &app,
        Method::POST,
        format!("/api/workspaces/{}/tasks", workspace.id.0),
        Some(serde_json::json!({
            "title": "pref persistence",
            "default_session": {
                "id": session_id.to_string(),
                "provider_id": "codex",
                "model_id": "gpt-5.4",
                "reasoning_effort": "xhigh",
                "remember_model_preference": true
            }
        })),
    )
    .await;
    assert_eq!(create_status, StatusCode::OK);
    assert_eq!(task.primary_session_id.map(|id| id.0), Some(session_id));
    let (sessions_status, sessions): (StatusCode, Vec<ctx_core::models::Session>) =
        common::json_request(
            &app,
            Method::GET,
            format!("/api/tasks/{}/sessions", task.id.0),
            None,
        )
        .await;
    assert_eq!(sessions_status, StatusCode::OK);
    let created_session = sessions
        .into_iter()
        .find(|session| session.id.0 == session_id)
        .expect("created default session should be listed");
    assert_eq!(created_session.model_id, "gpt-5.4");
    assert_eq!(created_session.reasoning_effort.as_deref(), Some("xhigh"));

    let (created_pref_status, created_pref_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        format!(
            "/api/workspaces/{}/provider_model_preferences/codex",
            workspace.id.0
        ),
        None,
    )
    .await;
    assert_eq!(created_pref_status, StatusCode::OK);
    assert_eq!(
        created_pref_body
            .get("preferred_model_id")
            .and_then(Value::as_str),
        Some("gpt-5.4/xhigh")
    );

    let (update_status, updated_session): (StatusCode, ctx_core::models::Session) =
        common::json_request(
            &app,
            Method::POST,
            format!("/api/sessions/{}/model", created_session.id.0),
            Some(serde_json::json!({
                "model_id": "gpt-5.4",
                "reasoning_effort": "medium"
            })),
        )
        .await;
    assert_eq!(update_status, StatusCode::OK);
    assert_eq!(updated_session.model_id, "gpt-5.4");
    assert_eq!(updated_session.reasoning_effort.as_deref(), Some("medium"));

    let (updated_pref_status, updated_pref_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        format!(
            "/api/workspaces/{}/provider_model_preferences/codex",
            workspace.id.0
        ),
        None,
    )
    .await;
    assert_eq!(updated_pref_status, StatusCode::OK);
    assert_eq!(
        updated_pref_body
            .get("preferred_model_id")
            .and_then(Value::as_str),
        Some("gpt-5.4/medium")
    );
}

#[tokio::test]
async fn session_creation_does_not_persist_auto_seeded_workspace_provider_preference() {
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let fixture = fake_codex_fixture().await;
    let app = fixture.router();
    let workspace = common::create_workspace(&app, repo.path(), "ws").await;
    let (create_status, task): (StatusCode, ctx_core::models::Task) = common::json_request(
        &app,
        Method::POST,
        format!("/api/workspaces/{}/tasks", workspace.id.0),
        Some(serde_json::json!({
            "title": "pref persistence",
            "default_session": {
                "provider_id": "codex",
                "model_id": "gpt-5.4",
                "reasoning_effort": "medium"
            }
        })),
    )
    .await;
    assert_eq!(create_status, StatusCode::OK);
    assert!(task.primary_session_id.is_some());

    let (pref_status, pref_body): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        format!(
            "/api/workspaces/{}/provider_model_preferences/codex",
            workspace.id.0
        ),
        None,
    )
    .await;
    assert_eq!(pref_status, StatusCode::OK);
    assert_eq!(
        pref_body.get("preferred_model_id").and_then(Value::as_str),
        None
    );
}
