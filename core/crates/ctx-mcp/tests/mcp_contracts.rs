use std::time::Duration;

use axum::{
    http::HeaderMap,
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tower::ServiceBuilder;

const TEST_SESSION_ID: &str = "00000000-0000-0000-0000-000000000001";
const TEST_WORKSPACE_ID: &str = "00000000-0000-0000-0000-000000000002";
const TEST_WORKTREE_ID: &str = "00000000-0000-0000-0000-000000000003";

fn mcp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ctx-mcp")
}

fn mcp_command() -> Command {
    let mut command = Command::new(mcp_bin());
    // These tests run inside ctx and inherit ambient session env. Scrub it so
    // the child MCP process only sees the context each test sets explicitly.
    for key in [
        "CTX_AUTH_TOKEN",
        "CTX_BUNDLE_DIR",
        "CTX_BUILD_IDENTITY_PATH",
        "CTX_DATA_DIR",
        "CTX_DAEMON_URL",
        "CTX_MCP_CAPABILITIES",
        "CTX_MCP_DEV_MODE",
        "CTX_MCP_TOKEN",
        "CTX_SESSION_ID",
        "CTX_WORKTREE_ID",
        "CTX_WORKTREE_ROOT",
    ] {
        command.env_remove(key);
    }
    command
}

async fn write_mcp_message(stdin: &mut tokio::process::ChildStdin, msg: Value) {
    let is_initialize = msg.get("method").and_then(Value::as_str) == Some("initialize");
    stdin.write_all(msg.to_string().as_bytes()).await.unwrap();
    stdin.write_all(b"\n").await.unwrap();
    if is_initialize {
        // The child-process stdio harness can leave a single initialize line
        // pending under cargo test on macOS. The server ignores blank lines.
        stdin.write_all(b"\n").await.unwrap();
    }
    stdin.flush().await.unwrap();
}

async fn wait_for_response(
    reader: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    response_id: i64,
    timeout: Duration,
) -> Value {
    let timeout = std::cmp::max(timeout, Duration::from_secs(60));
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let next_line = tokio::time::timeout_at(deadline, reader.next_line())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for response id {response_id}"));
        let Some(line) = next_line.unwrap() else {
            break;
        };
        let value: Value = serde_json::from_str(&line).unwrap();
        if value.get("id").and_then(|id| id.as_i64()) == Some(response_id) {
            return value;
        }
    }
    panic!("timed out waiting for response id {response_id}");
}

fn mcp_context_response(capabilities: Vec<&'static str>) -> Value {
    json!({
        "session_id": TEST_SESSION_ID,
        "workspace_id": TEST_WORKSPACE_ID,
        "worktree_id": TEST_WORKTREE_ID,
        "capabilities": capabilities,
    })
}

async fn serve_context_daemon(capabilities: Vec<&'static str>) -> std::net::SocketAddr {
    let context = mcp_context_response(capabilities);
    let app = Router::new().route(
        "/api/mcp/context",
        get(move || {
            let context = context.clone();
            async move { Json(context) }
        }),
    );
    serve_router(app).await
}

async fn serve_router(app: Router) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

#[tokio::test]
async fn mcp_tools_list_omits_removed_lsp_and_edit_plan_tools() {
    let addr = serve_context_daemon(vec!["subagents", "artifacts"]).await;
    let mut child = mcp_command()
        .arg("--stdio")
        .env("CTX_DAEMON_URL", format!("http://{addr}"))
        .env("CTX_MCP_TOKEN", "scoped-mcp-token")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout).lines();

    write_mcp_message(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}),
    )
    .await;
    write_mcp_message(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    )
    .await;

    let v = wait_for_response(&mut reader, 2, Duration::from_secs(15)).await;
    let tools = v["result"]["tools"].as_array().expect("tools array");
    let names: Vec<String> = tools
        .iter()
        .filter_map(|t| {
            t.get("name")
                .and_then(|n| n.as_str())
                .map(|s| s.to_string())
        })
        .collect();
    assert!(
        !names.iter().any(|n| n.starts_with("lsp_")),
        "expected removed lsp_* tools to stay absent"
    );
    assert!(
        !names.iter().any(|n| matches!(
            n.as_str(),
            "list_edit_plans" | "get_edit_plan" | "apply_edit_plan" | "discard_edit_plan"
        )),
        "expected removed edit plan tools to stay absent"
    );
    let _ = child.kill().await;
}

