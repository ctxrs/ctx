use super::*;

#[test]
fn logical_rows_split_at_sixty_four_and_resume_the_exact_keyset() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn);
    insert_checkpoint(&conn, "thread-a", "checkpoint-a");
    for index in 0..=CAPTURE_BATCH_MAX_RECORDS {
        insert_write(
            &conn,
            "thread-a",
            "checkpoint-a",
            "task-a",
            i64::try_from(index).unwrap(),
            &message_blob(vec![message_value(
                if index % 2 == 0 { "human" } else { "ai" },
                &format!("message-{index}"),
                &format!("message-id-{index}"),
            )]),
        );
    }
    let source = test_source("paging");
    let initial = initial_deepagents_position().unwrap();
    let batches = produce_all(&conn, source.clone(), initial, context(None));
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[0].records().len(), CAPTURE_BATCH_MAX_RECORDS);
    assert_eq!(batches[0].records()[0].ordinal(), 0);
    assert_eq!(batches[0].records()[63].ordinal(), 63);
    assert_eq!(batches[1].records().len(), 2);
    assert_eq!(batches[1].records()[0].ordinal(), 64);
    assert_eq!(batches[0].range_end(), batches[1].range_before());

    let replay_position = batches[0].range_end().clone();
    let decoded = decode_deepagents_position(&replay_position)
        .unwrap()
        .unwrap();
    assert_eq!(decoded.next_ordinal, 64);
    assert!(matches!(
        decoded.key,
        DeepAgentsPositionKey::Write {
            next_event_index: 64,
            ..
        }
    ));
    let replay = produce_all(&conn, source, replay_position, context(None));
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].records().len(), 2);
    assert_eq!(replay[0].records()[0].ordinal(), 64);
    assert_eq!(replay[0].range_end(), batches[1].range_end());
}

#[test]
fn resumed_fetch_does_not_hydrate_the_committed_write_prefix() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn);
    insert_checkpoint(&conn, "thread-a", "checkpoint-a");
    for index in 0..(CAPTURE_BATCH_MAX_RECORDS * 2) {
        insert_write(
            &conn,
            "thread-a",
            "checkpoint-a",
            "task-a",
            i64::try_from(index).unwrap(),
            &message_blob(vec![message_value(
                "human",
                &format!("message-{index}"),
                &format!("message-id-{index}"),
            )]),
        );
    }
    let source = test_source("bounded-resume");
    let initial = initial_deepagents_position().unwrap();
    let mut fetcher = DeepAgentsRowFetcher::new(&conn, context(None), None).unwrap();
    let mut producer =
        SqliteLogicalRowBatchProducer::new(source.clone(), initial, move |position| {
            fetcher.fetch(position)
        });
    let first = with_sqlite_read_snapshot(&conn, || {
        producer.next_batch().map_err(deepagents_sqlite_batch_error)
    })
    .unwrap()
    .unwrap();
    assert_eq!(first.records().len(), CAPTURE_BATCH_MAX_RECORDS);
    drop(producer);

    let resume = first.range_end().clone();
    let mut fetcher = DeepAgentsRowFetcher::new(&conn, context(None), None).unwrap();
    let mut producer =
        SqliteLogicalRowBatchProducer::new(source.clone(), resume, move |position| {
            fetcher.fetch(position)
        });
    DEEPAGENTS_IMPORT_TRACE.with(|trace| {
        *trace.borrow_mut() = Some(Vec::new());
    });
    let second = with_sqlite_read_snapshot(&conn, || {
        producer.next_batch().map_err(deepagents_sqlite_batch_error)
    })
    .unwrap()
    .unwrap();
    assert_eq!(second.records().len(), CAPTURE_BATCH_MAX_RECORDS);
    assert_eq!(second.records()[0].ordinal(), 64);
    assert_eq!(second.records()[63].ordinal(), 127);
    let trace = DEEPAGENTS_IMPORT_TRACE
        .with(|trace| trace.borrow_mut().take())
        .unwrap();
    let hydrated = trace
        .iter()
        .filter_map(|event| match event {
            DeepAgentsImportTraceEvent::WriteHydrated(rowid) => Some(*rowid),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(hydrated.len(), CAPTURE_BATCH_MAX_RECORDS + 1);
    assert_eq!(
        hydrated.iter().copied().collect::<BTreeSet<_>>().len(),
        CAPTURE_BATCH_MAX_RECORDS + 1
    );
    assert!(hydrated
        .iter()
        .all(|rowid| { *rowid >= i64::try_from(CAPTURE_BATCH_MAX_RECORDS).unwrap() }));

    let tail = produce_all(
        &conn,
        source.clone(),
        second.range_end().clone(),
        context(None),
    );
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].records().len(), 1);
    let exhausted = produce_all(&conn, source, tail[0].range_end().clone(), context(None));
    assert!(exhausted.is_empty());
}

