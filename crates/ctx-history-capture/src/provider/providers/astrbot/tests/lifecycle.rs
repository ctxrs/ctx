use super::*;

#[test]
fn astrbot_real_import_resumes_legacy_cursor_and_terminal_reopen_skips_projection() {
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
    for id in 1..=65_i64 {
        insert_platform_message(
            &conn,
            id,
            Some("checkpoint-linked"),
            &format!("platform message {id}"),
        );
    }
    drop(conn);

    let (source, stream) = import_source(&path);
    let resume_position = encode_astrbot_position(AstrBotKeyset {
        phase: AstrBotPhase::PlatformMessages,
        next_ordinal: 65,
        physical_rowid: 64,
    })
    .unwrap();
    let conversation_rows = BTreeMap::from([("session-linked".to_owned(), 1_i64)]);
    let checkpoint_sessions =
        BTreeMap::from([("checkpoint-linked".to_owned(), "session-linked".to_owned())]);
    let legacy_checkpoint = BoundedParserCheckpoint::from_serializable(&LegacyCheckpointFixture {
        schema_version: ASTRBOT_CHECKPOINT_SCHEMA_VERSION,
        source_shape_validated: true,
        conversation_rows: &conversation_rows,
        checkpoint_sessions: &checkpoint_sessions,
    })
    .unwrap();
    assert!(legacy_checkpoint.as_bytes().len() < CAPTURE_BATCH_MAX_PARSER_CHECKPOINT_BYTES);
    let legacy_checkpoint_bytes = legacy_checkpoint.as_bytes().len();
    let legacy_cursor = CertifiedProviderCursor::new(
        source.source_revision(),
        source.capture_revision(),
        source.policy_revision(),
        resume_position,
        legacy_checkpoint,
    )
    .unwrap();
    let adapter_context = context(Some(path.clone()));
    let mut store = Store::open(directory.path().join("store.sqlite")).unwrap();
    let conn = open_provider_sqlite_readonly(&path).unwrap();
    let sql = AstrBotSql::new(&conn).unwrap();
    let conversation = astrbot_hydrate_conversation(&conn, &sql.conversation_hydration, 1).unwrap();
    let parent_started_at =
        provider_timestamp_millis(conversation.created_at, adapter_context.imported_at);
    let parent_capture = astrbot_capture(
        AstrBotCaptureDraft {
            conversation: &conversation,
            provider_session_id: "session-linked",
            started_at: parent_started_at,
            ended_at: conversation.updated_at.map(|timestamp| {
                provider_timestamp_millis(Some(timestamp), adapter_context.imported_at)
            }),
            path: &path,
            user_version: conn
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .unwrap(),
            schema_fingerprint: &sqlite_schema_fingerprint(&conn).unwrap(),
            selected_conversation: None,
            event: None,
        },
        &adapter_context,
    );
    let seeded_parent = import_provider_capture_line(
        &mut store,
        &parent_capture,
        &NormalizedProviderImportOptions::default(),
        1,
        &mut ProviderImportCaches::default(),
    )
    .unwrap();
    assert_eq!(seeded_parent.imported_sessions, 1);
    drop(conn);
    let seeded = seed_certified_cursor(&store, &adapter_context, &stream, &legacy_cursor);

    astrbot_reset_relationship_projection_test_pacing();
    let summary = import_astrbot_sqlite_batched(
        &path,
        &mut store,
        adapter_context.clone(),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_events, 1);
    assert_eq!(astrbot_relationship_projection_test_prepare_count(), 1);
    astrbot_disable_relationship_projection_test_wait_hook();

    let session = store
        .session_by_external_session(CaptureProvider::AstrBot, "session-linked")
        .unwrap()
        .unwrap();
    assert_eq!(session.role_hint.as_deref(), Some("llm-context"));
    let events = store.events_for_session(session.id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].role, Some(EventRole::Assistant));
    assert!(events[0]
        .payload
        .to_string()
        .contains("platform message 65"));

    let published = store
        .get_sync_cursor(None, &adapter_context.machine_id, &stream)
        .unwrap()
        .unwrap();
    assert_eq!(published.id, seeded.id);
    assert_eq!(
        published.timestamps.created_at,
        seeded.timestamps.created_at
    );
    assert_ne!(published.cursor, seeded.cursor);
    let compact = CertifiedProviderCursor::decode(&published.cursor).unwrap();
    let terminal = decode_astrbot_position(compact.native_position())
        .unwrap()
        .unwrap();
    assert_eq!(terminal.phase, AstrBotPhase::PlatformMessages);
    assert_eq!(terminal.next_ordinal, 66);
    assert_eq!(terminal.physical_rowid, 65);
    assert!(compact.parser_checkpoint().as_bytes().len() < 128);
    assert!(compact.parser_checkpoint().as_bytes().len() < legacy_checkpoint_bytes);

    astrbot_reset_relationship_projection_test_pacing();
    let replay = import_astrbot_sqlite_batched(
        &path,
        &mut store,
        adapter_context.clone(),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(replay, ProviderImportSummary::default());
    assert_eq!(astrbot_relationship_projection_test_prepare_count(), 0);
    assert_eq!(
        astrbot_relationship_projection_test_pacing(),
        AstrBotRelationshipProjectionTestPacing::default()
    );
    assert_eq!(
        store
            .get_sync_cursor(None, &adapter_context.machine_id, &stream)
            .unwrap()
            .unwrap(),
        published
    );
    astrbot_disable_relationship_projection_test_wait_hook();
}

