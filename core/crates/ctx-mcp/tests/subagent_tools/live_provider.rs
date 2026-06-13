use ctx_core::models::SessionEventType;
use uuid::Uuid;

use super::*;

#[tokio::test]
#[ignore = "Requires live provider credentials and a built ctx-mcp binary on the product path."]
async fn live_provider_parent_can_invoke_real_agent_via_ctx_mcp() {
    let provider_id = std::env::var("CTX_LIVE_PROVIDER_ID").ok();
    let model_id = std::env::var("CTX_LIVE_MODEL_ID").ok();
    if provider_id.is_none() || model_id.is_none() {
        eprintln!(
            "skipping: set CTX_LIVE_PROVIDER_ID and CTX_LIVE_MODEL_ID to run live subagent canary"
        );
        return;
    }
    let provider_id = provider_id.unwrap();
    let model_id = model_id.unwrap();
    if !matches!(provider_id.as_str(), "codex" | "claude" | "claude-crp") {
        eprintln!("skipping: live subagent canary only supports codex/claude providers");
        return;
    }

    std::env::set_var("CTX_MCP_COMMAND", mcp_bin());
    let fixture = setup_live_provider_parent_session(&provider_id, &model_id)
        .await
        .unwrap();
    let base_url = fixture.base_url().to_string();
    let session_id = fixture.session_id();

    let client = reqwest::Client::new();
    let token = format!("CTX_SUBAGENT_LIVE_OK_{}", Uuid::new_v4());
    client
        .post(format!("{base_url}/api/sessions/{}/messages", session_id.0))
        .json(&json!({
            "content": format!(
                "Use ctx.spawn_agent to launch exactly one agent labeled ping. Ask it to reply with exactly {token}. Read the returned agent.agent.agent_id, then use ctx.wait_agent with that agent_id. After the child agent completes, reply with exactly {token} and nothing else."
            )
        }))
        .send()
        .await
        .unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(240);
    loop {
        let events = fixture.list_session_events().await.unwrap();
        if events
            .iter()
            .any(|event| matches!(event.event_type, SessionEventType::Done))
        {
            break;
        }
        if events.iter().any(|event| {
            matches!(
                event.event_type,
                SessionEventType::Error | SessionEventType::AuthRequired
            )
        }) {
            panic!("live subagent canary saw terminal error/auth-required events: {events:#?}");
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("timed out waiting for live subagent canary completion");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let subagents = fixture.list_subagent_sessions().await.unwrap();
    assert!(
        !subagents.is_empty(),
        "expected live provider to create at least one subagent session"
    );

    let events = fixture.list_session_events().await.unwrap();
    let assistant_messages = events
        .iter()
        .filter(|event| matches!(event.event_type, SessionEventType::AssistantMessageInserted))
        .filter_map(|event| {
            event
                .payload_json
                .get("content")
                .and_then(|value| value.as_str())
        })
        .collect::<Vec<_>>();
    assert!(
        assistant_messages
            .iter()
            .any(|message| message.contains(&token)),
        "expected final assistant message containing {token}; saw {assistant_messages:#?}"
    );
}
