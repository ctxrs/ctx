use super::*;

#[test]
fn shelley_public_route_publishes_one_exact_cursor_and_noops() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("shelley.db");
    let writer = Connection::open(&path).unwrap();
    create_shelley_tables(&writer);
    insert_conversation(&writer, "public-session", "2026-07-18T00:00:00Z");
    for index in 1..=CAPTURE_BATCH_MAX_RECORDS {
        insert_message(
            &writer,
            &format!("public-message-{index}"),
            "public-session",
            i64::try_from(index).unwrap(),
            "public route",
        );
    }
    drop(writer);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let options = crate::ShelleySqliteImportOptions {
        machine_id: "shelley-public-route".to_owned(),
        source_path: Some(temp.path().join("logical-shelley-source")),
        imported_at: test_context(&path).imported_at,
        history_record_id: None,
        capture_work_limit: crate::CaptureWorkLimit::Drain,
        inventory_observation_token: None,
    };
    let first = crate::import_shelley_sqlite(&path, &mut store, options.clone()).unwrap();
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, CAPTURE_BATCH_MAX_RECORDS);
    assert_eq!(first.imported, CAPTURE_BATCH_MAX_RECORDS + 1);
    assert_eq!(first.skipped, 0);
    let cursor_path = provider_path_identity(&fs::canonicalize(&path).unwrap()).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Shelley,
        SHELLEY_SQLITE_SOURCE_FORMAT,
        &cursor_path,
    );
    let cursor = store
        .get_sync_cursor(None, &options.machine_id, &stream)
        .unwrap()
        .unwrap();
    assert!(
        decode_shelley_position(
            CertifiedProviderCursor::decode_if_certified(&cursor.cursor)
                .unwrap()
                .unwrap()
                .native_position()
        )
        .unwrap()
        .unwrap()
        .exhausted
    );

    let second = crate::import_shelley_sqlite(&path, &mut store, options.clone()).unwrap();
    assert_eq!(second.imported, 0);
    assert_eq!(second.failed, 0);
    assert_eq!(
        store
            .get_sync_cursor(None, &options.machine_id, &stream)
            .unwrap()
            .unwrap(),
        cursor
    );
    let session = store
        .session_by_external_session(CaptureProvider::Shelley, "public-session")
        .unwrap()
        .unwrap();
    assert_eq!(
        store.events_for_session(session.id).unwrap().len(),
        CAPTURE_BATCH_MAX_RECORDS
    );
    let capture_source = store
        .capture_source_by_external_session(CaptureProvider::Shelley, "public-session")
        .unwrap()
        .unwrap();
    assert_eq!(
        capture_source.descriptor.raw_source_path,
        options
            .source_path
            .as_ref()
            .map(|source_path| source_path.display().to_string())
    );
    assert!(!capture_source.sync.metadata["cursor"].is_null());
}

