use super::*;

#[test]
fn nativepath_store_publication_is_core_first_resumable_and_idempotent() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let connection = new_database(&path);
    insert_thread(
        &connection,
        "root-thread",
        None,
        "json",
        &thread(vec![user("root-user", "root prompt")]),
    );
    insert_thread(
        &connection,
        "child-thread",
        Some("root-thread"),
        "json",
        &thread(vec![output_message(
            "call-child",
            "src/child.rs",
            "CTX-ZED-PRO-ONLY-SENTINEL",
            false,
        )]),
    );
    drop(connection);

    let store_path = directory.path().join("store.sqlite");
    let mut store = ctx_history_store::Store::open(&store_path).unwrap();
    let context = crate::ProviderAdapterContext {
        machine_id: "zed-nativepath-test-machine".to_owned(),
        source_path: Some(path.clone()),
        source_root: None,
        imported_at: chrono::DateTime::parse_from_rfc3339("2026-07-25T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
    };
    let options = crate::ProviderImportOptions {
        capture_work_limit: crate::CaptureWorkLimit::OneSafeGroup,
        ..crate::ProviderImportOptions::default()
    };

    let first = import_zed_nativepath(&path, &mut store, context.clone(), options.clone()).unwrap();
    assert!(first.work_remaining);
    assert_eq!(store.list_sessions().unwrap().len(), 2);
    drop(store);

    let mut store = ctx_history_store::Store::open(&store_path).unwrap();
    let second =
        import_zed_nativepath(&path, &mut store, context.clone(), options.clone()).unwrap();
    assert!(!second.work_remaining);
    let sessions = store.list_sessions().unwrap();
    let root = sessions
        .iter()
        .find(|session| session.external_session_id.as_deref() == Some("root-thread"))
        .unwrap();
    let child = sessions
        .iter()
        .find(|session| session.external_session_id.as_deref() == Some("child-thread"))
        .unwrap();
    assert_eq!(child.parent_session_id, Some(root.id));
    assert_eq!(child.root_session_id, Some(root.id));
    let child_events = store.events_for_session(child.id).unwrap();
    assert_eq!(child_events.len(), 1);
    assert_eq!(
        child_events[0].event_type,
        ctx_history_core::EventType::ToolCall
    );
    assert!(!serde_json::to_string(&child_events[0].payload)
        .unwrap()
        .contains("CTX-ZED-PRO-ONLY-SENTINEL"));

    let replay = import_zed_nativepath(&path, &mut store, context, options).unwrap();
    assert_eq!(replay.work_result(), crate::ProviderImportWorkResult::NoOp);
    assert_eq!(store.list_sessions().unwrap().len(), 2);
    assert_eq!(store.events_for_session(child.id).unwrap().len(), 1);
}

#[test]
fn nativepath_publication_combines_more_than_64_acquisition_units() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let mut connection = new_database(&path);
    let transaction = connection.transaction().unwrap();
    for index in 0..=crate::provider::native_ingestion::NATIVE_INGESTION_PAGE_MAX_UNITS {
        insert_thread(
            &transaction,
            &format!("unit-thread-{index:03}"),
            None,
            "json",
            &thread(Vec::new()),
        );
    }
    transaction.commit().unwrap();
    drop(connection);

    let mut store = ctx_history_store::Store::open(directory.path().join("store.sqlite")).unwrap();
    let context = adapter_context(&path, "zed-unit-boundary-machine");
    let options = crate::ProviderImportOptions {
        capture_work_limit: crate::CaptureWorkLimit::OneSafeGroup,
        ..crate::ProviderImportOptions::default()
    };

    let first = import_zed_nativepath(&path, &mut store, context.clone(), options.clone()).unwrap();
    assert_eq!(
        first.imported_sessions,
        crate::provider::native_ingestion::NATIVE_INGESTION_PAGE_MAX_UNITS + 1
    );
    assert!(first.work_remaining);
    assert_eq!(
        store.list_sessions().unwrap().len(),
        crate::provider::native_ingestion::NATIVE_INGESTION_PAGE_MAX_UNITS + 1
    );

    let second =
        import_zed_nativepath(&path, &mut store, context.clone(), options.clone()).unwrap();
    assert_eq!(second.imported_sessions, 0);
    assert!(!second.work_remaining);
    assert_eq!(
        store.list_sessions().unwrap().len(),
        crate::provider::native_ingestion::NATIVE_INGESTION_PAGE_MAX_UNITS + 1
    );

    let replay = import_zed_nativepath(&path, &mut store, context, options).unwrap();
    assert_eq!(replay.work_result(), crate::ProviderImportWorkResult::NoOp);
    assert_eq!(
        store.list_sessions().unwrap().len(),
        crate::provider::native_ingestion::NATIVE_INGESTION_PAGE_MAX_UNITS + 1
    );
}

