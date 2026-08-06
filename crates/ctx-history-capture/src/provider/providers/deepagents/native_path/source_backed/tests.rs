#[test]
fn direct_core_projection_is_complete_and_self_contained() {
    let sources = [include_str!("../source_backed.rs")];
    let production = sources.join("\n");
    assert!(production.contains("CoreRecord::new_selected"));
    assert!(production.contains("native_event_id = Some"));
    assert!(production.contains("DEEPAGENTS_SOURCE_PARSER_REVISION"));
    assert!(production.contains("validate_contract"));
    assert!(production.contains("message.text.clone()"));
    for removed_api in [
        concat!("Lexical", "Document"),
        concat!("SourceRecord", "Locator"),
        concat!("hyd", "rate_"),
        concat!("resol", "ver"),
    ] {
        assert!(!production.contains(removed_api), "found {removed_api}");
    }
    assert!(!production.contains("body.truncate"));
    assert!(!production.contains("body.chars().take"));
}

#[test]
fn core_projection_keeps_complete_success_failure_unknown_and_large_results_once() {
    use chrono::{TimeZone, Utc};
    use ctx_history_core::EventRole;

    let source = super::deepagents_source_key().unwrap();
    let key = super::DeepAgentsWriteKey {
        thread_id: "thread".to_owned(),
        checkpoint_id: "checkpoint".to_owned(),
        task_id: "task".to_owned(),
        idx: 0,
    };
    let session_id = super::deepagents_session_id(&source, &key.thread_id).unwrap();
    for (index, (status, expected)) in [
        (Some("success"), "success"),
        (Some("failed"), "failure"),
        (None, "unknown"),
    ]
    .into_iter()
    .enumerate()
    {
        let body = if index == 0 {
            format!(
                "deepagents-large-head-{}-deepagents-large-tail",
                "x".repeat(8 * 1024 * 1024)
            )
        } else {
            format!("{expected} complete native result")
        };
        let message = super::DeepAgentsMessage {
            role: EventRole::Tool,
            message_type: "tool".to_owned(),
            message_class: Some("ToolMessage".to_owned()),
            message_id: Some(format!("message-{index}")),
            tool_call_id: Some(format!("call-{index}")),
            status: status.map(str::to_owned),
            exit_code: None,
            duration_ms: Some(10),
            timed_out: false,
            is_error: None,
            success: None,
            text: body.clone(),
        };
        let record = super::deepagents_core_record(
            &source,
            &key,
            session_id,
            u64::try_from(index + 1).unwrap(),
            index,
            Utc.timestamp_millis_opt(1).unwrap(),
            None,
            None,
            &message,
        )
        .unwrap();
        assert_eq!(
            record.session_relationship,
            ctx_history_core::SessionRelationshipKind::Root
        );
        assert_eq!(record.event_origin, ctx_history_core::EventOrigin::Unknown);
        assert!(record.is_primary);
        assert_eq!(record.content.meaningful_text(), body);
        let structured = record.content.structured_content.as_ref().unwrap();
        assert_eq!(
            structured
                .pointer("/provider_native_result/result_outcome")
                .and_then(serde_json::Value::as_str),
            Some(expected)
        );
        let expected_call_id = format!("call-{index}");
        assert_eq!(
            structured
                .pointer("/provider_native_result/call_id")
                .and_then(serde_json::Value::as_str),
            Some(expected_call_id.as_str())
        );
        assert!(!structured.to_string().contains("complete native result"));
        assert!(!structured.to_string().contains("deepagents-large-head-"));
    }
}