#[test]
fn persistent_producer_keeps_only_fixed_keys_and_position_at_group_boundary() {
    let source_conn = Connection::open_in_memory().unwrap();
    create_tables(&source_conn);
    insert_checkpoint(&source_conn, "thread-a", "checkpoint-a");
    let large_text = "x".repeat(CAPTURE_BATCH_MAX_PAYLOAD_BYTES / 2 + 64 * 1024);
    for index in 0..5 {
        insert_write(
            &source_conn,
            "thread-a",
            "checkpoint-a",
            "task-a",
            index,
            &message_blob(vec![message_value(
                "human",
                &large_text,
                &format!("message-id-{index}"),
            )]),
        );
    }

    let directory = crate::test_support_paths::tempdir().unwrap();
    let mut store = Store::open(directory.path().join("store.sqlite")).unwrap();
    let context = context(Some(directory.path().join("sessions.db")));
    let source = test_source("persistent-group-boundary");
    let stream = captured_batch_cursor_stream(&source);
    let initial_position = initial_deepagents_position().unwrap();
    let admission = CapturedSourceAdmission::conversation_for_context(&source, &context).unwrap();
    let committed_store = Store::open_read_only(store.path()).unwrap();
    let fetcher = Rc::new(RefCell::new(
        DeepAgentsRowFetcher::new(&source_conn, context.clone(), Some(committed_store)).unwrap(),
    ));
    let observed_fetch_positions = Rc::new(RefCell::new(Vec::new()));
    let producer_fetcher = Rc::clone(&fetcher);
    let producer_observations = Rc::clone(&observed_fetch_positions);
    let mut producer = SqliteLogicalRowBatchProducer::new(
        source.clone(),
        initial_position.clone(),
        move |position: NativePosition| {
            producer_observations.borrow_mut().push(position.clone());
            producer_fetcher.borrow_mut().fetch(position)
        },
    );
    let batch_requests = Cell::new(0_usize);
    let max_batches = NonZeroUsize::new(CAPTURE_BATCH_MAX_BATCHES_PER_GROUP).unwrap();
    let mut projector = CursorOnlyProjector;

    let first_group = import_captured_batches(
        &mut store,
        &admission,
        NormalizedProviderImportOptions::default(),
        &context.machine_id,
        context.imported_at,
        None,
        &initial_position,
        CapturedBatchCursorMode::Resume,
        max_batches,
        &mut projector,
        || {
            if batch_requests.get() > 0 {
                fetcher.borrow_mut().reset_for_batch_request();
            }
            batch_requests.set(batch_requests.get() + 1);
            with_sqlite_read_snapshot(&source_conn, || {
                producer.next_batch().map_err(deepagents_sqlite_batch_error)
            })
        },
        || Ok(true),
    )
    .unwrap();
    assert_eq!(first_group.batches_imported, max_batches.get());
    assert!(!first_group.source_exhausted);
    assert_eq!(batch_requests.get(), max_batches.get());

    let group_cursor = store
        .get_sync_cursor(None, &context.machine_id, &stream)
        .unwrap()
        .unwrap();
    let certified = CertifiedProviderCursor::decode_if_certified(&group_cursor.cursor)
        .unwrap()
        .unwrap();
    assert_eq!(producer.current_position(), certified.native_position());
    let boundary_position = certified.native_position().clone();
    assert_eq!(
        observed_fetch_positions
            .borrow()
            .iter()
            .filter(|position| *position == &boundary_position)
            .count(),
        1,
        "the fifth row should exist only as the producer's one retained lookahead",
    );

    assert_eq!(std::mem::size_of::<DeepAgentsMessageDedupeKey>(), 32);
    assert_eq!(
        fetcher.borrow().retained_dedupe_key_counts(),
        (1, 1),
        "only the prior accepted row and current lookahead row keys may cross the boundary",
    );
    assert_eq!(
        fetcher.borrow().last_emitted.as_ref().unwrap().before,
        boundary_position,
        "fetcher retention should be position-only while producer owns lookahead payload",
    );

    let second_group = import_captured_batches(
        &mut store,
        &admission,
        NormalizedProviderImportOptions::default(),
        &context.machine_id,
        context.imported_at,
        Some(&group_cursor),
        &initial_position,
        CapturedBatchCursorMode::Resume,
        max_batches,
        &mut projector,
        || {
            if batch_requests.get() > 0 {
                fetcher.borrow_mut().reset_for_batch_request();
            }
            batch_requests.set(batch_requests.get() + 1);
            with_sqlite_read_snapshot(&source_conn, || {
                producer.next_batch().map_err(deepagents_sqlite_batch_error)
            })
        },
        || Ok(true),
    )
    .unwrap();
    assert!(second_group.source_exhausted);
    assert_eq!(
        observed_fetch_positions
            .borrow()
            .iter()
            .filter(|position| *position == &boundary_position)
            .count(),
        1,
        "the persistent producer must consume rather than refetch its boundary lookahead",
    );
    assert!(batch_requests.get() > max_batches.get());
    assert_eq!(
        fetcher.borrow().retained_dedupe_key_counts(),
        (1, 1),
        "source exhaustion may retain only the final cumulative row's fixed-size identity",
    );
}

