use super::*;

#[test]
fn astrbot_checkpoint_only_parent_authorizes_linked_child_event() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let path = directory.path().join("data_v4.db");
    let conn = Connection::open(&path).unwrap();
    create_tables(&conn);
    insert_conversation(
        &conn,
        1,
        "session-linked",
        &json!([{"type": "_checkpoint", "id": "checkpoint-linked"}]).to_string(),
    );
    insert_platform_message(&conn, 1, Some("checkpoint-linked"), "linked child event");
    drop(conn);

    let adapter_context = context(Some(path.clone()));
    let mut store = Store::open(directory.path().join("store.sqlite")).unwrap();
    astrbot_reset_relationship_projection_test_pacing();
    let summary = import_astrbot_sqlite_batched(
        &path,
        &mut store,
        adapter_context,
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    astrbot_disable_relationship_projection_test_wait_hook();
    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);

    let session = store
        .session_by_external_session(CaptureProvider::AstrBot, "session-linked")
        .unwrap()
        .unwrap();
    assert_eq!(session.role_hint.as_deref(), Some("llm-context"));
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 1);
}

#[test]
fn astrbot_projector_preserves_expected_order_links_and_metadata() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let path = directory.path().join("data_v4.db");
    let conn = Connection::open(&path).unwrap();
    create_tables(&conn);
    insert_conversation(
        &conn,
        1,
        "session-linked",
        &json!([
            {"type": "_checkpoint", "id": "checkpoint-linked"},
            {"role": "user", "id": "conversation-message", "content": "hello AstrBot"},
        ])
        .to_string(),
    );
    insert_conversation(&conn, 2, "session-plain", "plain conversation content");
    insert_platform_message(
        &conn,
        1,
        Some("checkpoint-linked"),
        "linked platform message",
    );
    insert_platform_message(&conn, 2, None, "unlinked platform message");
    conn.execute(
        "insert into preferences (key, value, scope) \
         values ('sel_conv_id', '{\"val\":\"session-linked\"}', 'umo')",
        [],
    )
    .unwrap();
    drop(conn);
    let adapter_context = context(Some(path.clone()));
    let conn = open_provider_sqlite_readonly(&path).unwrap();
    let batches = produce_all(
        &conn,
        test_source("equivalence"),
        initial_astrbot_position().unwrap(),
    );
    let mut projector = AstrBotCapturedBatchProjector {
        context: adapter_context,
        raw_source_path: path.display().to_string(),
        user_version: conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap(),
        schema_fingerprint: sqlite_schema_fingerprint(&conn).unwrap(),
        selected_conversation: astrbot_selected_conversation_bounded(&conn).unwrap(),
        parser_checkpoint: {
            let mut checkpoint = AstrBotParserCheckpoint::empty();
            checkpoint.source_shape_validated = true;
            checkpoint
        },
    };
    let mut output = CollectingProjectionOutput::default();
    for batch in batches {
        for record in batch.records() {
            projector.project_record(record, &mut output).unwrap();
        }
    }

    assert_eq!(
        output.normalization.summary,
        ProviderImportSummary::default()
    );
    assert!(output.normalization.files_touched.is_empty());
    assert_eq!(output.normalization.captures.len(), 4);

    let captures = output
        .normalization
        .captures
        .iter()
        .map(|(_, capture)| capture)
        .collect::<Vec<_>>();
    let expected_sessions = [
        "session-linked",
        "session-plain",
        "session-linked",
        "platform/platform-test/user-test",
    ];
    let expected_text = [
        "hello AstrBot",
        "plain conversation content",
        "linked platform message",
        "unlinked platform message",
    ];
    for ((capture, expected_session), expected_text) in
        captures.iter().zip(expected_sessions).zip(expected_text)
    {
        assert_eq!(capture.session.provider_session_id, expected_session);
        assert_eq!(
            capture.event.as_ref().unwrap().payload["text"],
            expected_text
        );
        assert_eq!(capture.source.raw_source_path.as_deref(), path.to_str());
    }
    assert_eq!(
        captures[0].event.as_ref().unwrap().role,
        Some(EventRole::User)
    );
    assert_eq!(captures[1].event.as_ref().unwrap().role, None);
    assert_eq!(
        captures[2].event.as_ref().unwrap().role,
        Some(EventRole::Assistant)
    );
    assert_eq!(
        captures[3].event.as_ref().unwrap().role,
        Some(EventRole::User)
    );
    assert_eq!(
        captures[0].session.metadata["selected_conversation"],
        "session-linked"
    );
    assert_eq!(
        captures[2].event.as_ref().unwrap().metadata["source"],
        "astrbot_platform_message_history"
    );
    assert_eq!(
        captures[3].session.metadata["fidelity_gap"],
        "platform history row was not linked to a conversations checkpoint"
    );
}
