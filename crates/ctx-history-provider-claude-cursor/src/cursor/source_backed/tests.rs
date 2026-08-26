use super::*;
use ctx_history_core::EventRole;

#[test]
fn source_and_session_identities_are_root_scoped() {
    let released = source_key("same-session").unwrap();
    let compatibility = source_key_scoped("same-session", SourceAnchorScope::Unqualified).unwrap();
    let first = source_key_scoped("same-session", SourceAnchorScope::Lineage([1; 32])).unwrap();
    let second = source_key_scoped("same-session", SourceAnchorScope::Lineage([2; 32])).unwrap();

    assert!(released.exact_descriptor_eq(&compatibility));
    assert_ne!(first.identity(), second.identity());
    assert_ne!(
        session_id(&first, "same-session").unwrap(),
        session_id(&second, "same-session").unwrap()
    );
}

fn event(event_type: EventType, body: CursorEventBody) -> CursorNativeEvent {
    CursorNativeEvent {
        native_order: super::super::projection::CursorNativeOrder {
            semantic_ordinal: 7,
            physical_ordinal: 11,
            part_ordinal: 0,
        },
        event_type,
        role: EventRole::Tool,
        occurred_at: None,
        body,
        record_byte_start: 0,
        record_byte_end_exclusive: 1,
        record_sha256: [0; 32],
        provider_event_hash: [1; 32],
    }
}

#[test]
fn activity_preserves_exact_cursor_channels_without_flattened_mcp_inference() {
    let invocation = cursor_annotation(&event(
        EventType::ToolCall,
        CursorEventBody::ToolCall {
            native_content: serde_json::json!({
                "type": "tool_use",
                "id": " call-1 ",
                "name": "mcp__forge__read",
                "input": {"command": "  git status  ", "path": "./a"},
            }),
            call_id: Some(" call-1 ".to_owned()),
            tool_name: Some("mcp__forge__read".to_owned()),
            arguments: Some(serde_json::json!({"command": "  git status  ", "path": "./a"})),
            protocol: None,
            server: None,
            explicit_tool: None,
            call_id_unavailable: false,
            tool_name_unavailable: false,
            arguments_unavailable: false,
            mcp_identity_unavailable: false,
            native_content_unavailable: false,
            literal_facts: Vec::new(),
        },
    ))
    .expect("invocation annotation");
    let invocation = invocation.activity.expect("invocation activity");
    assert_eq!(
        invocation.provider_call_id,
        Some(TypedKey::utf8(" call-1 ").expect("call id"))
    );
    let invocation = invocation.invocation.expect("invocation channel");
    assert_eq!(invocation.protocol, None);
    assert_eq!(invocation.server, None);
    assert_eq!(invocation.tool, "mcp__forge__read");
    assert_eq!(
        invocation.arguments,
        ActivityJsonCapture::Present {
            value: serde_json::json!({"command": "  git status  ", "path": "./a"}),
        }
    );

    let result_value = serde_json::json!({
        "type": "tool_result",
        "tool_use_id": " call-1 ",
        "content": [" exact ", {"ok": false}],
    });
    let result = cursor_annotation(&event(
        EventType::ToolOutput,
        CursorEventBody::ToolOutput {
            native_content: result_value.clone(),
            call_id: Some(" call-1 ".to_owned()),
            call_id_unavailable: false,
            content_unavailable: false,
            native_content_unavailable: false,
            literal_facts: Vec::new(),
        },
    ))
    .expect("result annotation");
    let result = result.activity.expect("result activity");
    assert_eq!(
        result.provider_call_id,
        Some(TypedKey::utf8(" call-1 ").expect("call id"))
    );
    let result = result.result.expect("result channel");
    assert_eq!(result.status, None);
    assert_eq!(result.text, ActivityTextCapture::Absent);
    assert_eq!(
        result.structured_content,
        ActivityJsonCapture::Present {
            value: result_value,
        }
    );
}

#[test]
fn cursor_parser_preserves_result_content_and_explicit_capture_states() {
    let exact = br#"{"type":"message","role":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call","content":" exact text ","unknown":{"kept":true}}]}}"#;
    let events = project_cursor_jsonl_record(exact, 0, 0, 0, exact.len() as u64)
        .unwrap()
        .unwrap();
    let annotation = cursor_annotation(&events[0]).unwrap();
    assert_eq!(
        annotation.structured_content,
        Some(serde_json::json!({
            "type":"tool_result",
            "tool_use_id":"call",
            "content":" exact text ",
            "unknown":{"kept":true},
        }))
    );
    let result = annotation.activity.unwrap().result.unwrap();
    assert_eq!(
        result.text,
        ActivityTextCapture::Present {
            value: " exact text ".to_owned(),
        }
    );

    let absent = br#"{"type":"message","role":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call"}]}}"#;
    let events = project_cursor_jsonl_record(absent, 0, 0, 0, absent.len() as u64)
        .unwrap()
        .unwrap();
    let result = cursor_annotation(&events[0])
        .unwrap()
        .activity
        .unwrap()
        .result
        .unwrap();
    assert_eq!(result.text, ActivityTextCapture::Absent);
}

#[test]
fn cursor_duplicate_selectors_withhold_channels_and_facts_keep_raw_order() {
    let duplicate = br#"{"type":"message","role":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"call","name":"tool","input":{"path":"one"},"input":{"path":"two"}}]}}"#;
    let events = project_cursor_jsonl_record(duplicate, 0, 0, 0, duplicate.len() as u64)
        .unwrap()
        .unwrap();
    let annotation = cursor_annotation(&events[0]).unwrap();
    assert!(annotation.structured_content.is_none());
    let invocation = annotation.activity.unwrap().invocation.unwrap();
    assert_eq!(invocation.arguments, ActivityJsonCapture::Unavailable);

    let ordered = br#"{"type":"message","role":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"call","name":"tool","input":{"command":" c ","path":" p ","url":" u "}}]}}"#;
    let events = project_cursor_jsonl_record(ordered, 0, 0, 0, ordered.len() as u64)
        .unwrap()
        .unwrap();
    let activity = cursor_annotation(&events[0]).unwrap().activity.unwrap();
    assert_eq!(
        activity
            .facts
            .iter()
            .map(|fact| (fact.kind, fact.value.as_str()))
            .collect::<Vec<_>>(),
        [
            (ctx_history_core::LiteralFactKind::Command, " c "),
            (ctx_history_core::LiteralFactKind::File, " p "),
            (ctx_history_core::LiteralFactKind::Url, " u "),
        ]
    );
}