#[tokio::test]
async fn mcp_tools_list_omits_global_workspace_and_oracle_tools() {
    let addr = serve_context_daemon(vec!["subagents", "artifacts"]).await;
    let mut child = mcp_command()
        .arg("--stdio")
        .env("CTX_DAEMON_URL", format!("http://{addr}"))
        .env("CTX_MCP_TOKEN", "scoped-mcp-token")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout).lines();

    write_mcp_message(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}),
    )
    .await;
    let _ = wait_for_response(&mut reader, 1, Duration::from_secs(15)).await;
    write_mcp_message(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    )
    .await;

    let v = wait_for_response(&mut reader, 2, Duration::from_secs(15)).await;
    let tools = v["result"]["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect();
    assert!(
        !names.contains(&"list_workspaces"),
        "ctx-mcp should not expose cross-workspace discovery"
    );
    assert!(
        !names.contains(&"oracle"),
        "ctx-mcp should not expose global oracle authority"
    );
    assert!(
        names.contains(&"spawn_agent"),
        "ctx-mcp should keep session-local agent tools"
    );
    assert!(
        !names.contains(&"merge_queue_submit"),
        "ctx-mcp should not expose merge queue submit without an explicit capability"
    );
    let _ = child.kill().await;
}

#[tokio::test]
async fn mcp_tools_list_includes_merge_queue_submit_with_explicit_capability() {
    let addr = serve_context_daemon(vec!["subagents", "artifacts", "merge_queue_submit"]).await;
    let mut child = mcp_command()
        .arg("--stdio")
        .env("CTX_DAEMON_URL", format!("http://{addr}"))
        .env("CTX_MCP_TOKEN", "scoped-mcp-token")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout).lines();

    write_mcp_message(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}),
    )
    .await;
    let _ = wait_for_response(&mut reader, 1, Duration::from_secs(15)).await;
    write_mcp_message(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    )
    .await;

    let v = wait_for_response(&mut reader, 2, Duration::from_secs(15)).await;
    let tools = v["result"]["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect();
    assert!(
        names.contains(&"merge_queue_submit"),
        "merge queue submit should be advertised only when explicitly scoped"
    );
    let _ = child.kill().await;
}

#[tokio::test]
async fn mcp_tools_list_uses_daemon_context_instead_of_forged_capability_env() {
    let addr = serve_context_daemon(vec!["subagents", "artifacts"]).await;
    let mut child = mcp_command()
        .arg("--stdio")
        .env("CTX_DAEMON_URL", format!("http://{addr}"))
        .env("CTX_MCP_TOKEN", "scoped-mcp-token")
        .env("CTX_MCP_CAPABILITIES", "merge_queue_submit")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout).lines();

    write_mcp_message(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}),
    )
    .await;
    let _ = wait_for_response(&mut reader, 1, Duration::from_secs(15)).await;
    write_mcp_message(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    )
    .await;

    let v = wait_for_response(&mut reader, 2, Duration::from_secs(15)).await;
    let tools = v["result"]["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect();
    assert!(
        !names.contains(&"merge_queue_submit"),
        "forged CTX_MCP_CAPABILITIES must not enable merge queue submit"
    );
    let _ = child.kill().await;
}

#[tokio::test]
async fn mcp_tools_list_gates_subagent_and_artifact_tools_by_daemon_context() {
    let addr = serve_context_daemon(vec!["merge_queue_submit"]).await;
    let mut child = mcp_command()
        .arg("--stdio")
        .env("CTX_DAEMON_URL", format!("http://{addr}"))
        .env("CTX_MCP_TOKEN", "scoped-mcp-token")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout).lines();

    write_mcp_message(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}),
    )
    .await;
    let _ = wait_for_response(&mut reader, 1, Duration::from_secs(15)).await;
    write_mcp_message(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    )
    .await;

    let v = wait_for_response(&mut reader, 2, Duration::from_secs(15)).await;
    let tools = v["result"]["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect();
    assert!(names.contains(&"merge_queue_submit"));
    assert!(!names.contains(&"spawn_agent"));
    assert!(!names.contains(&"list_agents"));
    assert!(!names.contains(&"artifacts_set"));
    let _ = child.kill().await;
}

