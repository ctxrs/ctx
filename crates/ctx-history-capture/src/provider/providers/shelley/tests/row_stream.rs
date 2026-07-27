use super::*;

#[test]
fn shelley_batches_split_at_sixty_four_and_resume_exactly_across_phases() {
    let conn = Connection::open_in_memory().unwrap();
    create_shelley_tables(&conn);
    insert_conversation(&conn, "messages", "2026-07-18T00:00:00Z");
    insert_conversation(&conn, "empty", "2026-07-18T00:02:00Z");
    for index in 1..=CAPTURE_BATCH_MAX_RECORDS + 1 {
        insert_message(
            &conn,
            &format!("message-{index}"),
            "messages",
            i64::try_from(index).unwrap(),
            &format!("message {index}"),
        );
    }
    let source = test_source("shelley-snapshot:paging");
    let batches = produce_all(&conn, source.clone(), initial_shelley_position().unwrap());

    assert_eq!(batches.len(), 3);
    assert_eq!(batches[0].records().len(), CAPTURE_BATCH_MAX_RECORDS);
    assert_eq!(batches[0].records()[0].ordinal(), 0);
    assert_eq!(batches[0].records()[63].ordinal(), 63);
    assert_eq!(
        batches[0].records()[0].record_kind().as_str(),
        SHELLEY_MESSAGE_KEY_MARKER_KIND
    );
    assert!(batches[0].records()[1..]
        .iter()
        .all(|record| record.record_kind().as_str() == SHELLEY_MESSAGE_KEY_MARKER_KIND));
    assert_eq!(batches[1].records().len(), CAPTURE_BATCH_MAX_RECORDS);
    assert_eq!(batches[2].records().len(), 4);
    assert_eq!(
        batches[2].records()[2].record_kind().as_str(),
        SHELLEY_NONEMPTY_CONVERSATION_RECORD_KIND
    );
    assert_eq!(
        batches[2].records()[3].record_kind().as_str(),
        SHELLEY_CONVERSATION_RECORD_KIND
    );
    assert_eq!(batches[0].range_end(), batches[1].range_before());
    assert_eq!(batches[1].range_end(), batches[2].range_before());

    let replay_position = batches[0].range_end().clone();
    let replay_keyset = decode_shelley_position(&replay_position).unwrap().unwrap();
    assert_eq!(
        replay_keyset.phase,
        ShelleyCapturePhase::MessageKeyClassification
    );
    assert_eq!(replay_keyset.next_ordinal, CAPTURE_BATCH_MAX_RECORDS as u64);
    let replay = produce_all(&conn, source, replay_position);
    assert_eq!(replay.len(), 2);
    assert_eq!(replay[0].records().len(), CAPTURE_BATCH_MAX_RECORDS);
    assert_eq!(replay[1].records().len(), 4);
    assert_eq!(replay[0].records()[0].ordinal(), 64);
    assert_eq!(
        replay[0].records()[0].record_kind().as_str(),
        SHELLEY_MESSAGE_KEY_MARKER_KIND
    );
    assert_eq!(replay[1].range_end(), batches[2].range_end());
}

#[test]
fn shelley_alternating_rowids_hydrate_each_parent_once() {
    let conn = Connection::open_in_memory().unwrap();
    create_shelley_tables(&conn);
    insert_conversation(&conn, "alpha", "2026-07-18T00:00:00Z");
    insert_conversation(&conn, "zeta", "2026-07-18T00:01:00Z");
    for index in 1..=CAPTURE_BATCH_MAX_RECORDS + 1 {
        insert_message(
            &conn,
            &format!("alpha-{index}"),
            "alpha",
            i64::try_from(index).unwrap(),
            "alpha child",
        );
        insert_message(
            &conn,
            &format!("zeta-{index}"),
            "zeta",
            i64::try_from(index).unwrap(),
            "zeta child",
        );
    }
    let mut fetcher = ShelleyRowFetcher::new(&conn).unwrap();
    let mut position = initial_shelley_position().unwrap();
    while let Some(row) = fetcher.fetch(position).unwrap() {
        position = row.next_position().clone();
    }
    assert_eq!(fetcher.message_parent_hydrations(), 2);
}

