use super::*;
use ctx_history_core::{ActivityJsonCapture, ActivityTextCapture};

#[test]
fn source_and_related_session_identities_are_root_scoped() {
    let released = source_key("same-session").unwrap();
    let compatibility = source_key_scoped("same-session", SourceAnchorScope::Unqualified).unwrap();
    let first = source_key_scoped("same-session", SourceAnchorScope::Lineage([1; 32])).unwrap();
    let second = source_key_scoped("same-session", SourceAnchorScope::Lineage([2; 32])).unwrap();

    assert!(released.exact_descriptor_eq(&compatibility));
    assert_ne!(first.identity(), second.identity());
    assert_ne!(
        session_identity_for_native("parent", SourceAnchorScope::Lineage([1; 32])).unwrap(),
        session_identity_for_native("parent", SourceAnchorScope::Lineage([2; 32])).unwrap()
    );
}

#[test]
fn omp_title_slot_is_ignored_only_at_the_first_physical_record() {
    let title = br#"{"type":"title","v":1,"title":"fixture","updatedAt":"2026-08-20T15:12:20.989Z","pad":""}"#;

    assert!(is_omp_title_slot(JsonlRecordRef::for_test(title, 0)));
    assert!(!is_omp_title_slot(JsonlRecordRef::for_test(title, 1)));
}

#[test]
fn malformed_omp_title_slots_remain_rejected() {
    for title in [
        br#"{"type":"title","v":2,"title":"fixture","updatedAt":"2026-08-20T15:12:20.989Z","pad":""}"#.as_slice(),
        br#"{"type":"title","v":1,"title":"fixture","updatedAt":"2026-08-20T15:12:20.989Z"}"#.as_slice(),
        br#"{"type":"title","v":1,"title":"fixture","updatedAt":"2026-08-20T15:12:20.989Z","pad":"","source":"other"}"#.as_slice(),
    ] {
        assert!(!is_omp_title_slot(JsonlRecordRef::for_test(title, 0)));
    }
}

#[test]
fn omp_path_parent_uses_only_the_native_filename_claim() {
    assert_eq!(
        omp_parent_native_session_id(
            r"C:\Users\ctx\.omp\agent\sessions\2026-09-01T00-00-00-000Z_parent.jsonl".to_owned()
        ),
        Some("parent".to_owned())
    );
    assert_eq!(
        omp_parent_native_session_id("opaque-parent-id".to_owned()),
        Some("opaque-parent-id".to_owned())
    );
    assert_eq!(
        omp_parent_native_session_id("/tmp/not-an-omp-session.jsonl".to_owned()),
        None
    );
    assert_eq!(
        omp_session_id_from_path("/tmp/2026-09-01T00-00-00-000Z_parent_with_underscores.jsonl"),
        Some("parent_with_underscores")
    );
    assert_eq!(
        omp_session_id_from_path(
            r"C:\Users\ctx\.omp\agent\sessions\2026-09-01T00-00-00-000Z_parent.jsonl"
        ),
        Some("parent")
    );
    for malformed in [
        "/tmp/parent.jsonl",
        "/tmp/2026-09-01T00-00-00-000Z_.jsonl",
        "/tmp/2026-09-01T00-00-00-000Z_parent.jsonl.bak",
        "/tmp/not-a-timestamp_parent.jsonl",
    ] {
        assert_eq!(omp_session_id_from_path(malformed), None);
    }
}

#[test]
fn unlinked_output_withholds_result_but_preserves_literal_facts() {
    let message = serde_json::json!({
        "type": "bashExecution",
        "output": "future output",
    });
    let facts = vec![ProviderDeclaredFact {
        kind: LiteralFactKind::Command,
        value: "printf future".to_owned(),
    }];

    let activity = pi_activity(&message, EventType::CommandOutput, facts.clone())
        .unwrap()
        .unwrap();

    assert!(activity.provider_call_id.is_none());
    assert!(activity.invocation.is_none());
    assert!(activity.result.is_none());
    assert_eq!(activity.facts, facts);
}

#[test]
fn exact_call_id_retains_linked_output_result() {
    let message = serde_json::json!({
        "type": "toolResult",
        "toolCallId": "pi-call-1",
        "content": "provider output",
    });

    let activity = pi_activity(&message, EventType::ToolOutput, Vec::new())
        .unwrap()
        .unwrap();

    assert_eq!(
        activity.provider_call_id,
        Some(TypedKey::utf8("pi-call-1").unwrap())
    );
    assert!(activity.invocation.is_none());
    assert!(activity.result.is_some());
}

#[test]
fn unadmitted_optional_linkage_does_not_emit_empty_activity() {
    let oversized = "x".repeat(64 * 1024 + 1);
    let output = serde_json::json!({"toolCallId": oversized});
    assert_eq!(
        pi_activity(&output, EventType::ToolOutput, Vec::new()).unwrap(),
        None
    );

    let call = serde_json::json!({
        "toolCallId": "pi-call-1",
        "toolName": oversized,
    });
    assert_eq!(
        pi_activity(&call, EventType::ToolCall, Vec::new()).unwrap(),
        None
    );
}