#[tokio::test]
async fn mcp_merge_queue_submit_call_requires_explicit_capability() {
    let addr = serve_context_daemon(vec!["subagents", "artifacts"]).await;
    let mut child = mcp_command()
        .arg("--stdio")
        .env("CTX_DAEMON_URL", format!("http://{addr}"))
        .env("CTX_MCP_TOKEN", "scoped-mcp-token")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout).lines();

    write_mcp_message(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}),
    )
    .await;
    let _ = wait_for_response(&mut reader, 1, Duration::from_secs(15)).await;
    write_mcp_message(
        &mut stdin,
        json!({
            "jsonrpc":"2.0",
            "id":2,
            "method":"tools/call",
            "params":{
                "name":"ctx.merge_queue_submit",
                "arguments":{
                    "target_branch":"main",
                    "message":"merge it"
                }
            }
        }),
    )
    .await;

    let v = wait_for_response(&mut reader, 2, Duration::from_secs(15)).await;
    assert_eq!(v["result"]["isError"].as_bool(), Some(true));
    let text = v["result"]["content"][0]["text"]
        .as_str()
        .expect("tool error text");
    assert!(
        text.contains("requires an explicit scoped MCP capability"),
        "expected capability error, got: {text}"
    );
    let _ = child.kill().await;
}

#[tokio::test]
async fn mcp_tools_call_gates_subagent_and_artifact_capabilities_from_daemon_context() {
    let addr = serve_context_daemon(vec!["merge_queue_submit"]).await;
    let mut child = mcp_command()
        .arg("--stdio")
        .env("CTX_DAEMON_URL", format!("http://{addr}"))
        .env("CTX_MCP_TOKEN", "scoped-mcp-token")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout).lines();

    for msg in [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}),
        json!({
            "jsonrpc":"2.0",
            "id":2,
            "method":"tools/call",
            "params":{
                "name":"ctx.spawn_agent",
                "arguments":{
                    "worktree":"inherit",
                    "prompt":"check foo",
                    "task_label":"Audit FooAPI"
                }
            }
        }),
        json!({
            "jsonrpc":"2.0",
            "id":3,
            "method":"tools/call",
            "params":{
                "name":"ctx.artifacts_set",
                "arguments":{
                    "artifacts":[{"absoluteFilePath":"/tmp/artifact.txt"}]
                }
            }
        }),
    ] {
        stdin.write_all(msg.to_string().as_bytes()).await.unwrap();
        stdin.write_all(b"\n").await.unwrap();
        stdin.flush().await.unwrap();
    }

    let _ = wait_for_response(&mut reader, 1, Duration::from_secs(15)).await;
    let spawn = wait_for_response(&mut reader, 2, Duration::from_secs(15)).await;
    let spawn_text = spawn["result"]["content"][0]["text"]
        .as_str()
        .expect("spawn error text");
    assert!(
        spawn_text.contains("subagents capability"),
        "expected subagents capability error, got: {spawn_text}"
    );
    let artifacts = wait_for_response(&mut reader, 3, Duration::from_secs(15)).await;
    let artifacts_text = artifacts["result"]["content"][0]["text"]
        .as_str()
        .expect("artifacts error text");
    assert!(
        artifacts_text.contains("artifacts capability"),
        "expected artifacts capability error, got: {artifacts_text}"
    );
    let _ = child.kill().await;
}

#[tokio::test]
async fn mcp_artifacts_set_uses_daemon_context_instead_of_forged_session_env() {
    let seen = std::sync::Arc::new(tokio::sync::Mutex::new(None::<String>));
    let seen2 = seen.clone();
    let context = mcp_context_response(vec!["subagents", "artifacts"]);
    let app = Router::new()
        .route(
            "/api/mcp/context",
            get(move || {
                let context = context.clone();
                async move { Json(context) }
            }),
        )
        .route(
            &format!("/api/sessions/{TEST_SESSION_ID}/artifacts"),
            post(move |Json(body): Json<Value>| {
                let seen2 = seen2.clone();
                async move {
                    let path = body["artifacts"][0]["absolute_file_path"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    *seen2.lock().await = Some(path);
                    Json(json!([]))
                }
            }),
        );
    let addr = serve_router(app).await;

    let mut child = mcp_command()
        .arg("--stdio")
        .env("CTX_DAEMON_URL", format!("http://{addr}"))
        .env("CTX_MCP_TOKEN", "scoped-mcp-token")
        .env("CTX_SESSION_ID", "ffffffff-ffff-ffff-ffff-ffffffffffff")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout).lines();

    for msg in [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}),
        json!({
            "jsonrpc":"2.0",
            "id":2,
            "method":"tools/call",
            "params":{
                "name":"ctx.artifacts_set",
                "arguments":{
                    "artifacts":[{"absoluteFilePath":"/tmp/artifact.txt"}]
                }
            }
        }),
    ] {
        stdin.write_all(msg.to_string().as_bytes()).await.unwrap();
        stdin.write_all(b"\n").await.unwrap();
        stdin.flush().await.unwrap();
    }

    let _ = wait_for_response(&mut reader, 1, Duration::from_secs(15)).await;
    let response = wait_for_response(&mut reader, 2, Duration::from_secs(15)).await;
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    assert_eq!(
        seen.lock().await.as_deref(),
        Some("/tmp/artifact.txt"),
        "artifact post must target daemon-derived session route"
    );
    let _ = child.kill().await;
}