#[test]
fn shelley_preflights_oversize_join_under_lowered_sqlite_allocation_limit() {
    let conn = Connection::open_in_memory().unwrap();
    create_shelley_tables(&conn);
    insert_conversation(&conn, "oversize", "2026-07-18T00:00:00Z");
    insert_oversize_message(&conn, "oversize-message", "oversize", 1);
    conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, 64 * 1024);

    let batches = produce_all(
        &conn,
        test_source("shelley-snapshot:oversize"),
        initial_shelley_position().unwrap(),
    );
    assert_eq!(batches.len(), 1);
    assert!(matches!(
        batches[0].records()[1].payload(),
        CapturedRecordPayload::StructuralRejection { .. }
    ));
    assert_eq!(batches[0].records().len(), 4);
    assert_eq!(
        batches[0].records()[0].record_kind().as_str(),
        SHELLEY_MESSAGE_KEY_MARKER_KIND
    );
    assert_eq!(
        batches[0].records()[2].record_kind().as_str(),
        SHELLEY_OVERSIZE_SESSION_RECORD_KIND
    );
    let CapturedRecordPayload::SqliteValues(values) = batches[0].records()[2].payload() else {
        panic!("oversized-only session metadata was not captured as SQLite values");
    };
    assert_eq!(
        decode_shelley_conversation(values).unwrap().conversation_id,
        "oversize"
    );
    assert_eq!(
        batches[0].records()[3].record_kind().as_str(),
        SHELLEY_NONEMPTY_CONVERSATION_RECORD_KIND
    );
}

#[test]
fn shelley_oversize_conversation_key_resumes_without_materializing_anchor() {
    let conn = Connection::open_in_memory().unwrap();
    create_shelley_tables(&conn);
    let oversize_conversation_id = "a".repeat(MAX_PROVIDER_SQLITE_VALUE_BYTES + 1);
    insert_message(
        &conn,
        "oversize-key-message",
        &oversize_conversation_id,
        1,
        "oversize ordering key",
    );
    insert_conversation(&conn, "healthy", "2026-07-18T00:01:00Z");
    insert_message(&conn, "healthy-message", "healthy", 1, "healthy sibling");
    for sequence in 2..=65_i64 {
        insert_message(
            &conn,
            &format!("healthy-message-{sequence}"),
            "healthy",
            sequence,
            "healthy sibling",
        );
    }
    conn.set_limit(
        Limit::SQLITE_LIMIT_LENGTH,
        i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).unwrap(),
    );

    let batches = produce_all(
        &conn,
        test_source("shelley-snapshot:oversize-conversation-key"),
        initial_shelley_position().unwrap(),
    );
    assert_eq!(
        conn.limit(Limit::SQLITE_LIMIT_LENGTH),
        i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).unwrap()
    );
    let records = batches
        .iter()
        .flat_map(|batch| batch.records())
        .collect::<Vec<_>>();
    assert!(matches!(
        records[0].payload(),
        CapturedRecordPayload::StructuralRejection { .. }
    ));
    let CapturedRecordPayload::SqliteValues(values) = records[66].payload() else {
        panic!("healthy sibling was not captured as SQLite values");
    };
    assert_eq!(
        decode_shelley_message_record(values)
            .unwrap()
            .0
            .conversation_id,
        "healthy"
    );
    assert_eq!(batches.len(), 3);
    assert_eq!(batches[0].records().len(), CAPTURE_BATCH_MAX_RECORDS);
    assert_eq!(
        records[131].record_kind().as_str(),
        SHELLEY_NONEMPTY_CONVERSATION_RECORD_KIND
    );
    assert_eq!(records.len(), 132);
    let replay = produce_all(
        &conn,
        test_source("shelley-snapshot:oversize-conversation-key"),
        batches[0].range_end().clone(),
    );
    assert_eq!(replay.len(), 2);
    assert_eq!(replay[0].records().len(), CAPTURE_BATCH_MAX_RECORDS);
    assert_eq!(replay[1].records().len(), 4);
    assert_eq!(replay[1].range_end(), batches[2].range_end());
    let source_temp_tables: i64 = conn
        .query_row("select count(*) from sqlite_temp_master", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(source_temp_tables, 0);
}