#[test]
fn astrbot_terminal_restart_does_not_build_relationship_projection() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn);
    insert_conversation(
        &conn,
        1,
        "session-linked",
        &json!([{"type": "_checkpoint", "id": "checkpoint-linked"}]).to_string(),
    );
    insert_platform_message(
        &conn,
        1,
        Some("checkpoint-linked"),
        "linked platform message",
    );
    let terminal_position = encode_astrbot_position(AstrBotKeyset {
        phase: AstrBotPhase::PlatformMessages,
        next_ordinal: 2,
        physical_rowid: 1,
    })
    .unwrap();
    let sql = AstrBotSql::new(&conn).unwrap();
    let mut checkpoint = AstrBotParserCheckpoint::empty();
    checkpoint.source_shape_validated = true;

    assert!(!astrbot_relationship_projection_needed(&conn, &sql, &terminal_position).unwrap());
    assert!(!astrbot_relationship_projection_exists(&conn).unwrap());
    let mut fetcher = AstrBotRowFetcher::new(&conn, sql, checkpoint).unwrap();
    assert!(fetcher.fetch(terminal_position).unwrap().is_none());
    assert!(!astrbot_relationship_projection_exists(&conn).unwrap());
}

#[test]
fn astrbot_source_snapshot_detects_database_mutation() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let path = directory.path().join("data_v4.db");
    fs::write(&path, b"astrbot-snapshot").unwrap();
    let snapshot = astrbot_source_snapshot(&path).unwrap();
    assert!(snapshot.revalidate(&path).unwrap());

    fs::write(&path, b"astrbot-snapshot-changed").unwrap();
    assert!(!snapshot.revalidate(&path).unwrap());
}

#[test]
fn astrbot_real_import_detects_mutation_after_released_projection_page() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let path = directory.path().join("data_v4.db");
    let conn = Connection::open(&path).unwrap();
    create_tables(&conn);
    for id in 1..=65_i64 {
        let content = if id == 65 {
            json!([{"type": "_checkpoint", "id": "checkpoint-linked"}]).to_string()
        } else {
            json!([{"role": "user", "content": format!("conversation {id}")}]).to_string()
        };
        insert_conversation(&conn, id, &format!("session-{id}"), &content);
    }
    insert_platform_message(&conn, 1, Some("checkpoint-linked"), "linked after mutation");
    drop(conn);

    let (source, stream) = import_source(&path);
    let start_position = encode_astrbot_position(AstrBotKeyset {
        phase: AstrBotPhase::PlatformMessages,
        next_ordinal: 65,
        physical_rowid: 0,
    })
    .unwrap();
    let mut checkpoint = AstrBotParserCheckpoint::empty();
    checkpoint.source_shape_validated = true;
    let cursor = CertifiedProviderCursor::new(
        source.source_revision(),
        source.capture_revision(),
        source.policy_revision(),
        start_position,
        BoundedParserCheckpoint::from_serializable(&checkpoint).unwrap(),
    )
    .unwrap();
    let adapter_context = context(Some(path.clone()));
    let mut store = Store::open(directory.path().join("store.sqlite")).unwrap();
    let seeded = seed_certified_cursor(&store, &adapter_context, &stream, &cursor);
    let hook_ran = std::rc::Rc::new(Cell::new(false));
    let observed_hook = std::rc::Rc::clone(&hook_ran);
    let mutation_path = path.clone();

    astrbot_reset_relationship_projection_test_pacing();
    astrbot_set_relationship_projection_test_release_hook(move || {
        let writer = Connection::open(mutation_path).unwrap();
        writer
            .execute(
                "insert into preferences (key, value, scope) values ('mutation', ?1, 'test')",
                ["m".repeat(32 * 1_024)],
            )
            .unwrap();
        observed_hook.set(true);
    });
    let error = import_astrbot_sqlite_batched(
        &path,
        &mut store,
        adapter_context.clone(),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(error, CaptureError::SourceChangedDuringCapture));
    assert!(hook_ran.get());
    assert_eq!(astrbot_relationship_projection_test_prepare_count(), 1);
    assert!(astrbot_relationship_projection_test_pacing().pages >= 1);
    astrbot_disable_relationship_projection_test_wait_hook();

    assert_eq!(
        store
            .get_sync_cursor(None, &adapter_context.machine_id, &stream)
            .unwrap()
            .unwrap(),
        seeded
    );
    assert!(store
        .session_by_external_session(CaptureProvider::AstrBot, "session-65")
        .unwrap()
        .is_none());
}
