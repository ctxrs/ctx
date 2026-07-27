use super::*;

#[test]
fn capped_thread_summary_skips_oversized_metadata_and_continues_to_sibling() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn);
    insert_oversized_checkpoint_metadata(&conn, "thread-a", "checkpoint-a");
    insert_checkpoint(&conn, "thread-a", "checkpoint-b");
    let sqlite_value_limit = i32::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES).unwrap();
    conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, sqlite_value_limit);

    let summary = deepagents_thread_summary(&conn, &context(None), "thread-a", None)
        .unwrap()
        .unwrap();

    assert_eq!(
        summary.thread.latest_checkpoint_id.as_deref(),
        Some("checkpoint-b")
    );
    assert_eq!(
        conn.limit(Limit::SQLITE_LIMIT_LENGTH),
        sqlite_value_limit,
        "the caller's SQLite length cap must be restored before metadata hydration",
    );
}

#[test]
fn capped_checkpoint_time_skips_oversized_metadata_and_continues_to_sibling_write() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn);
    insert_oversized_checkpoint_metadata(&conn, "thread-a", "checkpoint-a");
    insert_checkpoint(&conn, "thread-a", "checkpoint-b");
    insert_write(
        &conn,
        "thread-a",
        "checkpoint-a",
        "task-a",
        0,
        &message_blob(vec![message_value("human", "first", "message-a")]),
    );
    insert_write(
        &conn,
        "thread-a",
        "checkpoint-b",
        "task-b",
        0,
        &message_blob(vec![message_value("ai", "second", "message-b")]),
    );
    let sqlite_value_limit = i32::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES).unwrap();
    conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, sqlite_value_limit);
    DEEPAGENTS_IMPORT_TRACE.with(|trace| {
        *trace.borrow_mut() = Some(Vec::new());
    });

    let batches = produce_all(
        &conn,
        test_source("oversized-checkpoint-metadata"),
        initial_deepagents_position().unwrap(),
        context(None),
    );
    let trace = DEEPAGENTS_IMPORT_TRACE
        .with(|trace| trace.borrow_mut().take())
        .unwrap();
    let records = batches
        .iter()
        .flat_map(|batch| batch.records())
        .collect::<Vec<_>>();

    assert!(matches!(
        records[0].payload(),
        CapturedRecordPayload::SqliteValues(_)
    ));
    assert!(matches!(
        records[1].payload(),
        CapturedRecordPayload::SqliteValues(_)
    ));
    assert!(trace.iter().any(|event| matches!(
        event,
        DeepAgentsImportTraceEvent::CheckpointMetadataPreflightQueried(checkpoint_id)
            if checkpoint_id == "checkpoint-a"
    )));
    assert!(!trace.iter().any(|event| matches!(
        event,
        DeepAgentsImportTraceEvent::CheckpointMetadataHydrated(checkpoint_id)
            if checkpoint_id == "checkpoint-a"
    )));
    assert!(trace.iter().any(|event| matches!(
        event,
        DeepAgentsImportTraceEvent::CheckpointMetadataHydrated(checkpoint_id)
            if checkpoint_id == "checkpoint-b"
    )));
    assert_eq!(
        conn.limit(Limit::SQLITE_LIMIT_LENGTH),
        sqlite_value_limit,
        "the provider must restore the cap before each sibling hydration",
    );
}

#[test]
fn oversized_write_is_rejected_before_sqlite_blob_hydration() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn);
    insert_checkpoint(&conn, "thread-a", "checkpoint-a");
    let oversize = i64::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES)
        .unwrap()
        .checked_add(1)
        .unwrap();
    conn.execute(
        "insert into writes
         (thread_id, checkpoint_ns, checkpoint_id, task_id, idx, channel, type, value)
         values ('thread-a', '', 'checkpoint-a', 'task-a', 0, 'messages', 'msgpack', zeroblob(?1))",
        [oversize],
    )
    .unwrap();
    conn.set_limit(
        Limit::SQLITE_LIMIT_LENGTH,
        i32::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES).unwrap(),
    );
    let batches = produce_all(
        &conn,
        test_source("oversize"),
        initial_deepagents_position().unwrap(),
        context(None),
    );
    assert_eq!(
        batches[0].records()[0].record_kind().as_str(),
        DEEPAGENTS_THREAD_RECORD_KIND
    );
    assert!(matches!(
        batches[0].records()[1].payload(),
        CapturedRecordPayload::StructuralRejection {
            kind: StructuralRejectionKind::OversizeRecord,
            observed_bytes,
        } if *observed_bytes > CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES as u64
    ));
}