#[test]
fn shelley_multibatch_oversized_only_conversation_imports_one_session() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("shelley.db");
    let writer = Connection::open(&path).unwrap();
    create_shelley_tables(&writer);
    insert_conversation(&writer, "a-fill", "2026-07-18T00:00:00Z");
    insert_conversation(&writer, "z-oversized-only", "2026-07-18T00:02:00Z");
    for index in 1..CAPTURE_BATCH_MAX_RECORDS {
        insert_message(
            &writer,
            &format!("fill-message-{index}"),
            "a-fill",
            i64::try_from(index).unwrap(),
            "fill",
        );
    }
    insert_oversize_message(&writer, "oversized-only", "z-oversized-only", 1);
    drop(writer);

    let reader = open_provider_sqlite_readonly(&path).unwrap();
    let source = test_source("shelley-snapshot:multibatch-oversized-only");
    let batches = produce_all(&reader, source.clone(), initial_shelley_position().unwrap());
    assert_eq!(batches.len(), 3);
    assert_eq!(batches[0].records().len(), CAPTURE_BATCH_MAX_RECORDS);
    assert!(matches!(
        batches[1].records().last().unwrap().payload(),
        CapturedRecordPayload::StructuralRejection { .. }
    ));
    assert!(
        decode_shelley_position(batches[1].range_end())
            .unwrap()
            .unwrap()
            .pending_oversize_session
    );
    assert_eq!(
        batches[2].records()[0].record_kind().as_str(),
        SHELLEY_OVERSIZE_SESSION_RECORD_KIND
    );
    let resumed = produce_all(&reader, source, batches[1].range_end().clone());
    assert_eq!(
        resumed[0].records()[0].record_kind().as_str(),
        SHELLEY_OVERSIZE_SESSION_RECORD_KIND
    );
    assert_eq!(
        resumed.last().unwrap().range_end(),
        batches.last().unwrap().range_end()
    );
    drop(reader);

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let options = crate::ShelleySqliteImportOptions {
        machine_id: "shelley-oversized-only".to_owned(),
        source_path: Some(path.clone()),
        imported_at: test_context(&path).imported_at,
        history_record_id: None,
        capture_work_limit: crate::CaptureWorkLimit::Drain,
        inventory_observation_token: None,
    };
    let summary = crate::import_shelley_sqlite(&path, &mut store, options).unwrap();
    assert_eq!(summary.failed, 1);
    assert_eq!(summary.imported_sessions, 2);
    assert_eq!(summary.imported_events, CAPTURE_BATCH_MAX_RECORDS - 1);
    assert_eq!(summary.imported, CAPTURE_BATCH_MAX_RECORDS + 1);
    assert_eq!(summary.skipped, 0);
    assert_eq!(summary.failures[0].line, CAPTURE_BATCH_MAX_RECORDS * 2);
    let session = store
        .session_by_external_session(CaptureProvider::Shelley, "z-oversized-only")
        .unwrap()
        .unwrap();
    let capture_source = store
        .get_capture_source(session.capture_source_id.unwrap())
        .unwrap();
    assert_eq!(
        capture_source.descriptor.cwd.as_deref(),
        Some("/workspace/shelley")
    );
    assert_eq!(
        session.sync.metadata["metadata"]["slug"],
        "Conversation z-oversized-only"
    );
    assert!(store.events_for_session(session.id).unwrap().is_empty());
}

#[test]
fn shelley_multibatch_mixed_conversation_avoids_duplicate_session() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("shelley.db");
    let writer = Connection::open(&path).unwrap();
    create_shelley_tables(&writer);
    insert_conversation(&writer, "a-fill", "2026-07-18T00:00:00Z");
    insert_conversation(&writer, "z-mixed", "2026-07-18T00:02:00Z");
    for index in 1..CAPTURE_BATCH_MAX_RECORDS {
        insert_message(
            &writer,
            &format!("fill-message-{index}"),
            "a-fill",
            i64::try_from(index).unwrap(),
            "fill",
        );
    }
    insert_message(&writer, "mixed-accepted", "z-mixed", 1, "accepted");
    insert_oversize_message(&writer, "mixed-oversize", "z-mixed", 2);
    drop(writer);

    let reader = open_provider_sqlite_readonly(&path).unwrap();
    let batches = produce_all(
        &reader,
        test_source("shelley-snapshot:multibatch-mixed"),
        initial_shelley_position().unwrap(),
    );
    assert_eq!(batches.len(), 3);
    assert_eq!(batches[0].records().len(), CAPTURE_BATCH_MAX_RECORDS);
    let boundary = decode_shelley_position(batches[1].range_end())
        .unwrap()
        .unwrap();
    assert!(!boundary.pending_oversize_session);
    assert!(!batches
        .iter()
        .flat_map(|batch| batch.records())
        .any(|record| record.record_kind().as_str() == SHELLEY_OVERSIZE_SESSION_RECORD_KIND));
    drop(reader);

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let options = crate::ShelleySqliteImportOptions {
        machine_id: "shelley-mixed".to_owned(),
        source_path: Some(path.clone()),
        imported_at: test_context(&path).imported_at,
        history_record_id: None,
        capture_work_limit: crate::CaptureWorkLimit::Drain,
        inventory_observation_token: None,
    };
    let summary = crate::import_shelley_sqlite(&path, &mut store, options).unwrap();
    assert_eq!(summary.failed, 1);
    assert_eq!(summary.imported_sessions, 2);
    assert_eq!(summary.imported_events, CAPTURE_BATCH_MAX_RECORDS);
    assert_eq!(summary.imported, CAPTURE_BATCH_MAX_RECORDS + 2);
    assert_eq!(summary.skipped, 0);
    let session = store
        .session_by_external_session(CaptureProvider::Shelley, "z-mixed")
        .unwrap()
        .unwrap();
    let events = store.events_for_session(session.id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].sync.metadata["metadata"]["message_id"],
        "mixed-accepted"
    );
}

