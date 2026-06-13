mod common;

use axum::http::{Method, StatusCode};
use ctx_core::ids::WorkspaceId;
use serde_json::Value;

const AGENT_DEFAULT_APPEND: &str = "You are working inside ctx, an agent development environment. Use ctx MCP tools to attach photos/videos as artifacts, start persistent web sessions (Playwright REPL/scripts), and run sub-agents for research or well-scoped implementations. Check `.ctx/attachments/refs/` and `.ctx/attachments/docs/` for extra reference repos and docs.";
const SUBAGENT_DEFAULT_APPEND: &str =
    "You are a subagent. The user messaging you is the primary agent who will provide your instructions.";

#[tokio::test]
async fn system_prompt_endpoints_round_trip_agent_and_subagent_append() {
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();
    let ws = common::create_workspace(&app, repo.path(), "ws").await;

    let (agent_get_status, agent_default): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        format!("/api/workspaces/{}/agent_system_prompt", ws.id.0),
        None,
    )
    .await;
    assert_eq!(agent_get_status, StatusCode::OK);
    assert_eq!(
        agent_default.get("default_append").and_then(Value::as_str),
        Some(AGENT_DEFAULT_APPEND)
    );
    assert_eq!(
        agent_default
            .get("effective_append")
            .and_then(Value::as_str),
        Some(AGENT_DEFAULT_APPEND)
    );
    assert_eq!(agent_default.get("configured_append"), Some(&Value::Null));
    assert_eq!(
        agent_default.get("source").and_then(Value::as_str),
        Some("default")
    );

    let (agent_post_status, agent_configured): (StatusCode, Value) = common::json_request(
        &app,
        Method::POST,
        format!("/api/workspaces/{}/agent_system_prompt", ws.id.0),
        Some(serde_json::json!({
            "system_prompt_append": "  Agent append  "
        })),
    )
    .await;
    assert_eq!(agent_post_status, StatusCode::OK);
    assert_eq!(
        agent_configured
            .get("configured_append")
            .and_then(Value::as_str),
        Some("Agent append")
    );
    assert_eq!(
        agent_configured
            .get("effective_append")
            .and_then(Value::as_str),
        Some("Agent append")
    );
    assert_eq!(
        agent_configured.get("source").and_then(Value::as_str),
        Some("config")
    );

    let (subagent_get_status, subagent_default): (StatusCode, Value) = common::json_request(
        &app,
        Method::GET,
        format!("/api/workspaces/{}/subagent_system_prompt", ws.id.0),
        None,
    )
    .await;
    assert_eq!(subagent_get_status, StatusCode::OK);
    assert_eq!(
        subagent_default
            .get("default_append")
            .and_then(Value::as_str),
        Some(SUBAGENT_DEFAULT_APPEND)
    );
    assert_eq!(
        subagent_default
            .get("effective_append")
            .and_then(Value::as_str),
        Some(SUBAGENT_DEFAULT_APPEND)
    );
    assert_eq!(
        subagent_default.get("configured_append"),
        Some(&Value::Null)
    );
    assert_eq!(
        subagent_default.get("source").and_then(Value::as_str),
        Some("default")
    );

    let (subagent_post_status, subagent_configured): (StatusCode, Value) = common::json_request(
        &app,
        Method::POST,
        format!("/api/workspaces/{}/subagent_system_prompt", ws.id.0),
        Some(serde_json::json!({
            "system_prompt_append": "  Subagent append  "
        })),
    )
    .await;
    assert_eq!(subagent_post_status, StatusCode::OK);
    assert_eq!(
        subagent_configured
            .get("configured_append")
            .and_then(Value::as_str),
        Some("Subagent append")
    );
    assert_eq!(
        subagent_configured
            .get("effective_append")
            .and_then(Value::as_str),
        Some("Subagent append")
    );
    assert_eq!(
        subagent_configured.get("source").and_then(Value::as_str),
        Some("config")
    );
}

#[tokio::test]
async fn system_prompt_endpoints_preserve_id_error_statuses() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();

    for path in [
        "/api/workspaces/not-a-workspace/agent_system_prompt",
        "/api/workspaces/not-a-workspace/subagent_system_prompt",
    ] {
        let (status, body): (StatusCode, Value) =
            common::json_request(&app, Method::GET, path, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            body.get("error").and_then(Value::as_str),
            Some("invalid workspace id")
        );

        let (status, body): (StatusCode, Value) = common::json_request(
            &app,
            Method::POST,
            path,
            Some(serde_json::json!({"system_prompt_append": "prompt"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            body.get("error").and_then(Value::as_str),
            Some("invalid workspace id")
        );
    }

    let missing_workspace_id = WorkspaceId::new();
    for path in [
        format!(
            "/api/workspaces/{}/agent_system_prompt",
            missing_workspace_id.0
        ),
        format!(
            "/api/workspaces/{}/subagent_system_prompt",
            missing_workspace_id.0
        ),
    ] {
        let (status, body): (StatusCode, Value) =
            common::json_request(&app, Method::GET, &path, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            body.get("error").and_then(Value::as_str),
            Some("workspace not found")
        );

        let (status, body): (StatusCode, Value) = common::json_request(
            &app,
            Method::POST,
            &path,
            Some(serde_json::json!({"system_prompt_append": "prompt"})),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            body.get("error").and_then(Value::as_str),
            Some("workspace not found")
        );
    }
}
