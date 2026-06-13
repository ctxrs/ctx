use super::fixtures::{HeadCacheSeed, ToolEventLoopFixture};
use super::*;

#[tokio::test]
async fn large_tool_result_spills_to_artifact_and_keeps_preview_bounded() {
    let fixture = ToolEventLoopFixture::new(true, HeadCacheSeed::Active).await;

    let large_output = (1..=10)
        .map(|index| format!("line-{index}"))
        .collect::<Vec<_>>()
        .join("\n");
    fixture
        .run_event(NormalizedEvent {
            event_type: SessionEventType::ToolResult,
            payload_json: json!({
                "tool_call_id": "tool-large-1",
                "kind": "execute",
                "tool_name": "Bash",
                "title": "Bash",
                "status": "completed",
                "output_text": large_output,
                "order_seq": 1
            }),
        })
        .await;

    let events = fixture
        .store
        .list_session_events_for_turn(fixture.session_id, fixture.turn_id, false)
        .await
        .expect("load persisted events");
    let result_event = events
        .into_iter()
        .find(|event| matches!(event.event_type, SessionEventType::ToolResult))
        .expect("tool result event");
    let output_preview = result_event
        .payload_json
        .get("output_preview")
        .and_then(serde_json::Value::as_str)
        .expect("output preview");
    assert!(output_preview.contains("... +6 lines"));
    assert!(result_event.payload_json.get("output_text").is_none());
    let artifact_id = result_event
        .payload_json
        .get("output_artifact")
        .and_then(|value| value.get("artifact_id"))
        .and_then(serde_json::Value::as_str)
        .expect("artifact id");
    let artifact = fixture
        .store
        .get_artifact(ctx_core::ids::ArtifactId(
            uuid::Uuid::parse_str(artifact_id).expect("uuid"),
        ))
        .await
        .expect("artifact lookup")
        .expect("artifact row");
    assert_eq!(artifact.mime_type, "text/plain");
    assert_eq!(
        tokio::fs::read_to_string(&artifact.absolute_path)
            .await
            .expect("artifact body")
            .lines()
            .count(),
        10
    );

    let session_state = fixture
        .store
        .get_session_state(fixture.session_id)
        .await
        .expect("load session state");
    assert_eq!(session_state.artifacts.len(), 1);
    assert_eq!(session_state.artifacts[0].id, artifact.id);
}
