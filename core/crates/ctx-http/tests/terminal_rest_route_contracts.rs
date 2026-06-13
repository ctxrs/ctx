use reqwest::StatusCode;
use serde_json::json;

use ctx_core::ids::{TerminalId, WorkspaceId};
use ctx_core::models::{TerminalSession, Workspace};

mod common;

async fn assert_empty_body(response: reqwest::Response) {
    let body = response.text().await.unwrap();
    assert!(body.is_empty(), "expected empty body, got {body:?}");
}

#[tokio::test]
async fn terminal_rest_routes_preserve_error_body_contracts() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let server = fixture.spawn_server().await;
    let base = &server.base_url;
    let client = &server.client;

    let response = client
        .get(format!("{base}/api/workspaces/not-a-uuid/terminals"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_empty_body(response).await;

    let nonexistent_workspace_id = WorkspaceId::new();
    let response = client
        .get(format!(
            "{base}/api/workspaces/{}/terminals",
            nonexistent_workspace_id.0
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.json::<Vec<TerminalSession>>().await.unwrap().len(),
        0
    );

    let response = client
        .post(format!("{base}/api/workspaces/not-a-uuid/terminals"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap(),
        json!({"error": "invalid workspace id"})
    );

    for (body, message) in [
        (json!({"task_id": "not-a-task"}), "invalid task_id"),
        (json!({"session_id": "not-a-session"}), "invalid session_id"),
        (
            json!({"worktree_id": "not-a-worktree"}),
            "invalid worktree_id",
        ),
    ] {
        let response = client
            .post(format!(
                "{base}/api/workspaces/{}/terminals",
                nonexistent_workspace_id.0
            ))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response.json::<serde_json::Value>().await.unwrap(),
            json!({"error": message})
        );
    }

    let response = client
        .delete(format!("{base}/api/terminals/not-a-terminal"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_empty_body(response).await;

    let missing_terminal_id = TerminalId::new();
    let response = client
        .delete(format!("{base}/api/terminals/{}", missing_terminal_id.0))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_empty_body(response).await;

    let response = client
        .post(format!("{base}/api/terminals/not-a-terminal/stream_token"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_empty_body(response).await;

    let response = client
        .post(format!(
            "{base}/api/terminals/{}/stream_token",
            missing_terminal_id.0
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_empty_body(response).await;
}

#[tokio::test]
async fn terminal_rest_routes_preserve_success_wire_shapes() {
    let repo = common::init_git_repo(&[("file.txt", "hello\n")]).await;
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let server = fixture.spawn_server().await;
    let base = &server.base_url;
    let client = &server.client;

    let workspace: Workspace = client
        .post(format!("{base}/api/workspaces"))
        .json(&json!({"root_path": repo.path(), "name": "ws"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let terminal_response = client
        .post(format!(
            "{base}/api/workspaces/{}/terminals",
            workspace.id.0
        ))
        .json(&json!({"cwd": repo.path()}))
        .send()
        .await
        .unwrap();
    assert_eq!(terminal_response.status(), StatusCode::OK);
    let terminal_body = terminal_response.json::<serde_json::Value>().await.unwrap();
    let workspace_id_text = workspace.id.0.to_string();
    assert!(terminal_body.get("id").is_some());
    assert_eq!(
        terminal_body
            .get("workspace_id")
            .and_then(|value| value.as_str()),
        Some(workspace_id_text.as_str())
    );
    assert_eq!(
        terminal_body.get("status").and_then(|value| value.as_str()),
        Some("running")
    );
    assert!(terminal_body.get("stream_path").is_some());
    let terminal: TerminalSession = serde_json::from_value(terminal_body).unwrap();

    let list: Vec<TerminalSession> = client
        .get(format!(
            "{base}/api/workspaces/{}/terminals",
            workspace.id.0
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(list.iter().any(|candidate| candidate.id == terminal.id));

    let token_response = client
        .post(format!(
            "{base}/api/terminals/{}/stream_token",
            terminal.id.0
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(token_response.status(), StatusCode::OK);
    let token = token_response.json::<serde_json::Value>().await.unwrap();
    assert_eq!(
        token
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>(),
        ["expires_at".to_string(), "stream_path".to_string()]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
    );
    assert!(token["stream_path"]
        .as_str()
        .unwrap()
        .contains("/api/terminals/"));
    assert!(token["expires_at"].as_str().is_some());

    let delete_response = client
        .delete(format!("{base}/api/terminals/{}", terminal.id.0))
        .send()
        .await
        .unwrap();
    assert_eq!(delete_response.status(), StatusCode::NO_CONTENT);
    assert_empty_body(delete_response).await;
}