#[test]
fn shelley_guarded_previous_id_check_tolerates_oversized_prior_key() {
    let conn = Connection::open_in_memory().unwrap();
    create_shelley_tables(&conn);
    let oversized_id = "a".repeat(MAX_PROVIDER_SQLITE_VALUE_BYTES + 1);
    insert_message(&conn, "bad-key", &oversized_id, 1, "bad key");
    insert_conversation(&conn, "z-healthy", "2026-07-18T00:00:00Z");
    insert_oversize_message(&conn, "oversized-child", "z-healthy", 1);
    insert_message(&conn, "healthy-child", "z-healthy", 2, "survives");
    conn.set_limit(
        Limit::SQLITE_LIMIT_LENGTH,
        i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).unwrap(),
    );

    let batches = produce_all(
        &conn,
        test_source("shelley-snapshot:oversized-prior-key"),
        initial_shelley_position().unwrap(),
    );
    assert_eq!(
        conn.limit(Limit::SQLITE_LIMIT_LENGTH),
        i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).unwrap()
    );
    let records = batches
        .iter()
        .flat_map(|batch| batch.records())
        .collect::<Vec<_>>();
    assert!(matches!(
        records[0].payload(),
        CapturedRecordPayload::StructuralRejection { .. }
    ));
    assert!(matches!(
        records[3].payload(),
        CapturedRecordPayload::StructuralRejection { .. }
    ));
    assert_eq!(
        records[4].record_kind().as_str(),
        SHELLEY_OVERSIZE_SESSION_RECORD_KIND
    );
    assert_eq!(
        records[5].record_kind().as_str(),
        SHELLEY_MESSAGE_RECORD_KIND
    );
}

#[test]
fn shelley_native_restart_is_indexed_and_near_tail_work_is_bounded() {
    let conn = Connection::open_in_memory().unwrap();
    create_shelley_tables(&conn);
    insert_conversation(&conn, "large", "2026-07-18T00:00:00Z");
    for index in 1..=2_048_i64 {
        insert_message(
            &conn,
            &format!("message-{index}"),
            "large",
            index,
            "bounded message",
        );
    }
    let start = encode_shelley_position(ShelleyKeyset {
        phase: ShelleyCapturePhase::Messages,
        next_ordinal: 2_047,
        rowid: 2_047,
        exhausted: false,
        pending_oversize_session: false,
        classification_has_valid_message: false,
        classification_all_keys_valid: true,
    })
    .unwrap();
    let conversation_columns = shelley_conversation_columns(&conn).unwrap();
    let message_columns = shelley_message_columns(&conn).unwrap();
    let message_expressions = shelley_message_select_expressions(&message_columns, "m");
    let message_sql = shelley_same_group_message_candidate_sql(
        &shelley_retained_length_expr(&message_expressions),
        true,
    );
    let message_plan = conn
        .prepare(&format!("explain query plan {message_sql}"))
        .unwrap()
        .query_map(rusqlite::params!["large", 2_047_i64, 2_047_i64], |row| {
            row.get::<_, String>(3)
        })
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
        .join(" | ");
    assert!(
        message_plan.contains("SEARCH m USING INDEX idx_messages_conversation_sequence"),
        "{message_plan}"
    );
    assert!(!message_plan.contains("USE TEMP B-TREE"), "{message_plan}");
    let conversation_sql = shelley_conversation_candidate_sql(
        &shelley_retained_length_expr(&shelley_conversation_select_expressions(
            &conversation_columns,
            "c",
        )),
        true,
    );
    let conversation_plan = conn
        .prepare(&format!("explain query plan {conversation_sql}"))
        .unwrap()
        .query_map([0_i64], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
        .join(" | ");
    assert!(
        conversation_plan.contains("SEARCH c USING INTEGER PRIMARY KEY (rowid>?)"),
        "{conversation_plan}"
    );
    assert!(
        conversation_plan
            .contains("SEARCH m USING COVERING INDEX idx_messages_conversation_sequence"),
        "{conversation_plan}"
    );
    assert!(
        !conversation_plan.contains("USE TEMP B-TREE"),
        "{conversation_plan}"
    );

    // Fetcher construction performs fixed schema/index validation and prepares every
    // Shelley phase. Exclude that one-time setup so this counter measures the resumed
    // native seek, its bounded parent read, and message hydration only.
    let mut fetcher = ShelleyRowFetcher::new(&conn).unwrap();
    let operations = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&operations);
    conn.progress_handler(
        1,
        Some(move || {
            observed.fetch_add(1, Ordering::Relaxed);
            false
        }),
    );
    let row = fetcher.fetch(start).unwrap().unwrap();
    conn.progress_handler(0, None::<fn() -> bool>);
    assert_eq!(row.ordinal(), 2_047);
    let operations = operations.load(Ordering::Relaxed);
    assert!(
        operations < 5_000,
        "Shelley near-tail native seek used {operations} SQLite VM operations"
    );
    let temp_tables: i64 = conn
        .query_row(
            "select count(*) from sqlite_temp_master where name like 'shelley_%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(temp_tables, 0);
}

#[test]
fn shelley_duplicate_sequence_near_tail_work_is_bounded() {
    let conn = Connection::open_in_memory().unwrap();
    create_shelley_tables(&conn);
    insert_conversation(&conn, "duplicates", "2026-07-18T00:00:00Z");
    for index in 1..=2_048_i64 {
        insert_message(
            &conn,
            &format!("message-{index}"),
            "duplicates",
            1,
            "same native sequence",
        );
    }
    let start = encode_shelley_position(ShelleyKeyset {
        phase: ShelleyCapturePhase::Messages,
        next_ordinal: 2_047,
        rowid: 2_047,
        exhausted: false,
        pending_oversize_session: false,
        classification_has_valid_message: false,
        classification_all_keys_valid: true,
    })
    .unwrap();

    let mut fetcher = ShelleyRowFetcher::new(&conn).unwrap();
    let operations = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&operations);
    conn.progress_handler(
        1,
        Some(move || {
            observed.fetch_add(1, Ordering::Relaxed);
            false
        }),
    );
    let row = fetcher.fetch(start).unwrap().unwrap();
    conn.progress_handler(0, None::<fn() -> bool>);
    assert_eq!(row.ordinal(), 2_047);
    let operations = operations.load(Ordering::Relaxed);
    assert!(
        operations < 5_000,
        "Shelley duplicate-sequence near-tail seek used {operations} SQLite VM operations"
    );
}

