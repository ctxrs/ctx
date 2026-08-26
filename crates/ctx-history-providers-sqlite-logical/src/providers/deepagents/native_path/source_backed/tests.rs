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
fn root_scope_separates_identical_deepagents_conversations_and_unqualified_is_released() {
    use ctx_history_core::{CaptureProvider, SourceAnchor, SourceAnchorScope, SourceKey, TypedKey};

    let released = SourceKey::derive(
        CaptureProvider::DeepAgents.as_str(),
        super::DEEPAGENTS_SQLITE_SOURCE_FORMAT,
        super::DEEPAGENTS_SOURCE_SCHEMA_VARIANT,
        1,
        SourceAnchor::provider_native(
            super::DEEPAGENTS_SOURCE_ANCHOR_NAMESPACE,
            TypedKey::utf8(super::DEEPAGENTS_SOURCE_ANCHOR_KEY).unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let unqualified = super::deepagents_source_key_scoped(SourceAnchorScope::Unqualified).unwrap();
    assert!(released.exact_descriptor_eq(&unqualified));
    assert_eq!(
        released.identity().encode_canonical().unwrap(),
        unqualified.identity().encode_canonical().unwrap()
    );

    let first =
        super::deepagents_source_key_scoped(SourceAnchorScope::Lineage([0x11; 32])).unwrap();
    let second =
        super::deepagents_source_key_scoped(SourceAnchorScope::Lineage([0x22; 32])).unwrap();
    assert_ne!(first.identity(), second.identity());
    assert_ne!(
        super::deepagents_session_id(&first, "shared-thread").unwrap(),
        super::deepagents_session_id(&second, "shared-thread").unwrap()
    );
}

#[test]
fn core_projection_keeps_complete_success_failure_unknown_and_large_results_once() {
    use chrono::{TimeZone, Utc};
    use ctx_history_core::{AgentScope, EventRole};

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
        assert_eq!(record.agent_scope, Some(AgentScope::Primary));
        assert_eq!(record.session_relationship, None);
        assert_eq!(record.root_session_id, None);
        assert_eq!(record.content.meaningful_text(), body);
        let expected_call_id = format!("call-{index}");
        if index == 0 {
            assert!(record.content.structured_content.is_none());
            assert!(matches!(
                record
                    .content
                    .activity
                    .as_ref()
                    .and_then(|activity| activity.result.as_ref())
                    .map(|result| &result.structured_content),
                Some(ctx_history_core::ActivityJsonCapture::Omitted { .. })
            ));
            continue;
        }
        let structured = record.content.structured_content.as_ref().unwrap();
        assert_eq!(
            structured
                .pointer("/tool_call_id")
                .and_then(serde_json::Value::as_str),
            Some(expected_call_id.as_str())
        );
        assert!(structured.to_string().contains(&body));
    }
}

#[test]
fn empty_optional_session_facts_do_not_reject_or_create_an_empty_activity() {
    use chrono::{TimeZone, Utc};
    use ctx_history_core::EventRole;

    let source = super::deepagents_source_key().unwrap();
    let key = super::DeepAgentsWriteKey {
        thread_id: "thread-empty-facts".to_owned(),
        checkpoint_id: "checkpoint".to_owned(),
        task_id: "task".to_owned(),
        idx: 0,
    };
    let session_id = super::deepagents_session_id(&source, &key.thread_id).unwrap();
    let message = super::DeepAgentsMessage {
        role: EventRole::User,
        message_type: "human".to_owned(),
        message_class: Some("HumanMessage".to_owned()),
        message_id: Some("message-empty-facts".to_owned()),
        tool_call_id: None,
        status: None,
        exit_code: None,
        duration_ms: None,
        timed_out: false,
        is_error: None,
        success: None,
        text: "exact DeepAgents body".to_owned(),
    };
    let record = super::deepagents_core_record(
        &source,
        &key,
        session_id,
        1,
        0,
        Utc.timestamp_millis_opt(1).unwrap(),
        Some(""),
        Some(""),
        &message,
    )
    .unwrap();

    assert_eq!(record.content.meaningful_text(), "exact DeepAgents body");
    assert!(record.content.activity.is_none());
    record.validate_contract().unwrap();
}
