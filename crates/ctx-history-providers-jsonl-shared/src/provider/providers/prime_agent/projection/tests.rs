use super::*;
use serde_json::json;

fn projector() -> PrimeProjector<()> {
    let source = source_key("session", SourceAnchorScope::Unqualified).unwrap();
    PrimeProjector {
        session: session_id(&source, "session").unwrap(),
        rejections: JsonlRecordRejections::new(
            source.clone(),
            CaptureProvider::PrimeAgent,
            "/sessions/session.jsonl",
        ),
        source,
        binding: Binding {
            native_id: "session".into(),
            cwd: "/work/project".into(),
            parent: None,
            relationship: None,
            agent_scope: Some(AgentScope::Primary),
        },
        runtime: PhantomData,
    }
}

fn event(message: Value) -> Value {
    json!({"type":"message", "id":"event", "timestamp":"2026-10-01T12:00:00Z", "message":message})
}

#[test]
fn assistant_text_and_multiple_python_calls_have_separate_stable_records() {
    let value = event(json!({"role":"assistant", "content":[
        {"type":"text", "text":"Checking the implementation."},
        {"type":"toolCall", "id":"call-1", "name":"ipython", "arguments":{"code":"print(42)"}},
        {"type":"toolCall", "id":"call-2", "name":"python_repl", "arguments":{"code":"print(43)"}}
    ]}));
    let records = projector().records(&value, 5).unwrap();
    assert_eq!(records.len(), 3);
    assert_eq!(records[0].event_type, "message");
    assert_eq!(
        records[0].content.normalized_body.as_deref(),
        Some("Checking the implementation.")
    );
    for (index, record) in records.iter().enumerate().skip(1) {
        assert_eq!(record.event_type, "tool_call");
        assert_eq!(record.role, None);
        let activity = record.content.activity.as_ref().unwrap();
        assert_eq!(
            activity.provider_call_id,
            Some(TypedKey::utf8(format!("call-{index}")).unwrap())
        );
        assert!(activity.invocation.is_some());
        assert_ne!(record.event_id, records[0].event_id);
    }
    let again = projector().records(&value, 9).unwrap();
    assert_eq!(
        records.iter().map(|r| r.event_id).collect::<Vec<_>>(),
        again.iter().map(|r| r.event_id).collect::<Vec<_>>()
    );
}

#[test]
fn results_keep_exact_code_output_diffs_and_file_facts() {
    let value = event(
        json!({"role":"toolResult", "toolCallId":"call-1", "toolName":"ipython",
        "content":[{"type":"text", "text":"42\n"}], "isError":false,
        "details":{"status":"ok", "stdout":"42\n", "diffs":[{"path":"src/lib.rs", "diff":"+1 new line"}]}}),
    );
    let records = projector().records(&value, 2).unwrap();
    let record = &records[0];
    assert_eq!(record.event_type, "tool_output");
    assert!(record
        .content
        .normalized_body
        .as_ref()
        .unwrap()
        .contains("new line"));
    let activity = record.content.activity.as_ref().unwrap();
    assert!(activity
        .facts
        .iter()
        .any(|f| f.kind == LiteralFactKind::File && f.value == "src/lib.rs"));
    assert_eq!(
        activity.result.as_ref().unwrap().status.as_deref(),
        Some("ok")
    );
    assert_eq!(
        activity.provider_call_id,
        Some(TypedKey::utf8("call-1").unwrap())
    );
}

#[test]
fn custom_messages_keep_text_without_binary_or_private_provider_fields() {
    let value = json!({"type":"custom_message", "id":"custom", "timestamp":"2026-10-01T12:00:00Z",
        "customType":"goal_context", "content":[{"type":"text", "text":"Finish the parser"},
        {"type":"image", "data":"PRIVATE_IMAGE"}, {"type":"thinking", "thinking":"PRIVATE_THOUGHT", "redacted":true}],
        "details":{"secret":"PRIVATE_DETAIL"}});
    let records = projector().records(&value, 2).unwrap();
    assert_eq!(
        records[0].content.normalized_body.as_deref(),
        Some("Finish the parser")
    );
    assert!(!serde_json::to_string(&records).unwrap().contains("PRIVATE"));
    let message = event(
        json!({"role":"assistant", "content":[{"type":"thinking", "thinking":"plan", "thinkingSignature":"PRIVATE_SIGNATURE"}],
        "usage":{"cost":"PRIVATE_ACCOUNT"}, "responseId":"PRIVATE_RESPONSE"}),
    );
    assert!(
        !serde_json::to_string(&projector().records(&message, 3).unwrap())
            .unwrap()
            .contains("PRIVATE")
    );
}

#[test]
fn metadata_only_sessions_have_no_placeholder_text() {
    let header = json!({"type":"session", "version":3, "id":"session", "timestamp":"2026-10-01T12:00:00Z", "cwd":"/work/project"});
    let records = projector().records(&header, 0).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].content.normalized_body, None);
    assert!(records[0].content.structured_content.is_some());
    assert_eq!(records[0].role, None);
    assert!(projector().records(&header, 2).is_err());
}

#[test]
fn invalid_rows_and_indivisible_oversized_content_are_rejected() {
    assert!(projector().records(&json!({"type":"message"}), 1).is_err());
    let huge = event(
        json!({"role":"user", "content":"x".repeat(ctx_history_core::MAX_CORE_CONTENT_BYTES+1)}),
    );
    assert!(projector().records(&huge, 1).is_err());
    assert!(projector()
        .records(
            &event(json!({"role":"user", "content":"valid neighbor"})),
            2
        )
        .is_ok());
}