#[test]
fn nativepath_publication_splits_before_4096_attempted_store_mutations() {
    const MUTATIONS_PER_ROOT_SESSION: usize = 3;
    const FIXED_GROUP_MUTATIONS: usize = 2;

    let directory = tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let mut connection = new_database(&path);
    let transaction = connection.transaction().unwrap();
    let sessions_in_first_group = (ctx_history_store::NATIVE_PATH_MAX_MUTATION_UNITS
        - FIXED_GROUP_MUTATIONS)
        / MUTATIONS_PER_ROOT_SESSION;
    assert!(
        sessions_in_first_group
            > crate::provider::native_ingestion::NATIVE_INGESTION_PAGE_MAX_UNITS
    );
    for index in 0..=sessions_in_first_group {
        insert_thread(
            &transaction,
            &format!("mutation-thread-{index:04}"),
            None,
            "json",
            &thread(Vec::new()),
        );
    }
    transaction.commit().unwrap();
    drop(connection);

    let mut store = ctx_history_store::Store::open(directory.path().join("store.sqlite")).unwrap();
    let context = adapter_context(&path, "zed-mutation-boundary-machine");
    let options = crate::ProviderImportOptions {
        capture_work_limit: crate::CaptureWorkLimit::OneSafeGroup,
        ..crate::ProviderImportOptions::default()
    };

    let first = import_zed_nativepath(&path, &mut store, context.clone(), options.clone()).unwrap();
    assert_eq!(first.imported_sessions, sessions_in_first_group);
    assert!(first.work_remaining);
    assert_eq!(
        store.list_sessions().unwrap().len(),
        sessions_in_first_group
    );

    let second =
        import_zed_nativepath(&path, &mut store, context.clone(), options.clone()).unwrap();
    assert_eq!(second.imported_sessions, 1);
    assert!(second.work_remaining);
    assert_eq!(
        store.list_sessions().unwrap().len(),
        sessions_in_first_group + 1
    );

    let completed = import_zed_nativepath(&path, &mut store, context, options).unwrap();
    assert!(!completed.work_remaining);
    assert_eq!(
        store.list_sessions().unwrap().len(),
        sessions_in_first_group + 1
    );
}

