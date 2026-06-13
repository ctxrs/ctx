mod tests {
    use super::*;

    #[test]
    fn build_turn_tools_from_events_keeps_latest_preview_instead_of_merging() {
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let created_at = Utc::now();
        let update_event = SessionEvent {
            seq: 1,
            id: SessionEventId::new(),
            session_id,
            run_id: None,
            turn_id: Some(turn_id),
            event_type: SessionEventType::ToolCallUpdate,
            payload_json: serde_json::json!({
                "tool_call_id": "tool-1",
                "order_seq": 1,
                "status": "running",
                "output_preview": "line-1\nline-2\n... +4 lines\nline-7\nline-8",
                "output_truncated": true,
                "output_original_bytes": 64
            }),
            transient: true,
            created_at,
        };
        let result_event = SessionEvent {
            seq: 2,
            id: SessionEventId::new(),
            session_id,
            run_id: None,
            turn_id: Some(turn_id),
            event_type: SessionEventType::ToolResult,
            payload_json: serde_json::json!({
                "tool_call_id": "tool-1",
                "order_seq": 1,
                "status": "completed",
                "output_preview": "line-1\nline-2\n... +6 lines\nline-9\nline-10",
                "output_truncated": true,
                "output_original_bytes": 80
            }),
            transient: false,
            created_at,
        };

        let tools =
            build_turn_tools_from_events(session_id, turn_id, &[update_event, result_event]);
        assert_eq!(tools.len(), 1);
        assert_eq!(
            tools[0].output_text.as_deref(),
            Some("line-1\nline-2\n... +6 lines\nline-9\nline-10")
        );
        assert_eq!(tools[0].output_original_bytes, Some(80));
        assert_eq!(tools[0].status.as_deref(), Some("completed"));
    }
}
