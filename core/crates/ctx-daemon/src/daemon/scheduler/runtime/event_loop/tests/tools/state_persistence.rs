use super::fixtures::{HeadCacheSeed, ToolEventLoopFixture};
use super::*;

#[tokio::test]
async fn tool_events_publish_after_tool_state_persists() {
    let fixture = ToolEventLoopFixture::new(false, HeadCacheSeed::Compact).await;

    fixture
        .run_event(NormalizedEvent {
            event_type: SessionEventType::ToolCall,
            payload_json: json!({
                "tool_call_id": "call-1",
                "order_seq": 1,
                "toolCall": {
                    "name": "Bash",
                    "kind": "execute"
                },
                "status": "running",
                "rawInput": {
                    "command": "pwd"
                }
            }),
        })
        .await;

    let head = fixture
        .state
        .workspaces
        .workspace_active_snapshot
        .get_cached_session_head_for_read(fixture.session_id)
        .await
        .expect("hydrated session head should stay readable from the compact cache");
    let turn = head
        .turns
        .into_iter()
        .find(|turn| turn.turn_id == fixture.turn_id)
        .expect("turn in cached head");
    assert_eq!(turn.tool_total, 1);
    assert_eq!(turn.tool_running, 1);
    assert_eq!(turn.tool_pending, 0);
    assert_eq!(head.tool_summaries.len(), 1);
    assert_eq!(
        head.tool_summaries[0].status.as_deref(),
        Some("in_progress")
    );
}
