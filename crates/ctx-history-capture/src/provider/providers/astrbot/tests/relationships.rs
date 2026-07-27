use super::*;

#[test]
fn astrbot_large_relationship_restart_keeps_checkpoint_fixed_and_parents_equivalent() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn);
    let conversation_count = 4_096_i64;
    let mut legacy_conversation_rows = BTreeMap::new();
    let mut legacy_checkpoint_sessions = BTreeMap::new();
    let mut linked_checkpoint = String::new();
    conn.execute_batch("begin").unwrap();
    for id in 1..=conversation_count {
        let session_id = format!("session-{id:05}-{}", "s".repeat(32));
        let checkpoint_id = format!("checkpoint-{id:05}-{}", "c".repeat(32));
        insert_conversation(
            &conn,
            id,
            &session_id,
            &json!([
                {"type": "_checkpoint", "id": checkpoint_id},
                {"role": "user", "content": format!("conversation {id}")},
            ])
            .to_string(),
        );
        legacy_conversation_rows.insert(session_id.clone(), id);
        legacy_checkpoint_sessions.insert(checkpoint_id.clone(), session_id);
        linked_checkpoint = checkpoint_id;
    }
    for id in 1..=65_i64 {
        insert_platform_message(
            &conn,
            id,
            Some(&linked_checkpoint),
            &format!("platform message {id}"),
        );
    }
    conn.execute_batch("commit").unwrap();

    let legacy_checkpoint = serde_json::to_vec(&LegacyCheckpointFixture {
        schema_version: ASTRBOT_CHECKPOINT_SCHEMA_VERSION,
        source_shape_validated: true,
        conversation_rows: &legacy_conversation_rows,
        checkpoint_sessions: &legacy_checkpoint_sessions,
    })
    .unwrap();
    assert!(legacy_checkpoint.len() > CAPTURE_BATCH_MAX_PARSER_CHECKPOINT_BYTES);
    let legacy_checkpoint_state: AstrBotParserCheckpoint =
        serde_json::from_slice(&legacy_checkpoint).unwrap();
    legacy_checkpoint_state.validate().unwrap();
    assert!(legacy_checkpoint_state.source_shape_validated);

    let mut checkpoint = AstrBotParserCheckpoint::empty();
    checkpoint.source_shape_validated = true;
    let source = test_source("restart");
    let resume_position = encode_astrbot_position(AstrBotKeyset {
        phase: AstrBotPhase::PlatformMessages,
        next_ordinal: u64::try_from(conversation_count).unwrap() + 64,
        physical_rowid: 64,
    })
    .unwrap();
    let certified = CertifiedProviderCursor::new(
        source.source_revision(),
        source.capture_revision(),
        source.policy_revision(),
        resume_position.clone(),
        BoundedParserCheckpoint::from_serializable(&checkpoint).unwrap(),
    )
    .unwrap();
    assert!(certified.parser_checkpoint().as_bytes().len() < 128);
    let checkpoint: AstrBotParserCheckpoint = certified.parser_checkpoint().deserialize().unwrap();

    let sql = AstrBotSql::new(&conn).unwrap();
    assert!(astrbot_relationship_projection_needed(&conn, &sql, &resume_position).unwrap());
    assert!(!astrbot_relationship_projection_exists(&conn).unwrap());
    astrbot_reset_relationship_projection_test_pacing();
    astrbot_prepare_relationship_projection(&conn, &sql).unwrap();
    assert!(astrbot_relationship_projection_exists(&conn).unwrap());
    let pacing = astrbot_relationship_projection_test_pacing();
    assert!(pacing.pages > 1);
    assert!(pacing.max_source_rows <= ASTRBOT_RELATIONSHIP_PROJECTION_MAX_SOURCE_ROWS_PER_PAGE);
    assert!(
        pacing.max_retained_bytes <= ASTRBOT_RELATIONSHIP_PROJECTION_MAX_RETAINED_BYTES_PER_PAGE
    );
    assert!(pacing.max_temp_writes <= ASTRBOT_RELATIONSHIP_PROJECTION_MAX_TEMP_WRITES_PER_PAGE);
    assert_eq!(
        astrbot_relationship_projection_test_wait_count(),
        pacing.pages
    );
    astrbot_disable_relationship_projection_test_wait_hook();
    let temp_store: i64 = conn
        .pragma_query_value(None, "temp_store", |row| row.get(0))
        .unwrap();
    assert_eq!(temp_store, 1);

    let mut fetcher = AstrBotRowFetcher::new(&conn, sql, checkpoint).unwrap();
    let mut producer =
        SqliteLogicalRowBatchProducer::new(source, resume_position, move |position| {
            fetcher.fetch(position)
        });
    let batch = producer.next_batch().unwrap().unwrap();
    assert_eq!(batch.records().len(), 1);
    let CapturedRecordPayload::SqliteValues(values) = batch.records()[0].payload() else {
        panic!("resumed AstrBot platform message must retain SQLite values");
    };
    let (message, link) = decode_astrbot_platform_message(values).unwrap();
    assert_eq!(message.id, 65);
    let expected_session = legacy_checkpoint_sessions.get(&linked_checkpoint).unwrap();
    assert_eq!(
        link.as_ref().map(|link| link.provider_session_id.as_str()),
        Some(expected_session.as_str())
    );

    let mut projector = AstrBotCapturedBatchProjector {
        context: context(None),
        raw_source_path: "astrbot-restart.db".to_owned(),
        user_version: 0,
        schema_fingerprint: "astrbot-restart-schema".to_owned(),
        selected_conversation: None,
        parser_checkpoint: checkpoint,
    };
    let mut output = CollectingProjectionOutput::default();
    for record in batch.records() {
        projector.project_record(record, &mut output).unwrap();
    }
    assert_eq!(
        output.normalization.captures[0]
            .1
            .session
            .provider_session_id,
        expected_session.as_str()
    );
    let CapturedBatchCursorFinish::Advance(finished) = projector.finish_cursor(&batch).unwrap()
    else {
        panic!("AstrBot projector unexpectedly retained the prior cursor");
    };
    assert_eq!(
        finished.parser_checkpoint().as_bytes(),
        certified.parser_checkpoint().as_bytes()
    );
}

