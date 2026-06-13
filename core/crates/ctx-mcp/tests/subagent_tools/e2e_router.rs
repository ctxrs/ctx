use super::*;

#[tokio::test]
async fn mcp_agent_tools_work_end_to_end_against_real_daemon_router() {
    let fixture = setup_fake_provider_parent_session().await.unwrap();

    let mut child = mcp_command()
        .arg("--stdio")
        .env("CTX_DAEMON_URL", fixture.base_url())
        .env("CTX_MCP_TOKEN", fixture.mcp_token())
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
    let list_response = wait_for_response(&mut reader, 2).await;
    let list_text = list_response["result"]["tools"].to_string();
    assert!(list_text.contains("\"spawn_agent\""));

    let spawn_msg = json!({
        "jsonrpc":"2.0",
        "id":3,
        "method":"tools/call",
        "params":{
            "name":"ctx.spawn_agent",
            "arguments":{
                "worktree":"inherit",
                "prompt":"reply with exactly OK",
                "task_label":"Audit FooAPI",
                "harness":"fake",
                "model":"fake-model"
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
    let init_text = init_response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("");
    let init_payload: serde_json::Value = serde_json::from_str(init_text).unwrap();
    let agent_id = init_payload["agent"]["agent"]["agent_id"]
        .as_str()
        .expect("spawn_agent agent_id")
        .to_string();
    assert_eq!(init_payload["agent"]["agent"]["task_label"], "Audit FooAPI");
    let init_state = init_payload["agent"]["agent"]["state"].as_str();
    assert!(matches!(init_state, Some("starting") | Some("running")));

    stdin
        .write_all(
            json!({
                "jsonrpc":"2.0",
                "id":4,
                "method":"tools/call",
                "params":{
                    "name":"ctx.wait_agent",
                    "arguments":{
                        "agent_id": agent_id
                    }
                }
            })
            .to_string()
            .as_bytes(),
        )
        .await
        .unwrap();
    stdin.write_all(b"\n").await.unwrap();
    stdin.flush().await.unwrap();

    let wait_response = wait_for_response(&mut reader, 4).await;
    let wait_text = wait_response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("");
    let wait_payload: serde_json::Value = serde_json::from_str(wait_text).unwrap();
    assert_eq!(wait_payload["wait_status"], "matched");
    assert_eq!(
        wait_payload["results"][0]["agent"]["task_label"],
        "Audit FooAPI"
    );
    assert_eq!(
        wait_payload["results"][0]["agent"]["latest_result_status"], "completed",
        "wait payload: {wait_payload:#}"
    );
    assert!(wait_payload["results"][0]["latest_result"]["content"]
        .as_str()
        .unwrap_or("")
        .contains("done: reply with exactly OK"));

    stdin
        .write_all(
            json!({
                "jsonrpc":"2.0",
                "id":5,
                "method":"tools/call",
                "params":{
                    "name":"ctx.archive_agent",
                    "arguments":{
                        "agent_id": agent_id
                    }
                }
            })
            .to_string()
            .as_bytes(),
        )
        .await
        .unwrap();
    stdin.write_all(b"\n").await.unwrap();
    stdin.flush().await.unwrap();

    let archive_response = wait_for_response(&mut reader, 5).await;
    let archive_text = archive_response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("");
    let archive_payload: serde_json::Value = serde_json::from_str(archive_text).unwrap();
    assert_eq!(archive_payload["task_label"], "Audit FooAPI");
    assert_eq!(archive_payload["archived"], true);

    stdin
        .write_all(
            json!({
                "jsonrpc":"2.0",
                "id":6,
                "method":"tools/call",
                "params":{
                    "name":"ctx.list_agents",
                    "arguments":{}
                }
            })
            .to_string()
            .as_bytes(),
        )
        .await
        .unwrap();
    stdin.write_all(b"\n").await.unwrap();
    stdin.flush().await.unwrap();

    let list_response = wait_for_response(&mut reader, 6).await;
    let list_text = list_response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("");
    let list_payload: serde_json::Value = serde_json::from_str(list_text).unwrap();
    assert_eq!(list_payload.as_array().map(Vec::len), Some(0));

    let _ = child.kill().await;
}
