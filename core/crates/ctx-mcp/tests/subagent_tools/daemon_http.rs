use super::*;

#[tokio::test]
async fn mcp_agent_tools_call_daemon_http() {
    let parent_id = "00000000-0000-0000-0000-000000000001";
    let agent_id = "agent_testopaque";
    let context = json!({
        "session_id": parent_id,
        "workspace_id": "00000000-0000-0000-0000-000000000002",
        "worktree_id": "00000000-0000-0000-0000-000000000003",
        "capabilities": ["subagents", "artifacts"],
    });

    let app = Router::new()
        .route(
            "/api/mcp/context",
            get(move || {
                let context = context.clone();
                async move { Json(context) }
            }),
        )
        .route(
            &format!("/api/mcp/sessions/{parent_id}/spawn_agent"),
            post(move |Json(body): Json<serde_json::Value>| async move {
                assert_eq!(body["prompt"], "check foo");
                assert_eq!(body["task_label"], "Audit FooAPI");
                Json(json!({
                    "agent": {
                        "agent": {
                            "agent_id": agent_id,
                            "task_label": "Audit FooAPI",
                            "state": "running",
                            "health": "healthy",
                            "current_run_id": "run_spawned",
                            "last_event_seq": 1
                        }
                    }
                }))
            }),
        )
        .route(
            &format!("/api/mcp/sessions/{parent_id}/send_input"),
            post(move |Json(body): Json<serde_json::Value>| async move {
                assert_eq!(body["agent_id"], agent_id);
                assert_eq!(body["message"], "summarize output");
                Json(json!({
                    "agent": {
                        "agent": {
                            "agent_id": agent_id,
                            "task_label": "Audit FooAPI",
                            "state": "running",
                            "health": "healthy",
                            "current_run_id": "run_followup",
                            "last_event_seq": 2
                        }
                    },
                    "queued_run_id": "run_followup",
                    "delivery": "queued"
                }))
            }),
        )
        .route(
            &format!("/api/mcp/sessions/{parent_id}/archive_agent"),
            post(move |Json(body): Json<serde_json::Value>| async move {
                assert_eq!(body["agent_id"], agent_id);
                Json(json!({
                    "agent_id": agent_id,
                    "task_label": "Audit FooAPI",
                    "archived": true
                }))
            }),
        )
        .route(
            &format!("/api/mcp/sessions/{parent_id}/list_agents"),
            get(move || async move {
                Json(json!([
                    {
                        "agent_id": agent_id,
                        "task_label": "Audit FooAPI",
                        "state": "running",
                        "health": "healthy",
                        "current_run_id": "run_followup",
                        "last_event_seq": 2
                    }
                ]))
            }),
        )
        .route(
            &format!("/api/mcp/sessions/{parent_id}/wait_agent"),
            post(move |Json(body): Json<serde_json::Value>| async move {
                assert_eq!(body["agent_id"], agent_id);
                Json(json!({
                    "wait_status": "matched",
                    "mode": "any",
                    "until": "terminal",
                    "results": [{
                        "agent": {
                            "agent_id": agent_id,
                            "task_label": "Audit FooAPI",
                            "state": "waiting_input",
                            "health": "healthy",
                            "latest_result_status": "completed",
                            "last_event_seq": 3
                        },
                        "latest_result": {
                            "status": "completed",
                            "content": "final"
                        }
                    }]
                }))
            }),
        )
        .layer(ServiceBuilder::new());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

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
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    ] {
        stdin.write_all(msg.to_string().as_bytes()).await.unwrap();
        stdin.write_all(b"\n").await.unwrap();
        stdin.flush().await.unwrap();
    }
    let _ = wait_for_response(&mut reader, 1).await;
    let _ = wait_for_response(&mut reader, 2).await;

    let spawn_msg = json!({
        "jsonrpc":"2.0",
        "id":3,
        "method":"tools/call",
        "params":{
            "name":"ctx.spawn_agent",
            "arguments":{
                "worktree":"inherit",
                "prompt":"check foo",
                "task_label":"Audit FooAPI",
                "harness":"codex",
                "model":"gpt-5.2"
            }
        }
    });
    stdin
        .write_all(spawn_msg.to_string().as_bytes())
        .await
        .unwrap();
    stdin.write_all(b"\n").await.unwrap();
    stdin.flush().await.unwrap();

    let init_response = wait_for_response(&mut reader, 3).await;
    let text = init_response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("");
    let payload: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["agent"]["agent"]["task_label"], "Audit FooAPI");

    for msg in [
        json!({
            "jsonrpc":"2.0",
            "id":4,
            "method":"tools/call",
            "params":{
                "name":"ctx.send_input",
                "arguments":{
                    "agent_id": agent_id,
                    "message":"summarize output"
                }
            }
        }),
        json!({
            "jsonrpc":"2.0",
            "id":5,
            "method":"tools/call",
            "params":{
                "name":"ctx.list_agents",
                "arguments":{}
            }
        }),
        json!({
            "jsonrpc":"2.0",
            "id":7,
            "method":"tools/call",
            "params":{
                "name":"ctx.wait_agent",
                "arguments":{
                    "agent_id": agent_id
                }
            }
        }),
        json!({
            "jsonrpc":"2.0",
            "id":8,
            "method":"tools/call",
            "params":{
                "name":"ctx.archive_agent",
                "arguments":{
                    "agent_id": agent_id
                }
            }
        }),
    ] {
        stdin.write_all(msg.to_string().as_bytes()).await.unwrap();
        stdin.write_all(b"\n").await.unwrap();
    }
    stdin.flush().await.unwrap();

    let mut got_reply = false;
    let mut got_list = false;
    let mut got_wait = false;
    let mut got_archive = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        let Some(line) = reader.next_line().await.unwrap() else {
            break;
        };
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        match v.get("id").and_then(|id| id.as_i64()) {
            Some(4) => {
                let text = v["result"]["content"][0]["text"].as_str().unwrap_or("");
                assert!(text.contains("\"delivery\": \"queued\""));
                got_reply = true;
            }
            Some(5) => {
                let text = v["result"]["content"][0]["text"].as_str().unwrap_or("");
                assert!(text.contains("Audit FooAPI"));
                got_list = true;
            }
            Some(7) => {
                let text = v["result"]["content"][0]["text"].as_str().unwrap_or("");
                assert!(text.contains("final"));
                got_wait = true;
            }
            Some(8) => {
                let text = v["result"]["content"][0]["text"].as_str().unwrap_or("");
                assert!(text.contains("\"archived\": true"));
                got_archive = true;
            }
            _ => {}
        }
        if got_reply && got_list && got_wait && got_archive {
            break;
        }
    }

    assert!(got_reply, "did not receive send_input response");
    assert!(got_list, "did not receive list_agents response");
    assert!(got_wait, "did not receive wait_agent response");
    assert!(got_archive, "did not receive archive_agent response");
    let _ = child.kill().await;
}