#[test]
fn shelley_pre_sequence_requires_native_index_and_resumes_by_rowid() {
    let conn = Connection::open_in_memory().unwrap();
    create_pre_sequence_shelley_tables(&conn);
    conn.execute(
        "insert into conversations (
            conversation_id, slug, created_at, updated_at, cwd
         ) values ('legacy', 'Legacy', '2026-07-18T00:00:00Z',
                   '2026-07-18T00:01:00Z', '/workspace/shelley')",
        [],
    )
    .unwrap();
    conn.execute(
        "insert into messages (
            message_id, conversation_id, type, user_data, created_at
         ) values ('legacy-1', 'legacy', 'user', '{\"Text\":\"one\"}',
                   '2026-07-18T00:00:01Z')",
        [],
    )
    .unwrap();
    let anchor_rowid = conn.last_insert_rowid();
    conn.execute(
        "insert into messages (
            message_id, conversation_id, type, user_data, created_at
         ) values ('legacy-2', 'legacy', 'user', '{\"Text\":\"two\"}',
                   '2026-07-18T00:00:02Z')",
        [],
    )
    .unwrap();

    let message_columns = shelley_message_columns(&conn).unwrap();
    assert!(!message_columns.contains("sequence_id"));
    shelley_require_message_index(&conn, false).unwrap();
    let message_expressions = shelley_message_select_expressions(&message_columns, "m");
    let sql = shelley_same_group_message_candidate_sql(
        &shelley_retained_length_expr(&message_expressions),
        false,
    );
    let plan = conn
        .prepare(&format!("explain query plan {sql}"))
        .unwrap()
        .query_map(rusqlite::params!["legacy", anchor_rowid], |row| {
            row.get::<_, String>(3)
        })
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
        .join(" | ");
    assert!(plan.contains("idx_messages_conversation_id"), "{plan}");
    assert!(!plan.contains("USE TEMP B-TREE"), "{plan}");

    let mut fetcher = ShelleyRowFetcher::new(&conn).unwrap();
    let start = encode_shelley_position(ShelleyKeyset {
        phase: ShelleyCapturePhase::Messages,
        next_ordinal: 1,
        rowid: anchor_rowid,
        exhausted: false,
        pending_oversize_session: false,
        classification_has_valid_message: false,
        classification_all_keys_valid: true,
    })
    .unwrap();
    assert_eq!(fetcher.fetch(start).unwrap().unwrap().ordinal(), 1);
}