#[test]
fn nativepath_publication_splits_before_eight_mib_retained_encoding() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let connection = new_database(&path);
    insert_thread(
        &connection,
        "byte-thread-a",
        None,
        "json",
        &thread(Vec::new()),
    );
    insert_thread(
        &connection,
        "byte-thread-b",
        None,
        "json",
        &thread(Vec::new()),
    );
    let summary =
        "s".repeat(ctx_history_store::NATIVE_PATH_MAX_RETAINED_PAGE_BYTES / 2 + 64 * 1024);
    assert!(
        summary.len().saturating_mul(2) > ctx_history_store::NATIVE_PATH_MAX_RETAINED_PAGE_BYTES
    );
    connection
        .execute(
            "UPDATE threads SET summary=?1 WHERE id IN ('byte-thread-a', 'byte-thread-b')",
            [&summary],
        )
        .unwrap();
    drop(connection);

    let mut store = ctx_history_store::Store::open(directory.path().join("store.sqlite")).unwrap();
    let context = adapter_context(&path, "zed-byte-boundary-machine");
    let options = crate::ProviderImportOptions {
        capture_work_limit: crate::CaptureWorkLimit::OneSafeGroup,
        ..crate::ProviderImportOptions::default()
    };

    let first = import_zed_nativepath(&path, &mut store, context.clone(), options.clone()).unwrap();
    assert_eq!(first.imported_sessions, 1);
    assert!(first.work_remaining);
    assert_eq!(store.list_sessions().unwrap().len(), 1);

    let second =
        import_zed_nativepath(&path, &mut store, context.clone(), options.clone()).unwrap();
    assert_eq!(second.imported_sessions, 1);
    assert!(second.work_remaining);
    assert_eq!(store.list_sessions().unwrap().len(), 2);

    let completed = import_zed_nativepath(&path, &mut store, context, options).unwrap();
    assert!(!completed.work_remaining);
    assert_eq!(store.list_sessions().unwrap().len(), 2);
}

#[test]
fn nativepath_missing_source_retires_once_without_deleting_core_history() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let connection = new_database(&path);
    insert_thread(
        &connection,
        "retained-thread",
        None,
        "json",
        &thread(vec![user("retained-user", "retained prompt")]),
    );
    drop(connection);

    let mut store = ctx_history_store::Store::open(directory.path().join("store.sqlite")).unwrap();
    let context = crate::ProviderAdapterContext {
        machine_id: "zed-retirement-test-machine".to_owned(),
        source_path: Some(path.clone()),
        source_root: None,
        imported_at: chrono::DateTime::parse_from_rfc3339("2026-07-25T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
    };
    import_zed_nativepath(
        &path,
        &mut store,
        context.clone(),
        crate::ProviderImportOptions::default(),
    )
    .unwrap();
    fs::remove_file(&path).unwrap();

    let retired = import_zed_nativepath(
        &path,
        &mut store,
        context.clone(),
        crate::ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(
        retired.work_result(),
        crate::ProviderImportWorkResult::Changed
    );
    assert_eq!(store.list_sessions().unwrap().len(), 1);

    let replay = import_zed_nativepath(
        &path,
        &mut store,
        context,
        crate::ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(replay.work_result(), crate::ProviderImportWorkResult::NoOp);
    assert_eq!(store.list_sessions().unwrap().len(), 1);
}

#[test]
fn later_pro_activation_replays_exact_outputs_without_republishing_core() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let connection = new_database(&path);
    insert_thread(
        &connection,
        "output-thread",
        None,
        "json",
        &thread(vec![output_message(
            "output-call",
            "src/output.rs",
            "CTX-ZED-LATER-PRO-SENTINEL",
            false,
        )]),
    );
    drop(connection);

    let mut store = ctx_history_store::Store::open(directory.path().join("store.sqlite")).unwrap();
    let context = crate::ProviderAdapterContext {
        machine_id: "zed-output-replay-test-machine".to_owned(),
        source_path: Some(path.clone()),
        source_root: None,
        imported_at: chrono::DateTime::parse_from_rfc3339("2026-07-25T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
    };
    import_zed_nativepath(
        &path,
        &mut store,
        context.clone(),
        crate::ProviderImportOptions::default(),
    )
    .unwrap();
    let session = store
        .session_by_external_session(ctx_history_core::CaptureProvider::Zed, "output-thread")
        .unwrap()
        .unwrap();
    let core = store.events_for_session(session.id).unwrap();
    assert_eq!(core.len(), 1);
    assert!(!serde_json::to_string(&core[0].payload)
        .unwrap()
        .contains("CTX-ZED-LATER-PRO-SENTINEL"));

    let sink = Arc::new(RecordingProSink::default());
    let replay_options = crate::ProviderImportOptions {
        import_profile: crate::ImportProfile::ProReplayOnly(sink.clone()),
        ..crate::ProviderImportOptions::default()
    };
    let replay =
        import_zed_nativepath(&path, &mut store, context.clone(), replay_options.clone()).unwrap();
    assert_eq!(replay.work_result(), crate::ProviderImportWorkResult::NoOp);
    let output = sink.content.lock().unwrap();
    assert_eq!(output.len(), 1);
    assert!(String::from_utf8_lossy(&output[0]).contains("CTX-ZED-LATER-PRO-SENTINEL"));
    drop(output);
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 1);

    import_zed_nativepath(&path, &mut store, context, replay_options).unwrap();
    assert_eq!(sink.content.lock().unwrap().len(), 1);
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 1);
}