#[tokio::test]
async fn mcp_removed_lsp_tool_calls_return_actionable_errors() {
    let addr = serve_context_daemon(vec!["subagents", "artifacts"]).await;
    let mut child = mcp_command()
        .arg("--stdio")
        .env("CTX_DAEMON_URL", format!("http://{addr}"))
        .env("CTX_MCP_TOKEN", "scoped-mcp-token")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout).lines();

    write_mcp_message(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}),
    )
    .await;
    let _ = wait_for_response(&mut reader, 1, Duration::from_secs(15)).await;
    write_mcp_message(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"lsp_hover","arguments":{"workspace":"ignored","file":"ignored","line":1,"character":1}}}),
    )
    .await;

    let v = wait_for_response(&mut reader, 2, Duration::from_secs(15)).await;
    assert_eq!(v["result"]["isError"].as_bool(), Some(true));
    let text = v["result"]["content"][0]["text"]
        .as_str()
        .expect("tool error text");
    assert!(
        text.contains("tool removed: lsp_hover"),
        "expected removed tool message, got: {text}"
    );
    assert!(
        text.contains("795129c6a"),
        "expected recovery commit in removed tool message, got: {text}"
    );
    let _ = child.kill().await;
}

#[tokio::test]
async fn mcp_agent_tool_schemas_avoid_top_level_combinators() {
    let addr = serve_context_daemon(vec!["subagents", "artifacts"]).await;
    let mut child = mcp_command()
        .arg("--stdio")
        .env("CTX_DAEMON_URL", format!("http://{addr}"))
        .env("CTX_MCP_TOKEN", "scoped-mcp-token")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout).lines();

    write_mcp_message(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}),
    )
    .await;
    let _ = wait_for_response(&mut reader, 1, Duration::from_secs(15)).await;
    write_mcp_message(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    )
    .await;

    let v = wait_for_response(&mut reader, 2, Duration::from_secs(15)).await;
    let tools = v["result"]["tools"].as_array().expect("tools array");
    let spawn_agent = tools
        .iter()
        .find(|tool| tool.get("name").and_then(|name| name.as_str()) == Some("spawn_agent"))
        .expect("missing tool spawn_agent");
    let required = spawn_agent["inputSchema"]["required"]
        .as_array()
        .expect("spawn_agent required array");
    assert!(
        required
            .iter()
            .any(|value| value.as_str() == Some("worktree")),
        "spawn_agent schema must require worktree"
    );
    for tool_name in ["wait_agent", "interrupt_agent", "archive_agent"] {
        let tool = tools
            .iter()
            .find(|tool| tool.get("name").and_then(|name| name.as_str()) == Some(tool_name))
            .unwrap_or_else(|| panic!("missing tool {tool_name}"));
        let schema = tool["inputSchema"].as_object().expect("inputSchema object");
        assert_eq!(
            schema.get("type").and_then(|value| value.as_str()),
            Some("object"),
            "expected {tool_name} schema type=object"
        );
        for key in ["anyOf", "allOf", "oneOf", "not", "enum"] {
            assert!(
                !schema.contains_key(key),
                "expected {tool_name} schema to avoid top-level {key}"
            );
        }
    }
    let _ = child.kill().await;
}

