use super::fixtures::{HeadCacheSeed, ToolEventLoopFixture};
use super::*;

#[tokio::test]
async fn tool_result_uses_sanitized_payload_for_persisted_summary() {
    let fixture = ToolEventLoopFixture::new(true, HeadCacheSeed::Active).await;

    fixture
        .run_event(NormalizedEvent {
            event_type: SessionEventType::ToolResult,
            payload_json: json!({
                "tool_call_id": "toolu_exec_result_1",
                "kind": "unknown",
                "tool_name": "unknown",
                "title": "unknown",
                "status": "completed",
                "output_preview": "1"
            }),
        })
        .await;

    let tool = fixture
        .store
        .get_session_turn_tool(fixture.session_id, "toolu_exec_result_1")
        .await
        .expect("load tool")
        .expect("persisted tool summary");
    assert_eq!(tool.order_seq, 1);
    assert_eq!(tool.tool_kind.as_deref(), Some("execute"));
    assert_eq!(tool.provider_tool_name.as_deref(), Some("Bash"));
    assert_eq!(tool.title.as_deref(), Some("Bash"));
    assert_eq!(tool.output_text.as_deref(), Some("1"));

    let events = fixture
        .store
        .list_session_events_for_turn(fixture.session_id, fixture.turn_id, false)
        .await
        .expect("load persisted events");
    let result_event = events
        .into_iter()
        .find(|event| matches!(event.event_type, SessionEventType::ToolResult))
        .expect("tool result event");
    assert!(result_event.payload_json.get("output_artifact").is_none());
}