#[test]
fn pro_failure_marks_only_output_behind_after_core_commit() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let connection = new_database(&path);
    insert_thread(
        &connection,
        "pro-failure-thread",
        None,
        "json",
        &thread(vec![output_message(
            "failing-call",
            "src/failing.rs",
            "CTX-ZED-FAILED-PRO-SENTINEL",
            false,
        )]),
    );
    drop(connection);

    let mut store = ctx_history_store::Store::open(directory.path().join("store.sqlite")).unwrap();
    let sink = Arc::new(FailingProSink::default());
    let summary = import_zed_nativepath(
        &path,
        &mut store,
        crate::ProviderAdapterContext {
            machine_id: "zed-pro-failure-test-machine".to_owned(),
            source_path: Some(path.clone()),
            source_root: None,
            imported_at: chrono::DateTime::parse_from_rfc3339("2026-07-25T12:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        },
        crate::ProviderImportOptions {
            import_profile: crate::ImportProfile::CoreAndPro(sink.clone()),
            ..crate::ProviderImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(
        summary.work_result(),
        crate::ProviderImportWorkResult::Changed
    );
    assert_eq!(sink.behind.load(Ordering::SeqCst), 1);
    let session = store
        .session_by_external_session(ctx_history_core::CaptureProvider::Zed, "pro-failure-thread")
        .unwrap()
        .unwrap();
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 1);
}

#[test]
fn normalized_payload_rewrites_only_the_exact_released_legacy_hash() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let connection = new_database(&path);
    insert_thread(
        &connection,
        "rewrite-thread",
        None,
        "json",
        &thread(vec![user("rewrite-user", "released body")]),
    );
    drop(connection);
    let (_, scanned) = scan(&path);
    let legacy_hash = scanned.events()[0].legacy_content_hash.clone();
    assert_eq!(
        legacy_hash,
        "dc03d5862aa4e2931b4973b520c3409b71eaa1ea52419b7b0f510687b7f5ffd0"
    );

    let store_path = directory.path().join("store.sqlite");
    let context = adapter_context(&path, "zed-rewrite-test-machine");
    let mut store = ctx_history_store::Store::open(&store_path).unwrap();
    import_zed_nativepath(
        &path,
        &mut store,
        context.clone(),
        crate::ProviderImportOptions::default(),
    )
    .unwrap();
    let session = store
        .session_by_external_session(CaptureProvider::Zed, "rewrite-thread")
        .unwrap()
        .unwrap();
    let original = store.events_for_session(session.id).unwrap().remove(0);
    let normalized_hash = crate::compute_payload_hash(&original.payload).unwrap();
    assert_eq!(
        original.sync.metadata["provider_event_hash"],
        json!(normalized_hash)
    );
    assert_eq!(
        original.sync.metadata["provider_event_hash_authority"],
        json!("normalized_payload_fallback")
    );
    assert_ne!(normalized_hash, legacy_hash);

    let released_dedupe = ctx_history_store::Store::provider_event_dedupe_key_with_payload_hash(
        original.dedupe_key.as_deref().unwrap(),
        &legacy_hash,
    )
    .unwrap();
    let mut released_metadata = original.sync.metadata.clone();
    released_metadata["provider_event_hash"] = json!(legacy_hash);
    released_metadata["provider_event_hash_authority"] = json!("provider_supplied");
    drop(store);
    let raw = Connection::open(&store_path).unwrap();
    raw.execute(
        "UPDATE events
         SET payload_json=?2, dedupe_key=?3, metadata_json=?4
         WHERE id=?1",
        params![
            original.id.to_string(),
            json!({"released_payload": true}).to_string(),
            released_dedupe,
            released_metadata.to_string(),
        ],
    )
    .unwrap();
    drop(raw);

    let mut store = ctx_history_store::Store::open(&store_path).unwrap();
    let migrated = import_zed_nativepath(
        &path,
        &mut store,
        context.clone(),
        crate::ProviderImportOptions {
            inventory_observation_token: Some("rewrite-generation-2".to_owned()),
            ..crate::ProviderImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(migrated.skipped_events, 1);
    let retained = store.get_event(original.id).unwrap();
    assert_eq!(retained.id, original.id);
    assert_eq!(retained.seq, original.seq);
    assert_eq!(retained.payload, original.payload);
    assert_eq!(
        retained.sync.metadata["provider_event_hash_authority"],
        json!("normalized_payload_fallback")
    );

    let wrong_hash = "f".repeat(64);
    let wrong_dedupe = ctx_history_store::Store::provider_event_dedupe_key_with_payload_hash(
        retained.dedupe_key.as_deref().unwrap(),
        &wrong_hash,
    )
    .unwrap();
    let mut wrong_metadata = retained.sync.metadata.clone();
    wrong_metadata["provider_event_hash"] = json!(wrong_hash);
    wrong_metadata["provider_event_hash_authority"] = json!("provider_supplied");
    drop(store);
    let raw = Connection::open(&store_path).unwrap();
    raw.execute(
        "UPDATE events SET dedupe_key=?2, metadata_json=?3 WHERE id=?1",
        params![
            retained.id.to_string(),
            wrong_dedupe,
            wrong_metadata.to_string(),
        ],
    )
    .unwrap();
    drop(raw);

    let mut store = ctx_history_store::Store::open(&store_path).unwrap();
    let error = import_zed_nativepath(
        &path,
        &mut store,
        context,
        crate::ProviderImportOptions {
            inventory_observation_token: Some("rewrite-generation-3".to_owned()),
            ..crate::ProviderImportOptions::default()
        },
    )
    .unwrap_err();
    assert!(matches!(
        error,
        crate::CaptureError::Store(ctx_history_store::StoreError::ProviderEventConflict { .. })
    ));
}

#[test]
fn long_message_hydrates_by_native_message_ordinal_and_detects_mutation() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let connection = new_database(&path);
    let full_text = format!(
        "{}-complete-nativepath-message",
        "x".repeat(PROVIDER_MAX_TEXT_CHARS)
    );
    insert_thread(
        &connection,
        "hydrate-thread",
        None,
        "json",
        &thread(vec![
            result_only_message("omitted-result-a", "private result A"),
            result_only_message("omitted-result-b", "private result B"),
            user("hydrate-user", &full_text),
        ]),
    );
    drop(connection);

    let mut store = ctx_history_store::Store::open(directory.path().join("store.sqlite")).unwrap();
    import_zed_nativepath(
        &path,
        &mut store,
        adapter_context(&path, "zed-hydrate-test-machine"),
        crate::ProviderImportOptions::default(),
    )
    .unwrap();
    let session = store
        .session_by_external_session(CaptureProvider::Zed, "hydrate-thread")
        .unwrap()
        .unwrap();
    let event = store.events_for_session(session.id).unwrap().remove(0);
    assert_eq!(
        event.sync.metadata["source_record_subrecord_index"],
        json!(2)
    );
    assert_eq!(
        event.payload["text_retention"]["limit_chars"],
        json!(PROVIDER_MAX_TEXT_CHARS)
    );
    assert_eq!(event.payload["text_retention"]["truncated"], json!(true));
    assert_eq!(
        event.payload["text"].as_str().unwrap().chars().count(),
        PROVIDER_MAX_TEXT_CHARS
    );
    let locators = VerifiedContentLocatorsV1::from_metadata_value(
        &event.sync.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
    )
    .unwrap();
    assert!(locators.locator(VerifiedContentRole::MessageBody).is_some());

    let request = complete_message_request(&path, &event);
    let complete = SqliteCompleteContentResolver::new()
        .resolve(std::slice::from_ref(&request))
        .unwrap();
    assert_eq!(complete[0].text, full_text);

    let connection = Connection::open(&path).unwrap();
    let mutated = format!("{}-mutated", "y".repeat(PROVIDER_MAX_TEXT_CHARS));
    update_thread(
        &connection,
        "hydrate-thread",
        "json",
        &thread(vec![
            result_only_message("omitted-result-a", "private result A"),
            result_only_message("omitted-result-b", "private result B"),
            user("hydrate-user", &mutated),
        ]),
    );
    drop(connection);
    let mutated_request = complete_message_request(&path, &event);
    let error = SqliteCompleteContentResolver::new()
        .resolve(std::slice::from_ref(&mutated_request))
        .unwrap_err();
    assert_eq!(
        error.kind,
        CompleteContentErrorKind::ContentVerificationFailed
    );
}

#[test]
fn rejection_total_is_exact_beyond_the_bounded_failure_samples() {
    const REJECTIONS: usize = crate::summaries::MAX_RETAINED_PROVIDER_FAILURES + 17;

    let directory = tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let mut connection = new_database(&path);
    let transaction = connection.transaction().unwrap();
    for index in 0..REJECTIONS {
        transaction
            .execute(
                "INSERT INTO threads (
                     id, summary, updated_at, data_type, data, created_at
                 ) VALUES (?1, 'malformed', '2026-07-24T12:00:10Z',
                           'json', CAST('{\"messages\":[' AS BLOB),
                           '2026-07-24T12:00:00Z')",
                [format!("rejection-{index:03}")],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
    drop(connection);

    let mut store = ctx_history_store::Store::open(directory.path().join("store.sqlite")).unwrap();
    let summary = import_zed_nativepath(
        &path,
        &mut store,
        adapter_context(&path, "zed-rejection-test-machine"),
        crate::ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(summary.failed, REJECTIONS);
    assert_eq!(
        summary.failures.len(),
        crate::summaries::MAX_RETAINED_PROVIDER_FAILURES
    );
}

#[test]
fn temporary_sqlite_full_is_system_io_but_provider_schema_is_typed() {
    let disk_full = rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL),
        Some("temporary staging is full".to_owned()),
    );
    let system = into_capture_error(ZedNativePathError::SystemSqlite {
        operation: "using Zed NativePath staging",
        source: disk_full,
    });
    assert!(matches!(system, crate::CaptureError::SystemIo { .. }));

    let source_database =
        into_capture_error(ZedNativePathError::Sqlite(rusqlite::Error::InvalidQuery));
    assert!(matches!(
        source_database,
        crate::CaptureError::Sqlite(rusqlite::Error::InvalidQuery)
    ));

    let schema = into_capture_error(ZedNativePathError::UnsupportedSchema(
        "required threads table is missing".to_owned(),
    ));
    assert!(matches!(
        schema,
        crate::CaptureError::UnsupportedSchema(ref reason)
            if reason == "required threads table is missing"
    ));

    let directory = tempdir().unwrap();
    let path = directory.path().join("unsupported.db");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute("CREATE TABLE unrelated (value TEXT)", [])
        .unwrap();
    drop(connection);
    let mut store = ctx_history_store::Store::open(directory.path().join("store.sqlite")).unwrap();
    let import_error = import_zed_nativepath(
        &path,
        &mut store,
        adapter_context(&path, "zed-schema-test-machine"),
        crate::ProviderImportOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(
        import_error,
        crate::CaptureError::UnsupportedSchema(ref reason)
            if reason.contains("threads")
    ));
}

#[cfg(unix)]
#[test]
fn cross_device_relocation_reuses_identity_only_after_prior_path_is_missing() {
    use std::os::unix::fs::MetadataExt;

    let directory = tempdir().unwrap();
    let original_path = directory.path().join("threads.db");
    let connection = new_database(&original_path);
    insert_thread(
        &connection,
        "relocated-thread",
        None,
        "json",
        &thread(vec![user("relocated-user", "same bytes")]),
    );
    drop(connection);
    let relocated_directory = match tempfile::Builder::new()
        .prefix("ctx-zed-cross-device-")
        .tempdir_in("/dev/shm")
    {
        Ok(directory) => directory,
        Err(_) => return,
    };
    if fs::metadata(directory.path()).unwrap().dev()
        == fs::metadata(relocated_directory.path()).unwrap().dev()
    {
        return;
    }
    let relocated_path = relocated_directory.path().join("threads.db");
    fs::copy(&original_path, &relocated_path).unwrap();

    let mut guard_store =
        ctx_history_store::Store::open(directory.path().join("guard-store.sqlite")).unwrap();
    import_zed_nativepath(
        &original_path,
        &mut guard_store,
        adapter_context(&original_path, "zed-relocation-guard-machine"),
        crate::ProviderImportOptions::default(),
    )
    .unwrap();
    import_zed_nativepath(
        &relocated_path,
        &mut guard_store,
        adapter_context(&relocated_path, "zed-relocation-guard-machine"),
        crate::ProviderImportOptions::default(),
    )
    .unwrap();
    let guarded_identities = guard_store
        .list_capture_sources()
        .unwrap()
        .into_iter()
        .filter_map(|source| source.descriptor.source_identity)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(guarded_identities.len(), 2);
    drop(guard_store);

    let store_path = directory.path().join("store.sqlite");
    let mut store = ctx_history_store::Store::open(&store_path).unwrap();
    import_zed_nativepath(
        &original_path,
        &mut store,
        adapter_context(&original_path, "zed-relocation-test-machine"),
        crate::ProviderImportOptions::default(),
    )
    .unwrap();
    let before = store
        .session_by_external_session(CaptureProvider::Zed, "relocated-thread")
        .unwrap()
        .unwrap();
    let source_identity = store
        .list_capture_sources()
        .unwrap()
        .into_iter()
        .find(|source| source.id == before.capture_source_id.unwrap())
        .unwrap()
        .descriptor
        .source_identity
        .unwrap();

    fs::remove_file(&original_path).unwrap();
    let summary = import_zed_nativepath(
        &relocated_path,
        &mut store,
        adapter_context(&relocated_path, "zed-relocation-test-machine"),
        crate::ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(
        summary.work_result(),
        crate::ProviderImportWorkResult::Changed
    );
    let after = store
        .session_by_external_session(CaptureProvider::Zed, "relocated-thread")
        .unwrap()
        .unwrap();
    assert_eq!(after.id, before.id);
    assert_eq!(after.capture_source_id, before.capture_source_id);
    let relocated_source = store
        .list_capture_sources()
        .unwrap()
        .into_iter()
        .find(|source| source.id == after.capture_source_id.unwrap())
        .unwrap();
    assert_eq!(
        relocated_source.descriptor.source_identity.as_deref(),
        Some(source_identity.as_str())
    );
    assert_eq!(
        relocated_source.descriptor.raw_source_path.as_deref(),
        Some(relocated_path.to_string_lossy().as_ref())
    );
}