#[test]
fn oversized_ordering_key_is_rejected_before_sqlite_text_hydration() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn);
    insert_checkpoint(&conn, "thread-a", "checkpoint-a");
    let oversized_task = "z".repeat(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES + 1);
    insert_write(
        &conn,
        "thread-a",
        "checkpoint-a",
        &oversized_task,
        0,
        &message_blob(vec![message_value("human", "oversized key", "message-a")]),
    );
    let oversized_rowid = conn.last_insert_rowid();
    insert_write(
        &conn,
        "thread-a",
        "checkpoint-a",
        "a-normal-task",
        0,
        &message_blob(vec![message_value("ai", "healthy sibling", "message-z")]),
    );
    let healthy_rowid = conn.last_insert_rowid();
    conn.set_limit(
        Limit::SQLITE_LIMIT_LENGTH,
        i32::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES).unwrap(),
    );
    DEEPAGENTS_IMPORT_TRACE.with(|trace| {
        *trace.borrow_mut() = Some(Vec::new());
    });

    let batches = produce_all(
        &conn,
        test_source("oversized-ordering-key"),
        initial_deepagents_position().unwrap(),
        context(None),
    );
    let trace = DEEPAGENTS_IMPORT_TRACE
        .with(|trace| trace.borrow_mut().take())
        .unwrap();
    let records = batches
        .iter()
        .flat_map(|batch| batch.records())
        .collect::<Vec<_>>();
    assert_eq!(
        records[0].record_kind().as_str(),
        DEEPAGENTS_THREAD_RECORD_KIND
    );
    assert_eq!(
        records[1].record_kind().as_str(),
        DEEPAGENTS_WRITE_RECORD_KIND
    );
    assert_eq!(
        records[2].record_kind().as_str(),
        DEEPAGENTS_WRITE_RECORD_KIND
    );
    assert!(matches!(
        records[0].payload(),
        CapturedRecordPayload::SqliteValues(_)
    ));
    assert!(matches!(
        records[1].payload(),
        CapturedRecordPayload::SqliteValues(_)
    ));
    assert!(matches!(
        records[2].payload(),
        CapturedRecordPayload::StructuralRejection {
            kind: StructuralRejectionKind::OversizeRecord,
            observed_bytes,
        } if *observed_bytes > CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES as u64
    ));
    assert!(!trace
        .iter()
        .any(|event| { *event == DeepAgentsImportTraceEvent::WriteKeyHydrated(oversized_rowid) }));
    assert!(trace
        .iter()
        .any(|event| { *event == DeepAgentsImportTraceEvent::WriteKeyHydrated(healthy_rowid) }));
}

#[test]
fn malformed_write_key_is_rejected_without_poisoning_healthy_sibling() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn);
    insert_checkpoint(&conn, "thread-a", "checkpoint-a");
    insert_write(
        &conn,
        "thread-a",
        "checkpoint-a",
        "healthy-task",
        0,
        &message_blob(vec![message_value(
            "human",
            "healthy sibling",
            "healthy-message",
        )]),
    );
    conn.execute(
        "insert into writes
         (thread_id, checkpoint_ns, checkpoint_id, task_id, idx, channel, type, value)
         values ('thread-a', '', 'checkpoint-a', x'ff', 1, 'messages', 'msgpack', x'00')",
        [],
    )
    .unwrap();

    let context = context(Some("/tmp/deepagents/malformed-key.db".into()));
    let batches = produce_all(
        &conn,
        test_source("malformed-key"),
        initial_deepagents_position().unwrap(),
        context.clone(),
    );
    let mut projector = DeepAgentsCapturedBatchProjector {
        context,
        raw_source_path: Some("/tmp/deepagents/malformed-key.db".to_owned()),
        user_version: 0,
        schema_fingerprint: "schema:test".to_owned(),
        source_revision: "deepagents-snapshot:malformed-key".to_owned(),
        committed_store: None,
    };
    let mut output = CollectingProjectionOutput::default();
    for batch in &batches {
        for record in batch.records() {
            projector.project_record(record, &mut output).unwrap();
        }
    }
    assert_eq!(output.rejections.len(), 1);
    assert!(output.rejections[0]
        .1
        .contains("unsupported SQLite storage class"));
    assert_eq!(
        output
            .normalizations
            .iter()
            .flat_map(|normalization| normalization.captures.iter())
            .filter(|(_, capture)| capture.event.is_some())
            .count(),
        1
    );
}

#[test]
fn source_snapshot_detects_database_mutation() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let path = directory.path().join("sessions.db");
    fs::write(&path, b"deepagents-snapshot").unwrap();
    let snapshot = deepagents_source_snapshot(&path).unwrap();
    assert!(snapshot.revalidate(&path).unwrap());

    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"-changed").unwrap();
    file.sync_all().unwrap();
    assert!(!snapshot.revalidate(&path).unwrap());
}