#[tokio::test]
async fn mcp_global_workspace_tool_call_returns_removed_error() {
    let addr = serve_context_daemon(vec!["subagents", "artifacts"]).await;
    let mut child = mcp_command()
        .arg("--stdio")
        .env("CTX_DAEMON_URL", format!("http://{addr}"))
        .env("CTX_MCP_TOKEN", "scoped-mcp-token")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout).lines();

    for msg in [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}),
        json!({
            "jsonrpc":"2.0",
            "id":2,
            "method":"tools/call",
            "params":{
                "name":"ctx.list_workspaces",
                "arguments":{}
            }
        }),
    ] {
        stdin.write_all(msg.to_string().as_bytes()).await.unwrap();
        stdin.write_all(b"\n").await.unwrap();
    }
    stdin.flush().await.unwrap();

    let v = wait_for_response(&mut reader, 2, Duration::from_secs(15)).await;
    assert_eq!(v["result"]["isError"].as_bool(), Some(true));
    let text = v["result"]["content"][0]["text"]
        .as_str()
        .expect("tool error text");
    assert!(
        text.contains("tool removed: list_workspaces"),
        "expected removed list_workspaces message, got: {text}"
    );
    assert!(
        text.contains("session/worktree-local tools"),
        "expected scoped-authority explanation, got: {text}"
    );
    let _ = child.kill().await;
}

#[tokio::test]
async fn mcp_daemon_access_requires_scoped_mcp_token() {
    let app = Router::new()
        .route(
            "/api/merge-queue/entries",
            post(|| async { Json(json!({"status":"queued"})) }),
        )
        .layer(ServiceBuilder::new());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let temp_dir = tempfile::tempdir().unwrap();
    let daemon_auth = json!({
        "token": "local-daemon-token",
        "daemon_url": "http://127.0.0.1:4399"
    });
    tokio::fs::write(
        temp_dir.path().join("daemon_auth.json"),
        serde_json::to_vec(&daemon_auth).unwrap(),
    )
    .await
    .unwrap();

    let mut child = mcp_command()
        .arg("--stdio")
        .env("CTX_DATA_DIR", temp_dir.path())
        .env("CTX_DAEMON_URL", format!("http://{addr}"))
        .env("CTX_AUTH_TOKEN", "daemon-secret")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout).lines();

    write_mcp_message(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}),
    )
    .await;
    let _ = wait_for_response(&mut reader, 1, Duration::from_secs(15)).await;
    write_mcp_message(
        &mut stdin,
        json!({
            "jsonrpc":"2.0",
            "id":2,
            "method":"tools/call",
            "params":{
                "name":"ctx.merge_queue_submit",
                "arguments":{
                    "target_branch":"main",
                    "message":"merge it"
                }
            }
        }),
    )
    .await;

    let v = wait_for_response(&mut reader, 2, Duration::from_secs(15)).await;
    assert_eq!(
        v["error"]["message"].as_str(),
        Some("ctx-mcp context unavailable")
    );
    let text = v["error"]["data"]["error"]
        .as_str()
        .expect("context error text");
    assert!(
        text.contains("missing scoped ctx-mcp token"),
        "expected scoped MCP token requirement, got: {text}"
    );
    assert!(
        text.contains("CTX_MCP_TOKEN"),
        "expected CTX_MCP_TOKEN guidance, got: {text}"
    );
    let _ = child.kill().await;
}

#[tokio::test]
async fn mcp_daemon_access_requires_explicit_daemon_url() {
    let temp_dir = tempfile::tempdir().unwrap();
    let daemon_auth = json!({
        "token": "local-daemon-token",
        "daemon_url": "http://127.0.0.1:4399"
    });
    tokio::fs::write(
        temp_dir.path().join("daemon_auth.json"),
        serde_json::to_vec(&daemon_auth).unwrap(),
    )
    .await
    .unwrap();

    let mut child = mcp_command()
        .arg("--stdio")
        .env("CTX_DATA_DIR", temp_dir.path())
        .env("CTX_MCP_TOKEN", "scoped-mcp-token")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout).lines();

    write_mcp_message(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}),
    )
    .await;
    let _ = wait_for_response(&mut reader, 1, Duration::from_secs(15)).await;
    write_mcp_message(
        &mut stdin,
        json!({
            "jsonrpc":"2.0",
            "id":2,
            "method":"tools/call",
            "params":{
                "name":"ctx.merge_queue_submit",
                "arguments":{
                    "target_branch":"main",
                    "message":"merge it"
                }
            }
        }),
    )
    .await;

    let v = wait_for_response(&mut reader, 2, Duration::from_secs(15)).await;
    assert_eq!(
        v["error"]["message"].as_str(),
        Some("ctx-mcp context unavailable")
    );
    let text = v["error"]["data"]["error"]
        .as_str()
        .expect("context error text");
    assert!(
        text.contains("missing daemon URL"),
        "expected daemon URL requirement, got: {text}"
    );
    assert!(
        text.contains("CTX_DAEMON_URL"),
        "expected CTX_DAEMON_URL guidance, got: {text}"
    );
    let _ = child.kill().await;
}

