use super::*;

#[test]
fn projector_preserves_sessions_events_order_and_message_dedupe() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn);
    insert_checkpoint(&conn, "thread-a", "checkpoint-a");
    insert_write(
        &conn,
        "thread-a",
        "checkpoint-a",
        "task-a",
        0,
        &message_blob(vec![
            message_value("human", "hello", "message-1"),
            message_value("ai", "world", "message-2"),
        ]),
    );
    insert_write(
        &conn,
        "thread-a",
        "checkpoint-a",
        "task-a",
        1,
        &message_blob(vec![
            message_value("human", "duplicate", "message-1"),
            message_value("tool", "tool output", "message-3"),
        ]),
    );
    let context = context(Some("/tmp/deepagents/sessions.db".into()));
    let batches = produce_all(
        &conn,
        test_source("equivalence"),
        initial_deepagents_position().unwrap(),
        context.clone(),
    );
    let mut projector = DeepAgentsCapturedBatchProjector {
        context,
        raw_source_path: Some("/tmp/deepagents/sessions.db".to_owned()),
        user_version: 0,
        schema_fingerprint: "schema:test".to_owned(),
        source_revision: "deepagents-snapshot:equivalence".to_owned(),
        committed_store: None,
    };
    let mut output = CollectingProjectionOutput::default();
    for batch in &batches {
        for record in batch.records() {
            projector.project_record(record, &mut output).unwrap();
        }
    }
    assert!(output.rejections.is_empty());
    assert_eq!(output.normalizations.len(), 4);
    assert!(output
        .normalizations
        .first()
        .and_then(|normalization| normalization.captures.first())
        .is_some_and(|(_, capture)| capture.event.is_none()));
    let captures = output
        .normalizations
        .into_iter()
        .flat_map(|normalization| normalization.captures)
        .filter(|(_, capture)| capture.event.is_some())
        .collect::<Vec<_>>();
    assert_eq!(captures.len(), 3);
    let result_event = captures[2].1.event.as_ref().unwrap();
    let result_locators = VerifiedContentLocatorsV1::from_metadata_value(
        &result_event.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
    )
    .unwrap();
    let result_locator = result_locators
        .locator(VerifiedContentRole::ResultBody)
        .unwrap();
    assert!(result_locator.content_ref().verifies(b"tool output"));
    assert_eq!(
        serde_json::from_value::<ctx_history_core::ContentRef>(
            result_event.payload["result_content_ref"].clone()
        )
        .unwrap(),
        result_locator.content_ref().clone(),
    );
    let result_address =
        decode_deepagents_content_address(result_locator.source_locator().unwrap().value())
            .unwrap();
    assert_eq!(result_address.thread_id, "thread-a");
    assert_eq!(result_address.checkpoint_id, "checkpoint-a");
    assert_eq!(result_address.task_id, "task-a");
    assert_eq!(result_address.write_idx, 1);
    assert_eq!(result_address.message_offset, 1);
    assert!(!serde_json::to_string(&result_event.metadata)
        .unwrap()
        .contains("tool output"));
    assert_eq!(
        captures
            .iter()
            .map(|(line, capture)| {
                let event = capture.event.as_ref().unwrap();
                json!({
                    "line": line,
                    "session": capture.session.provider_session_id,
                    "index": event.provider_event_index,
                    "hash": event.provider_event_hash,
                    "cursor": event.cursor,
                    "event_type": event.event_type,
                    "role": event.role,
                    "text": event.payload["text"],
                })
            })
            .collect::<Vec<_>>(),
        vec![
            json!({
                "line": provider_line_from_index(2),
                "session": "thread-a",
                "index": 1,
                "hash": deepagents_message_identity("thread-a", "message-1").payload_hash,
                "cursor": "thread:thread-a:checkpoint:checkpoint-a:task:task-a:write:0:message:0",
                "event_type": EventType::Message,
                "role": EventRole::User,
                "text": "hello",
            }),
            json!({
                "line": provider_line_from_index(2),
                "session": "thread-a",
                "index": 2,
                "hash": deepagents_message_identity("thread-a", "message-2").payload_hash,
                "cursor": "thread:thread-a:checkpoint:checkpoint-a:task:task-a:write:0:message:1",
                "event_type": EventType::Message,
                "role": EventRole::Assistant,
                "text": "world",
            }),
            json!({
                "line": provider_line_from_index(3),
                "session": "thread-a",
                "index": 3,
                "hash": deepagents_message_identity("thread-a", "message-3").payload_hash,
                "cursor": "thread:thread-a:checkpoint:checkpoint-a:task:task-a:write:1:message:1",
                "event_type": EventType::ToolOutput,
                "role": EventRole::Tool,
                "text": "",
            }),
        ]
    );
}