#[test]
fn astrbot_relationship_projection_pages_by_retained_bytes() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn);
    let large_text = "x".repeat(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES / 2);
    for id in 1..=2_i64 {
        insert_conversation(
            &conn,
            id,
            &format!("session-{id}"),
            &json!([
                {"type": "_checkpoint", "id": format!("checkpoint-{id}")},
                {"role": "user", "content": large_text.as_str()},
            ])
            .to_string(),
        );
    }
    let sql = AstrBotSql::new(&conn).unwrap();

    astrbot_reset_relationship_projection_test_pacing();
    astrbot_prepare_relationship_projection(&conn, &sql).unwrap();
    let pacing = astrbot_relationship_projection_test_pacing();
    assert_eq!(pacing.pages, 2);
    assert_eq!(pacing.max_source_rows, 1);
    assert!(
        pacing.max_retained_bytes <= ASTRBOT_RELATIONSHIP_PROJECTION_MAX_RETAINED_BYTES_PER_PAGE
    );
    assert!(pacing.max_temp_writes <= ASTRBOT_RELATIONSHIP_PROJECTION_MAX_TEMP_WRITES_PER_PAGE);
    assert_eq!(
        astrbot_relationship_projection_test_wait_count(),
        pacing.pages
    );
    astrbot_disable_relationship_projection_test_wait_hook();
}