#[tokio::test]
async fn mcp_merge_queue_submit_scrubs_internal_ids() {
    let body_tx = std::sync::Arc::new(tokio::sync::Mutex::new(None::<Value>));
    let body_tx2 = body_tx.clone();

    let context = mcp_context_response(vec!["subagents", "artifacts", "merge_queue_submit"]);
    let app = Router::new()
        .route(
            "/api/mcp/context",
            get(move || {
                let context = context.clone();
                async move { Json(context) }
            }),
        )
        .route(
            "/api/merge-queue/entries",
            post(move |Json(body): Json<Value>| {
                let body_tx2 = body_tx2.clone();
                async move {
                    *body_tx2.lock().await = Some(body);
                    Json(json!({
                        "id":"entry-1",
                        "session_id":"sess-internal",
                        "workspace_id":"ws-1",
                        "worktree_id":"wt-1",
                        "task_id":"task-1",
                        "status":"queued",
                        "target_branch":"main",
                        "message":"merge it",
                        "meta": {
                            "session_id": "sess-internal",
                            "note": "keep"
                        }
                    }))
                }
            }),
        )
        .layer(ServiceBuilder::new());

    let addr = serve_router(app).await;

    let mut child = mcp_command()
        .arg("--stdio")
        .env("CTX_DAEMON_URL", format!("http://{addr}"))
        .env("CTX_MCP_TOKEN", "scoped-mcp-token")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout).lines();

    for msg in [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}),
        json!({
            "jsonrpc":"2.0",
            "id":2,
            "method":"tools/call",
            "params":{
                "name":"ctx.merge_queue_submit",
                "arguments":{
                    "target_branch":"main",
                    "message":"merge it"
                }
            }
        }),
    ] {
        stdin.write_all(msg.to_string().as_bytes()).await.unwrap();
        stdin.write_all(b"\n").await.unwrap();
    }
    stdin.flush().await.unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut got_call = false;
    while tokio::time::Instant::now() < deadline {
        let Some(line) = reader.next_line().await.unwrap() else {
            break;
        };
        let v: Value = serde_json::from_str(&line).unwrap();
        if v.get("id").and_then(|id| id.as_i64()) == Some(2) {
            let text = v["result"]["content"][0]["text"].as_str().unwrap_or("");
            let payload: Value = serde_json::from_str(text).unwrap();
            let obj = payload.as_object().expect("response must be object");
            assert_eq!(obj.get("status").and_then(|v| v.as_str()), Some("queued"));
            assert_eq!(
                obj.get("target_branch").and_then(|v| v.as_str()),
                Some("main")
            );
            assert_eq!(
                obj.get("message").and_then(|v| v.as_str()),
                Some("merge it")
            );
            assert!(obj.get("id").is_none(), "internal id should be scrubbed");
            assert!(
                obj.get("session_id").is_none(),
                "internal session_id should be scrubbed"
            );
            assert!(
                obj.get("workspace_id").is_none(),
                "internal workspace_id should be scrubbed"
            );
            assert!(
                obj.get("worktree_id").is_none(),
                "internal worktree_id should be scrubbed"
            );
            assert!(
                obj.get("task_id").is_none(),
                "internal task_id should be scrubbed"
            );
            let meta = obj
                .get("meta")
                .and_then(|v| v.as_object())
                .expect("missing meta");
            assert!(
                meta.get("session_id").is_none(),
                "nested session_id should be scrubbed"
            );
            assert_eq!(meta.get("note").and_then(|v| v.as_str()), Some("keep"));
            got_call = true;
            break;
        }
    }

    assert!(got_call, "did not receive merge_queue_submit response");

    let body = body_tx.lock().await.clone().expect("missing request body");
    assert_eq!(
        body.get("session_id").and_then(|v| v.as_str()),
        Some(TEST_SESSION_ID),
        "expected session context to be passed to daemon"
    );
    assert_eq!(
        body.get("worktree_id").and_then(|v| v.as_str()),
        Some(TEST_WORKTREE_ID),
        "expected worktree context to be passed to daemon"
    );

    let _ = child.kill().await;
}