#[test]
fn shelley_production_import_rejects_orphan_and_advances_with_valid_siblings() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("shelley.db");
    let writer = Connection::open(&path).unwrap();
    create_shelley_tables(&writer);
    insert_conversation(&writer, "with-message", "2026-07-18T00:00:00Z");
    insert_conversation(&writer, "empty", "2026-07-18T00:01:00Z");
    insert_message(&writer, "valid", "with-message", 1, "accepted");
    insert_message(&writer, "orphan", "missing", 2, "rejected locally");
    drop(writer);

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let options = crate::ShelleySqliteImportOptions {
        machine_id: "shelley-orphan-sibling".to_owned(),
        source_path: Some(temp.path().join("logical-shelley-source")),
        imported_at: test_context(&path).imported_at,
        history_record_id: None,
        capture_work_limit: crate::CaptureWorkLimit::Drain,
        inventory_observation_token: None,
    };
    let first = crate::import_shelley_sqlite(&path, &mut store, options.clone()).unwrap();
    assert_eq!(first.failed, 1, "{:?}", first.failures);
    assert_eq!(first.failures[0].line, 2);
    assert!(first.failures[0]
        .error
        .contains("missing conversation missing"));
    assert_eq!(first.imported_sessions, 2);
    assert_eq!(first.imported_events, 1);
    assert_eq!(first.imported, 3);
    assert_eq!(
        store
            .session_by_external_session(CaptureProvider::Shelley, "with-message")
            .unwrap()
            .map(|session| store.events_for_session(session.id).unwrap().len()),
        Some(1)
    );
    assert!(store
        .session_by_external_session(CaptureProvider::Shelley, "empty")
        .unwrap()
        .is_some());
    assert!(store
        .session_by_external_session(CaptureProvider::Shelley, "missing")
        .unwrap()
        .is_none());

    let cursor_path = provider_path_identity(&fs::canonicalize(&path).unwrap()).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Shelley,
        SHELLEY_SQLITE_SOURCE_FORMAT,
        &cursor_path,
    );
    let cursor = store
        .get_sync_cursor(None, &options.machine_id, &stream)
        .unwrap()
        .unwrap();
    assert!(
        decode_shelley_position(
            CertifiedProviderCursor::decode_if_certified(&cursor.cursor)
                .unwrap()
                .unwrap()
                .native_position()
        )
        .unwrap()
        .unwrap()
        .exhausted
    );

    let second = crate::import_shelley_sqlite(&path, &mut store, options).unwrap();
    assert_eq!(second.imported, 0);
    assert_eq!(
        store
            .get_sync_cursor(None, "shelley-orphan-sibling", &stream)
            .unwrap()
            .unwrap(),
        cursor
    );
    assert_eq!(
        store
            .session_by_external_session(CaptureProvider::Shelley, "with-message")
            .unwrap()
            .map(|session| store.events_for_session(session.id).unwrap().len()),
        Some(1)
    );
}

#[test]
fn shelley_releases_batch_snapshot_and_detects_source_mutation() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("shelley.db");
    let writer = Connection::open(&path).unwrap();
    create_shelley_tables(&writer);
    insert_conversation(&writer, "mutation", "2026-07-18T00:00:00Z");
    for index in 1..=CAPTURE_BATCH_MAX_RECORDS {
        insert_message(
            &writer,
            &format!("message-{index}"),
            "mutation",
            i64::try_from(index).unwrap(),
            "before mutation",
        );
    }
    drop(writer);
    let snapshot = shelley_source_snapshot(&path).unwrap();
    let reader = open_provider_sqlite_readonly(&path).unwrap();
    let mut fetcher = ShelleyRowFetcher::new(&reader).unwrap();
    let mut producer = SqliteLogicalRowBatchProducer::new(
        test_source("shelley-snapshot:mutation"),
        initial_shelley_position().unwrap(),
        move |position| fetcher.fetch(position),
    );
    let first = with_sqlite_read_snapshot(&reader, || {
        producer.next_batch().map_err(shelley_sqlite_batch_error)
    })
    .unwrap()
    .unwrap();
    assert_eq!(first.records().len(), CAPTURE_BATCH_MAX_RECORDS);
    assert!(reader.is_autocommit());

    let writer = Connection::open(&path).unwrap();
    insert_message(&writer, "message-65", "mutation", 65, "after mutation");
    drop(writer);
    assert!(!snapshot.revalidate(&path).unwrap());
    assert!(reader.is_autocommit());
}