#[test]
fn astrbot_relationship_projection_restores_query_only_and_cleans_failed_prep() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn);
    insert_conversation(
        &conn,
        1,
        "session-success",
        &json!([{"type": "_checkpoint", "id": "checkpoint-success"}]).to_string(),
    );
    let sql = AstrBotSql::new(&conn).unwrap();
    conn.pragma_update(None, "query_only", true).unwrap();
    astrbot_prepare_relationship_projection(&conn, &sql).unwrap();
    let query_only = conn
        .pragma_query_value(None, "query_only", |row| row.get::<_, i64>(0))
        .unwrap();
    assert_eq!(query_only, 1);
    assert!(astrbot_relationship_projection_exists(&conn).unwrap());

    let failed = Connection::open_in_memory().unwrap();
    create_tables(&failed);
    insert_conversation(
        &failed,
        1,
        "session-failure",
        &json!([
            {"type": "_checkpoint", "id": "checkpoint-failure"},
            {"role": "user", "content": "x".repeat(2 * 1_024)},
        ])
        .to_string(),
    );
    let failed_sql = AstrBotSql::new(&failed).unwrap();
    failed.set_limit(Limit::SQLITE_LIMIT_LENGTH, 1_024);
    failed.pragma_update(None, "query_only", true).unwrap();
    let error = astrbot_prepare_relationship_projection(&failed, &failed_sql).unwrap_err();
    assert!(matches!(error, CaptureError::Sqlite(_)), "{error:?}");
    let query_only = failed
        .pragma_query_value(None, "query_only", |row| row.get::<_, i64>(0))
        .unwrap();
    assert_eq!(query_only, 1);
    let remaining_temp_tables: i64 = failed
        .query_row(
            "select count(*) from sqlite_temp_master \
             where type = 'table' and name in (?1, ?2)",
            [
                ASTRBOT_CONVERSATION_SESSIONS_TEMP_TABLE,
                ASTRBOT_CHECKPOINT_SESSIONS_TEMP_TABLE,
            ],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(remaining_temp_tables, 0);
}