#[test]
fn high_fanout_parent_metadata_is_hydrated_once_and_children_are_parent_free() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn);
    insert_checkpoint(&conn, "thread-a", "checkpoint-a");
    let write_count = CAPTURE_BATCH_MAX_RECORDS + 2;
    for index in 0..write_count {
        insert_write(
            &conn,
            "thread-a",
            "checkpoint-a",
            "task-a",
            i64::try_from(index).unwrap(),
            &message_blob(vec![message_value(
                "human",
                "cached checkpoint metadata",
                &format!("message-{index}"),
            )]),
        );
    }
    DEEPAGENTS_IMPORT_TRACE.with(|trace| {
        *trace.borrow_mut() = Some(Vec::new());
    });

    let batches = produce_all(
        &conn,
        test_source("checkpoint-time-cache"),
        initial_deepagents_position().unwrap(),
        context(None),
    );
    let trace = DEEPAGENTS_IMPORT_TRACE
        .with(|trace| trace.borrow_mut().take())
        .unwrap();

    assert!(batches.len() >= 2);
    assert_eq!(
        trace
            .iter()
            .filter(|event| matches!(
                event,
                DeepAgentsImportTraceEvent::ThreadMetadataHydrated(checkpoint_id)
                    if checkpoint_id == "checkpoint-a"
            ))
            .count(),
        1,
        "the parent phase must hydrate the one checkpoint metadata row exactly once",
    );
    assert_eq!(
        trace
            .iter()
            .filter(|event| matches!(
                event,
                DeepAgentsImportTraceEvent::CheckpointMetadataPreflightQueried(_)
                    | DeepAgentsImportTraceEvent::CheckpointMetadataHydrated(_)
            ))
            .count(),
        0,
        "the latest-checkpoint time derived by the parent phase must serve every child",
    );
    let child_values = batches
        .iter()
        .flat_map(|batch| batch.records())
        .filter(|record| record.record_kind().as_str() == DEEPAGENTS_WRITE_RECORD_KIND)
        .map(|record| match record.payload() {
            CapturedRecordPayload::SqliteValues(values) => values,
            _ => panic!("Deep Agents child record must use SQLite values"),
        })
        .collect::<Vec<_>>();
    assert_eq!(child_values.len(), write_count);
    assert!(child_values.iter().all(|values| values.len() == 11));
    assert!(child_values.iter().all(|values| {
        let decoded = decode_deepagents_write_values(values).unwrap();
        decoded.key.thread_id == "thread-a"
            && decoded.occurred_at.is_some()
            && !values.iter().any(|value| {
                matches!(value,
                CapturedSqliteValue::Text(text) if text == "deepagents-test-agent"
                    || text == "codex/deepagents-test"
                    || text == "/workspace/deepagents-test")
            })
    }));
}
