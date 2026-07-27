use super::*;
use serde_json::json;

fn test_row() -> KiroConversationRow {
    KiroConversationRow {
        table: "conversations_v2",
        rowid: 11,
        key: "/workspace/project".to_owned(),
        conversation_id: Some("kiro-session-11".to_owned()),
        value: String::new(),
        created_at: None,
        updated_at: None,
    }
}

#[test]
fn assistant_tool_use_does_not_invent_a_completion_result() {
    let message = kiro_assistant_message(&json!({
        "assistant": {
            "ToolUse": {
                "content": "created commit 0123456789abcdef0123456789abcdef01234567",
                "tool_uses": [{"name": "shell", "input": {"command": "git commit"}}]
            }
        }
    }))
    .unwrap();
    assert_eq!(message.event_type, EventType::ToolCall);
}

#[test]
fn authoritative_history_iterator_preserves_edge_record_order_identity_and_content() {
    let row = test_row();
    let provider_session_id = "kiro-session-11";
    let started_at = DateTime::parse_from_rfc3339("2026-07-18T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let value = json!({
        "history": [
            {
                "user": {
                    "content": {"Prompt": {"prompt": "first user"}},
                    "timestamp": "2026-07-18T00:00:01Z"
                },
                "assistant": {
                    "Response": {"content": "first assistant"},
                    "timestamp": "2026-07-18T00:00:02Z"
                }
            },
            {
                "unrecognized": true
            },
            {
                "assistant": {
                    "ToolUse": {
                        "toolUses": [{"name": "write"}, {"name": "shell"}]
                    },
                    "timestamp": "2026-07-18T00:00:03Z"
                }
            },
            {
                "user": {
                    "content": {"Prompt": {"prompt": "   "}}
                },
                "assistant": {
                    "ToolUse": {"content": "plain assistant fallback"}
                },
                "timestamp": "2026-07-18T00:00:04Z"
            },
            {"unrecognized": true}
        ]
    });

    let decoded_events = kiro_history_events(&row, provider_session_id, &value, started_at)
        .map(|decoded| {
            let text = decoded.complete_text();
            (decoded.event, text)
        })
        .collect::<Vec<_>>();

    assert_eq!(
        decoded_events
            .iter()
            .map(|(event, _)| event.provider_event_index)
            .collect::<Vec<_>>(),
        vec![0, 1, 5, 7]
    );
    assert_eq!(
        decoded_events
            .iter()
            .map(|(event, _)| event.event_type)
            .collect::<Vec<_>>(),
        vec![
            EventType::Message,
            EventType::Message,
            EventType::ToolCall,
            EventType::Message,
        ]
    );
    assert!(decoded_events
        .iter()
        .all(|(event, _)| event.provider_event_hash.is_none()));
    assert_eq!(
        decoded_events
            .iter()
            .map(|(event, _)| {
                event.metadata["legacy_provider_event_hash"]
                    .as_str()
                    .unwrap()
            })
            .collect::<Vec<_>>(),
        vec![
            "conversations_v2:kiro-session-11:0:user",
            "conversations_v2:kiro-session-11:0:assistant",
            "conversations_v2:kiro-session-11:2:assistant",
            "conversations_v2:kiro-session-11:3:assistant",
        ]
    );
    assert_eq!(
        decoded_events
            .iter()
            .map(|(event, _)| event.cursor.as_str())
            .collect::<Vec<_>>(),
        vec![
            "conversations_v2:kiro-session-11:history:0:user",
            "conversations_v2:kiro-session-11:history:0:assistant",
            "conversations_v2:kiro-session-11:history:2:assistant",
            "conversations_v2:kiro-session-11:history:3:assistant",
        ]
    );
    assert_eq!(
        decoded_events
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>(),
        vec![
            "first user",
            "first assistant",
            "tool calls: write, shell",
            "plain assistant fallback",
        ]
    );
}

#[test]
fn malformed_history_shapes_fail_closed_without_events() {
    let row = test_row();
    for value in [
        json!({}),
        json!({"history": null}),
        json!({"history": {}}),
        json!({"history": "not-an-array"}),
    ] {
        assert!(
            kiro_history_events(&row, "kiro-session-11", &value, DateTime::<Utc>::UNIX_EPOCH,)
                .next()
                .is_none()
        );
    }
}