#[tokio::test]
async fn scoped_mcp_merge_queue_submit_uses_scoped_ids_instead_of_worktree_root() {
    let body_tx = std::sync::Arc::new(tokio::sync::Mutex::new(None::<Value>));
    let auth_tx = std::sync::Arc::new(tokio::sync::Mutex::new(None::<String>));
    let body_tx2 = body_tx.clone();
    let auth_tx2 = auth_tx.clone();

    let context = mcp_context_response(vec!["subagents", "artifacts", "merge_queue_submit"]);
    let app = Router::new()
        .route(
            "/api/mcp/context",
            get(move || {
                let context = context.clone();
                async move { Json(context) }
            }),
        )
        .route(
            "/api/merge-queue/entries",
            post(move |headers: HeaderMap, Json(body): Json<Value>| {
                let body_tx2 = body_tx2.clone();
                let auth_tx2 = auth_tx2.clone();
                async move {
                    *body_tx2.lock().await = Some(body);
                    *auth_tx2.lock().await = headers
                        .get(axum::http::header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_string);
                    Json(json!({
                        "id":"entry-1",
                        "status":"queued",
                        "target_branch":"main",
                        "message":"merge it"
                    }))
                }
            }),
        );

    let addr = serve_router(app).await;

    let mut child = mcp_command()
        .arg("--stdio")
        .env("CTX_DAEMON_URL", format!("http://{addr}"))
        .env("CTX_MCP_TOKEN", "scoped-mcp-token")
        .env("CTX_SESSION_ID", "ffffffff-ffff-ffff-ffff-ffffffffffff")
        .env("CTX_WORKTREE_ID", "eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout).lines();

    for msg in [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}),
        json!({
            "jsonrpc":"2.0",
            "id":2,
            "method":"tools/call",
            "params":{
                "name":"ctx.merge_queue_submit",
                "arguments":{
                    "target_branch":"main",
                    "message":"merge it"
                }
            }
        }),
    ] {
        stdin.write_all(msg.to_string().as_bytes()).await.unwrap();
        stdin.write_all(b"\n").await.unwrap();
    }
    stdin.flush().await.unwrap();

    let _ = wait_for_response(&mut reader, 2, Duration::from_secs(15)).await;

    let auth = auth_tx.lock().await.clone().expect("missing auth header");
    assert_eq!(auth, "Bearer scoped-mcp-token");

    let body = body_tx.lock().await.clone().expect("missing request body");
    assert_eq!(
        body.get("session_id").and_then(|v| v.as_str()),
        Some(TEST_SESSION_ID),
        "expected scoped session context to be passed to daemon"
    );
    assert!(
        body.get("worktree_root").is_none(),
        "scoped ctx-mcp should not fall back to worktree_root"
    );
    assert_eq!(
        body.get("worktree_id").and_then(|v| v.as_str()),
        Some(TEST_WORKTREE_ID),
        "expected scoped worktree context to be passed to daemon"
    );

    let _ = child.kill().await;
}

#[tokio::test]
async fn mcp_oracle_tool_call_returns_removed_error() {
    let addr = serve_context_daemon(vec!["subagents", "artifacts"]).await;
    let mut child = mcp_command()
        .arg("--stdio")
        .env("CTX_DAEMON_URL", format!("http://{addr}"))
        .env("CTX_MCP_TOKEN", "scoped-mcp-token")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout).lines();

    for msg in [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}),
        json!({
            "jsonrpc":"2.0",
            "id":2,
            "method":"tools/call",
            "params":{
                "name":"ctx.oracle",
                "arguments":{
                    "prompt_path":"prompt.txt"
                }
            }
        }),
    ] {
        stdin.write_all(msg.to_string().as_bytes()).await.unwrap();
        stdin.write_all(b"\n").await.unwrap();
    }
    stdin.flush().await.unwrap();

    let v = wait_for_response(&mut reader, 2, Duration::from_secs(15)).await;
    assert_eq!(v["result"]["isError"].as_bool(), Some(true));
    let text = v["result"]["content"][0]["text"]
        .as_str()
        .expect("tool error text");
    assert!(
        text.contains("tool removed: oracle"),
        "expected removed oracle message, got: {text}"
    );
    assert!(
        text.contains("session/worktree-local tools"),
        "expected scoped-authority explanation, got: {text}"
    );

    let _ = child.kill().await;
}