#[test]
fn astrbot_relationship_projection_normalizes_long_high_fanout_sessions() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn);
    let checkpoint_count = 1_024_usize;
    let long_session = format!("session-long-{}", "s".repeat(64 * 1_024));
    let mut checkpoints = (0..checkpoint_count)
        .map(|index| {
            json!({
                "type": "_checkpoint",
                "id": format!("checkpoint-{index:04}"),
            })
        })
        .collect::<Vec<_>>();
    checkpoints.push(json!({
        "type": "_checkpoint",
        "id": "checkpoint-duplicate",
    }));
    insert_conversation(
        &conn,
        1,
        &long_session,
        &Value::Array(checkpoints).to_string(),
    );
    insert_conversation(
        &conn,
        2,
        &long_session,
        &json!([{"type": "_checkpoint", "id": "checkpoint-latest"}]).to_string(),
    );
    insert_conversation(
        &conn,
        3,
        "session-replacement",
        &json!([{"type": "_checkpoint", "id": "checkpoint-duplicate"}]).to_string(),
    );
    let sql = AstrBotSql::new(&conn).unwrap();

    astrbot_reset_relationship_projection_test_pacing();
    astrbot_prepare_relationship_projection(&conn, &sql).unwrap();
    let pacing = astrbot_relationship_projection_test_pacing();
    assert_eq!(pacing.total_temp_writes, checkpoint_count + 6);
    assert!(pacing.max_source_rows <= ASTRBOT_RELATIONSHIP_PROJECTION_MAX_SOURCE_ROWS_PER_PAGE);
    assert!(
        pacing.max_retained_bytes <= ASTRBOT_RELATIONSHIP_PROJECTION_MAX_RETAINED_BYTES_PER_PAGE
    );
    assert!(pacing.max_temp_writes <= ASTRBOT_RELATIONSHIP_PROJECTION_MAX_TEMP_WRITES_PER_PAGE);
    assert!(
        pacing.pages
            >= pacing
                .total_temp_writes
                .div_ceil(ASTRBOT_RELATIONSHIP_PROJECTION_MAX_TEMP_WRITES_PER_PAGE)
    );
    assert_eq!(
        astrbot_relationship_projection_test_wait_count(),
        pacing.pages
    );
    astrbot_disable_relationship_projection_test_wait_hook();

    let session_rows: i64 = conn
        .query_row(
            "select count(*) from temp.astrbot_conversation_sessions",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let checkpoint_rows: i64 = conn
        .query_row(
            "select count(*) from temp.astrbot_checkpoint_sessions",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(session_rows, 2);
    assert_eq!(
        checkpoint_rows,
        i64::try_from(checkpoint_count + 2).unwrap()
    );
    let (stable_session_key, latest_source_rowid) = conn
        .query_row(
            "select session_key, source_rowid \
             from temp.astrbot_conversation_sessions \
             where provider_session_id = ?1",
            [&long_session],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .unwrap();
    assert_eq!(stable_session_key, 1);
    assert_eq!(latest_source_rowid, 2);
    let linked_latest_source_rowid: i64 = conn
        .query_row(
            "select s.source_rowid \
             from temp.astrbot_checkpoint_sessions c \
             join temp.astrbot_conversation_sessions s \
               on s.session_key = c.session_key \
             where c.checkpoint_id = ?1",
            ["checkpoint-0000"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(linked_latest_source_rowid, 2);
    let duplicate_source_rowid: i64 = conn
        .query_row(
            "select s.source_rowid \
             from temp.astrbot_checkpoint_sessions c \
             join temp.astrbot_conversation_sessions s \
               on s.session_key = c.session_key \
             where c.checkpoint_id = 'checkpoint-duplicate'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(duplicate_source_rowid, 3);

    let checkpoint_columns = conn
        .prepare("pragma temp.table_info(astrbot_checkpoint_sessions)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        checkpoint_columns,
        vec!["checkpoint_id".to_owned(), "session_key".to_owned()]
    );
    let stored_session_text_bytes: i64 = conn
        .query_row(
            "select sum(length(cast(provider_session_id as blob))) \
             from temp.astrbot_conversation_sessions",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        stored_session_text_bytes,
        i64::try_from(long_session.len() + "session-replacement".len()).unwrap()
    );

    let temp_page_count: u64 = conn
        .query_row("pragma temp.page_count", [], |row| row.get(0))
        .unwrap();
    let temp_page_size: u64 = conn
        .query_row("pragma temp.page_size", [], |row| row.get(0))
        .unwrap();
    let temp_bytes = temp_page_count.saturating_mul(temp_page_size);
    let repeated_session_bytes = u64::try_from(long_session.len())
        .unwrap()
        .saturating_mul(u64::try_from(checkpoint_count).unwrap());
    assert!(temp_bytes < repeated_session_bytes / 4);
}

#[test]
fn astrbot_relationship_decision_reads_one_oversize_frontier_row() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn);
    let oversize = i64::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES)
        .unwrap()
        .checked_add(1)
        .unwrap();
    conn.execute(
        "insert into platform_message_history (id, content, created_at) \
         values (1, zeroblob(?1), 1)",
        [oversize],
    )
    .unwrap();
    conn.execute_batch("begin").unwrap();
    for id in 2..=2_048_i64 {
        insert_platform_message(&conn, id, None, &format!("unlinked message {id}"));
    }
    conn.execute_batch("commit").unwrap();
    let sql = AstrBotSql::new(&conn).unwrap();
    let capped_length = i32::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES).unwrap();
    conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, capped_length);
    let operations = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&operations);
    conn.progress_handler(
        1,
        Some(move || {
            observed.fetch_add(1, Ordering::Relaxed);
            false
        }),
    );
    assert!(astrbot_relationship_projection_needed(
        &conn,
        &sql,
        &initial_astrbot_position().unwrap()
    )
    .unwrap());
    conn.progress_handler(0, None::<fn() -> bool>);
    assert_eq!(conn.limit(Limit::SQLITE_LIMIT_LENGTH), capped_length);
    assert!(
        operations.load(Ordering::Relaxed) < 2_000,
        "AstrBot relationship setup decision scanned beyond its single keyset row"
    );
}