#[test]
fn provider_output_is_projected_without_adjudicating_success() {
    let successful_tool = serde_json::json!({
        "type": "message",
        "message": {
            "role": "toolResult",
            "toolCallId": "pi-call-success",
            "content": "done",
            "isError": false,
        },
    });
    assert_eq!(
        projected_body(&successful_tool, EventType::ToolOutput),
        "done"
    );

    let successful_command = serde_json::json!({
        "type": "message",
        "message": {
            "role": "bashExecution",
            "command": "true",
            "output": "done",
            "exitCode": 0,
            "cancelled": false,
        },
    });
    assert_eq!(
        projected_body(&successful_command, EventType::CommandOutput),
        "done"
    );

    let failed_tool = serde_json::json!({
        "type": "message",
        "message": {
            "role": "toolResult",
            "toolCallId": "pi-call-failed",
            "content": "failure details",
            "isError": true,
        },
    });
    assert_eq!(
        projected_body(&failed_tool, EventType::ToolOutput),
        "failure details"
    );
}

#[test]
fn nested_tool_call_retains_joinable_invocation() {
    let message = serde_json::json!({"role": "assistant", "content": [
        {"type": "text", "text": "Running the command."},
        {"type": "toolCall", "id": "nested-call", "name": "bash",
            "arguments": {"command": "git status --short"}}
    ]});
    let activity = pi_activity(&message, EventType::ToolCall, Vec::new())
        .unwrap()
        .expect("nested invocation");
    assert_eq!(
        activity.provider_call_id,
        Some(TypedKey::utf8("nested-call").unwrap())
    );
    let invocation = activity.invocation.unwrap();
    assert_eq!(invocation.tool, "bash");
    assert_eq!(
        invocation.arguments,
        ActivityJsonCapture::Present {
            value: serde_json::json!({"command": "git status --short"})
        }
    );
}

#[test]
fn flat_call_aliases_stay_joinable() {
    for (id, name) in [
        ("toolCallId", "toolName"),
        ("tool_call_id", "tool_name"),
        ("callId", "name"),
    ] {
        let message = serde_json::json!({"role": "assistant", id: "flat-call", name: "bash", "arguments": {"command": "pwd"}});
        let kind = pi_event_type("message", Some(&message));
        assert_eq!(kind, EventType::ToolCall);
        let activity = pi_activity(&message, kind, Vec::new()).unwrap().unwrap();
        assert_eq!(
            activity.provider_call_id,
            Some(TypedKey::utf8("flat-call").unwrap())
        );
        assert_eq!(activity.invocation.unwrap().tool, "bash");
    }
}

#[test]
fn result_blocks_preserve_exact_text_empty_absent_and_native_error_status() {
    for (content, expected) in [
        (
            serde_json::json!([{ "type": "text", "text": "first" }, {"type": "text", "text": "second"}]),
            Some("first\nsecond"),
        ),
        (serde_json::json!("flat output"), Some("flat output")),
        (
            serde_json::json!([{ "type": "text", "text": "" }]),
            Some(""),
        ),
        (
            serde_json::json!([{ "type": "image", "data": "AA==", "mimeType": "image/png" }]),
            None,
        ),
        (serde_json::Value::Null, None),
    ] {
        for is_error in [false, true] {
            let message = serde_json::json!({"role": "toolResult", "toolCallId": "call", "content": content, "isError": is_error});
            let activity = pi_activity(&message, EventType::ToolOutput, Vec::new())
                .unwrap()
                .unwrap();
            let result = activity.result.unwrap();
            assert_eq!(
                result.text,
                expected.map_or(ActivityTextCapture::Absent, |value| {
                    ActivityTextCapture::Present {
                        value: value.to_owned(),
                    }
                })
            );
            assert_eq!(
                result.status.as_deref(),
                Some(if is_error { "error" } else { "success" })
            );
            assert_eq!(result.structured_content, ActivityJsonCapture::Absent);
        }
    }
}

#[test]
fn duplicate_nested_call_ids_preserve_content_without_arbitrary_linkage() {
    let value = serde_json::json!({"type": "message", "message": {"role": "assistant", "content": [
        {"type": "toolCall", "id": "duplicate", "name": "bash", "arguments": {"command": "pwd"}},
        {"type": "toolCall", "id": "duplicate", "name": "bash", "arguments": {"command": "git status"}}
    ]}});
    let entries = split_tool_calls(value.clone()).unwrap();
    assert_eq!(entries, vec![(0, value.clone())]);
    assert_eq!(
        pi_activity(&value["message"], EventType::ToolCall, Vec::new()).unwrap(),
        None
    );
}