#[test]
fn projector_attaches_a_compound_locator_only_to_truncated_message_bodies() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn);
    insert_checkpoint(&conn, "thread-a", "checkpoint-a");
    let long = "message body ".repeat(crate::PROVIDER_MAX_TEXT_CHARS / 8 + 1);
    insert_write(
        &conn,
        "thread-a",
        "checkpoint-a",
        "task-a",
        9,
        &message_blob(vec![message_value("human", &long, "message-long")]),
    );
    let context = context(Some("/tmp/deepagents/sessions.db".into()));
    let batches = produce_all(
        &conn,
        test_source("complete-message"),
        initial_deepagents_position().unwrap(),
        context.clone(),
    );
    let mut projector = DeepAgentsCapturedBatchProjector {
        context,
        raw_source_path: Some("/tmp/deepagents/sessions.db".to_owned()),
        user_version: 0,
        schema_fingerprint: "schema:test".to_owned(),
        source_revision: "deepagents-snapshot:complete-message".to_owned(),
        committed_store: None,
    };
    let mut output = CollectingProjectionOutput::default();
    for record in batches.iter().flat_map(|batch| batch.records()) {
        projector.project_record(record, &mut output).unwrap();
    }
    let event = output
        .normalizations
        .iter()
        .flat_map(|normalization| &normalization.captures)
        .find_map(|(_, capture)| capture.event.as_ref())
        .unwrap();
    assert_eq!(event.payload["text_retention"]["truncated"], true);
    let locators = VerifiedContentLocatorsV1::from_metadata_value(
        &event.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
    )
    .unwrap();
    let locator = locators.locator(VerifiedContentRole::MessageBody).unwrap();
    assert!(locator.content_ref().verifies(long.as_bytes()));
    assert!(locators.locator(VerifiedContentRole::ResultBody).is_none());
    let address =
        decode_deepagents_content_address(locator.source_locator().unwrap().value()).unwrap();
    assert_eq!(address.write_idx, 9);
    assert_eq!(address.message_offset, 0);
    assert!(!serde_json::to_string(&event.metadata)
        .unwrap()
        .contains("message body"));
}

#[test]
fn terminal_record_preserves_a_session_for_noise_only_writes_without_rescanning() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn);
    insert_checkpoint(&conn, "thread-noise", "checkpoint-a");
    insert_write(
        &conn,
        "thread-noise",
        "checkpoint-a",
        "task-a",
        0,
        &message_blob(vec![message_value(
            "system",
            "internal system prompt",
            "noise-1",
        )]),
    );
    let context = context(Some("/tmp/deepagents/noise-only.db".into()));
    let batches = produce_all(
        &conn,
        test_source("noise-only"),
        initial_deepagents_position().unwrap(),
        context.clone(),
    );
    let mut projector = DeepAgentsCapturedBatchProjector {
        context,
        raw_source_path: Some("/tmp/deepagents/noise-only.db".to_owned()),
        user_version: 0,
        schema_fingerprint: "schema:test".to_owned(),
        source_revision: "deepagents-snapshot:noise-only".to_owned(),
        committed_store: None,
    };
    let mut output = CollectingProjectionOutput::default();
    for batch in &batches {
        for record in batch.records() {
            projector.project_record(record, &mut output).unwrap();
        }
    }

    assert!(output.rejections.is_empty());
    let captures = output
        .normalizations
        .into_iter()
        .flat_map(|normalization| normalization.captures)
        .collect::<Vec<_>>();
    assert_eq!(captures.len(), 1);
    assert_eq!(captures[0].1.session.provider_session_id, "thread-noise");
    assert!(captures[0].1.event.is_none());
}