#[test]
fn shelley_missing_or_incompatible_native_index_fails_closed() {
    let conn = Connection::open_in_memory().unwrap();
    create_shelley_tables(&conn);
    conn.execute_batch("drop index idx_messages_conversation_sequence")
        .unwrap();
    let missing = ShelleyRowFetcher::new(&conn).err().unwrap();
    assert!(matches!(
        missing,
        CaptureError::InvalidPayload(ref message)
            if message.contains("index on (conversation_id, sequence_id)")
    ));

    conn.execute_batch(
        "create index idx_messages_incompatible
             on messages(sequence_id, conversation_id);",
    )
    .unwrap();
    let incompatible = ShelleyRowFetcher::new(&conn).err().unwrap();
    assert!(matches!(
        incompatible,
        CaptureError::InvalidPayload(ref message)
            if message.contains("index on (conversation_id, sequence_id)")
    ));

    let legacy = Connection::open_in_memory().unwrap();
    create_pre_sequence_shelley_tables(&legacy);
    legacy
        .execute_batch("drop index idx_messages_conversation_id")
        .unwrap();
    let missing_legacy = ShelleyRowFetcher::new(&legacy).err().unwrap();
    assert!(matches!(
        missing_legacy,
        CaptureError::InvalidPayload(ref message)
            if message.contains("index on (conversation_id)")
    ));
}

#[test]
fn shelley_terminal_cursor_is_a_constant_work_noop() {
    let conn = Connection::open_in_memory().unwrap();
    create_shelley_tables(&conn);
    insert_conversation(&conn, "terminal", "2026-07-18T00:00:00Z");
    insert_message(&conn, "terminal-message", "terminal", 1, "terminal");
    let source = test_source("shelley-snapshot:terminal");
    let batches = produce_all(&conn, source.clone(), initial_shelley_position().unwrap());
    let terminal = batches.last().unwrap().range_end().clone();
    assert!(
        decode_shelley_position(&terminal)
            .unwrap()
            .unwrap()
            .exhausted
    );

    let operations = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&operations);
    conn.progress_handler(
        1,
        Some(move || {
            observed.fetch_add(1, Ordering::Relaxed);
            false
        }),
    );
    assert!(produce_all(&conn, source, terminal).is_empty());
    conn.progress_handler(0, None::<fn() -> bool>);
    assert!(operations.load(Ordering::Relaxed) < 100);
    let temp_tables: i64 = conn
        .query_row(
            "select count(*) from sqlite_temp_master where name like 'shelley_%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(temp_tables, 0);
    let source_tables = conn
        .prepare("select name from sqlite_master where type = 'table' order by name")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        source_tables,
        vec!["conversations".to_owned(), "messages".to_owned()]
    );
}

#[test]
fn shelley_message_only_source_uses_bounded_terminal_marker() {
    let conn = Connection::open_in_memory().unwrap();
    create_shelley_tables(&conn);
    insert_message(&conn, "orphan", "missing", 1, "orphan");
    let source = test_source("shelley-snapshot:message-only-terminal");
    let batches = produce_all(&conn, source.clone(), initial_shelley_position().unwrap());
    let records = batches
        .iter()
        .flat_map(|batch| batch.records())
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 3);
    assert_eq!(
        records.last().unwrap().record_kind().as_str(),
        SHELLEY_TERMINAL_MARKER_KIND
    );
    let terminal = batches.last().unwrap().range_end().clone();
    assert!(
        decode_shelley_position(&terminal)
            .unwrap()
            .unwrap()
            .exhausted
    );

    let operations = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&operations);
    conn.progress_handler(
        1,
        Some(move || {
            observed.fetch_add(1, Ordering::Relaxed);
            false
        }),
    );
    assert!(produce_all(&conn, source, terminal).is_empty());
    conn.progress_handler(0, None::<fn() -> bool>);
    assert!(operations.load(Ordering::Relaxed) < 100);
}
