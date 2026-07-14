#[test]
fn manifested_completion_requires_exact_post_import_observation() {
    let temp = tempfile::tempdir().unwrap();
    let source_path = temp.path().join("sessions");
    fs::create_dir(&source_path).unwrap();
    fs::write(source_path.join("messages.jsonl"), b"{}\n").unwrap();
    fs::write(source_path.join("meta.json"), b"{}\n").unwrap();
    let source = explicit_path_source(CaptureProvider::MistralVibe, source_path.clone());
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let mut import_file = |_store: &mut Store, _pending_source: &SourceInfo| {
        let mut meta = fs::OpenOptions::new()
            .append(true)
            .open(source_path.join("meta.json"))
            .unwrap();
        use std::io::Write as _;
        meta.write_all(b"changed\n").unwrap();
        meta.sync_all().unwrap();
        Ok(successful_file_summary())
    };

    let summary = import_manifested_source_with_importer(
        &mut store,
        &source,
        new_id(),
        None,
        None,
        false,
        &mut import_file,
    )
    .unwrap();

    assert_eq!(summary.imported_events, 1);
    let pending = store
        .list_pending_source_import_files(source.provider, source.path.to_str().unwrap())
        .unwrap();
    assert_eq!(pending.len(), 1, "changed companion must remain pending");
    assert_eq!(store.source_import_file_counts().unwrap().indexed, 0);
}

#[test]
fn whole_source_completion_requires_exact_post_import_observation() {
    let temp = tempdir();
    let source_path = temp.path().join("state.json");
    fs::write(&source_path, b"before").unwrap();
    let source = explicit_path_source(CaptureProvider::CodeBuddy, source_path.clone());
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let (_, observed) = observe_source_root(&source).unwrap();
    let persisted =
        persist_new_source_import_observation(&store, &source, std::slice::from_ref(&observed))
            .unwrap();
    fs::write(&source_path, b"after with a different size").unwrap();

    persist_reobserved_source_root_result(
        &store,
        &source,
        &SourcePreinventory::SourceRoot {
            file: observed,
            inventory_generation: persisted.inventory_generation,
        },
        CatalogIndexedStatus::Indexed,
        "",
    )
    .unwrap();

    assert_eq!(
        store
            .list_pending_source_import_files(source.provider, source.path.to_str().unwrap())
            .unwrap()
            .len(),
        1,
        "a changed whole-source observation must remain pending"
    );
}

#[test]
fn whole_source_post_import_observation_failure_is_not_success() {
    let temp = tempdir();
    let source_path = temp.path().join("state.json");
    fs::write(&source_path, b"before").unwrap();
    let source = explicit_path_source(CaptureProvider::CodeBuddy, source_path.clone());
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let (_, observed) = observe_source_root(&source).unwrap();
    let persisted =
        persist_new_source_import_observation(&store, &source, std::slice::from_ref(&observed))
            .unwrap();
    fs::remove_file(&source_path).unwrap();

    persist_reobserved_source_root_result(
        &store,
        &source,
        &SourcePreinventory::SourceRoot {
            file: observed,
            inventory_generation: persisted.inventory_generation,
        },
        CatalogIndexedStatus::Indexed,
        "",
    )
    .expect_err("a missing post-import source cannot be reported as complete");
}

#[test]
fn whole_source_store_failure_takes_precedence_over_source_failure() {
    let observation_error = anyhow::Error::new(StoreError::BulkSearchImportBusy);
    let selected = final_observation_system_error(Err(observation_error))
        .expect("a store failure during final observation must abort the whole run");

    assert_eq!(import_error_scope(&selected), ImportFailureScope::System);
    assert!(selected.downcast_ref::<StoreError>().is_some());
}

#[test]
fn manifested_system_error_survives_failed_post_observation() {
    let temp = tempdir();
    let source_path = temp.path().join("sessions");
    fs::create_dir(&source_path).unwrap();
    fs::write(source_path.join("messages.jsonl"), b"{}\n").unwrap();
    let source = explicit_path_source(CaptureProvider::MistralVibe, source_path.clone());
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let mut import_file = |_store: &mut Store, _pending_source: &SourceInfo| {
        fs::remove_dir_all(&source_path).unwrap();
        Err(anyhow::Error::new(CaptureError::SystemInvariant(
            "original manifested system failure",
        )))
    };

    let error = import_manifested_source_with_importer(
        &mut store,
        &source,
        new_id(),
        None,
        None,
        false,
        &mut import_file,
    )
    .expect_err("the original system error must abort the manifested import");

    assert!(matches!(
        error.downcast_ref::<CaptureError>(),
        Some(CaptureError::SystemInvariant(
            "original manifested system failure"
        ))
    ));
}

#[test]
fn manifested_source_error_keeps_its_typed_identity() {
    let temp = tempdir();
    let source_path = temp.path().join("sessions");
    fs::create_dir(&source_path).unwrap();
    fs::write(source_path.join("messages.jsonl"), b"{}\n").unwrap();
    let source = explicit_path_source(CaptureProvider::MistralVibe, source_path);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let mut import_file = |_store: &mut Store, _pending_source: &SourceInfo| {
        Err(anyhow::Error::new(CaptureError::Sqlite(
            rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                Some("provider database is busy".to_owned()),
            ),
        )))
    };

    let error = import_manifested_source_with_importer(
        &mut store,
        &source,
        new_id(),
        None,
        None,
        false,
        &mut import_file,
    )
    .expect_err("a source adapter failure must remain an error");

    assert!(matches!(
        error.downcast_ref::<CaptureError>(),
        Some(CaptureError::Sqlite(rusqlite::Error::SqliteFailure(
            code,
            _
        ))) if code.code == rusqlite::ErrorCode::DatabaseBusy
    ));
    assert_eq!(import_error_scope(&error), ImportFailureScope::Source);
}

#[test]
fn manifested_sqlite_change_during_import_remains_pending() {
    let temp = tempfile::tempdir().unwrap();
    let source_dir = temp.path().join("provider");
    fs::create_dir(&source_dir).unwrap();
    let source_path = source_dir.join("opencode.db");
    let setup = rusqlite::Connection::open(&source_path).unwrap();
    setup
        .execute_batch(
            "CREATE TABLE entries (id INTEGER PRIMARY KEY, value TEXT);\n\
             INSERT INTO entries VALUES (1, 'before');",
        )
        .unwrap();
    drop(setup);
    let source = explicit_path_source(CaptureProvider::OpenCode, source_path.clone());
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let mut writer = None;
    let mut import_file = |_store: &mut Store, _pending_source: &SourceInfo| {
        let conn = rusqlite::Connection::open(&source_path).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        conn.execute_batch("PRAGMA wal_autocheckpoint = 0").unwrap();
        conn.execute("UPDATE entries SET value = 'after' WHERE id = 1", [])
            .unwrap();
        writer = Some(conn);
        Ok(successful_file_summary())
    };

    import_manifested_source_with_importer(
        &mut store,
        &source,
        new_id(),
        None,
        None,
        false,
        &mut import_file,
    )
    .unwrap();

    assert!(writer.is_some());
    assert_eq!(
        store
            .list_pending_source_import_files(source.provider, source.path.to_str().unwrap())
            .unwrap()
            .len(),
        1,
        "a new WAL generation must fence completion"
    );
}

#[cfg(unix)]
#[test]
fn optional_companion_directory_change_invalidates_the_absence_watch() {
    let temp = tempfile::tempdir().unwrap();
    let source_path = temp.path().join("sessions");
    fs::create_dir(&source_path).unwrap();
    fs::write(source_path.join("messages.jsonl"), b"{}\n").unwrap();
    let initial_modified = fs::metadata(&source_path).unwrap().modified().unwrap();
    let source = explicit_path_source(CaptureProvider::MistralVibe, source_path.clone());
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let mut import_file = |_store: &mut Store, _pending_source: &SourceInfo| {
        let companion = source_path.join("meta.json");
        fs::write(&companion, b"temporary companion").unwrap();
        fs::remove_file(&companion).unwrap();
        fs::File::open(&source_path)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new().set_modified(
                    initial_modified
                        .checked_add(std::time::Duration::from_secs(5))
                        .unwrap(),
                ),
            )
            .unwrap();
        Ok(successful_file_summary())
    };

    import_manifested_source_with_importer(
        &mut store,
        &source,
        new_id(),
        None,
        None,
        false,
        &mut import_file,
    )
    .unwrap();

    assert_eq!(
        store
            .list_pending_source_import_files(source.provider, source.path.to_str().unwrap())
            .unwrap()
            .len(),
        1,
        "observed companion-directory churn must not complete the old absence"
    );
}

#[test]
fn removed_and_new_manifest_units_are_stale_and_pending() {
    let temp = tempfile::tempdir().unwrap();
    let source_path = temp.path().join("sessions");
    fs::create_dir(&source_path).unwrap();
    let old_path = source_path.join("old.jsonl");
    let new_path = source_path.join("new.jsonl");
    fs::write(&old_path, b"{}\n").unwrap();
    let source = explicit_path_source(CaptureProvider::Pi, source_path.clone());
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let mut import_file = |_store: &mut Store, _pending_source: &SourceInfo| {
        fs::remove_file(&old_path).unwrap();
        fs::write(&new_path, b"{}\n").unwrap();
        Ok(successful_file_summary())
    };

    import_manifested_source_with_importer(
        &mut store,
        &source,
        new_id(),
        None,
        None,
        false,
        &mut import_file,
    )
    .unwrap();

    let pending = store
        .list_pending_source_import_files(source.provider, source.path.to_str().unwrap())
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].source_path, new_path.display().to_string());
    let counts = store.source_import_file_counts().unwrap();
    assert_eq!(counts.stale, 1);
    assert_eq!(counts.pending, 1);
    assert_eq!(counts.indexed, 0);
}

#[test]
fn removed_manifest_unit_drops_its_source_failure_after_reobservation() {
    let temp = tempdir();
    let source_path = temp.path().join("sessions");
    fs::create_dir(&source_path).unwrap();
    let removed_path = source_path.join("session.jsonl");
    fs::write(&removed_path, b"{}\n").unwrap();
    let source = explicit_path_source(CaptureProvider::Pi, source_path);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let mut import_file = |_store: &mut Store, _pending_source: &SourceInfo| {
        fs::remove_file(&removed_path).unwrap();
        Err(anyhow::Error::new(CaptureError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "owner disappeared before open",
        ))))
    };

    let summary = import_manifested_source_with_importer(
        &mut store,
        &source,
        new_id(),
        None,
        None,
        false,
        &mut import_file,
    )
    .unwrap();

    assert_eq!(summary, ProviderImportSummary::default());
    let counts = store.source_import_file_counts().unwrap();
    assert_eq!(counts.stale, 1);
    assert_eq!(counts.failed, 0);
    assert_eq!(counts.rejected, 0);
}

#[test]
fn pre_summary_source_error_is_terminal_for_the_observed_revision() {
    let temp = tempfile::tempdir().unwrap();
    let source_path = temp.path().join("transcript.jsonl");
    let source = explicit_path_source(CaptureProvider::Hermes, source_path.clone());
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let file = SourceImportFile {
        provider: source.provider,
        source_format: source.source_format.to_owned(),
        import_revision: source.import_revision,
        source_root: source_path.display().to_string(),
        source_path: source_path.display().to_string(),
        file_size_bytes: 17,
        file_modified_at_ms: 23,
        observed_at_ms: 29,
        metadata: json!({}),
    };
    let inventory_generation = inventory_source_file(&store, &file);
    let error = anyhow::Error::new(CaptureError::InvalidProviderTranscriptPath {
        path: source_path,
        reason: "expected a provider transcript file",
    });

    assert!(rejected_source_summary(&error).is_none());
    let status = import_error_status(&error);
    assert_eq!(status, CatalogIndexedStatus::Rejected);
    mark_source_import_file_result(
        &store,
        &file,
        inventory_generation,
        status,
        Some(&error.to_string()),
    )
    .unwrap();

    assert!(store
        .list_pending_source_import_files(source.provider, &file.source_root)
        .unwrap()
        .is_empty());
    assert_eq!(store.source_import_file_counts().unwrap().rejected, 1);
}

#[test]
fn transient_source_io_remains_retryable_for_the_observed_revision() {
    let temp = tempfile::tempdir().unwrap();
    let source_path = temp.path().join("transcript.jsonl");
    let source = explicit_path_source(CaptureProvider::Hermes, source_path.clone());
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let file = SourceImportFile {
        provider: source.provider,
        source_format: source.source_format.to_owned(),
        import_revision: source.import_revision,
        source_root: source_path.display().to_string(),
        source_path: source_path.display().to_string(),
        file_size_bytes: 17,
        file_modified_at_ms: 23,
        observed_at_ms: 29,
        metadata: json!({}),
    };
    let inventory_generation = inventory_source_file(&store, &file);
    let error = anyhow::Error::new(CaptureError::Io(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "transient test failure",
    )));

    assert_eq!(import_error_scope(&error), ImportFailureScope::Source);
    let status = import_error_status(&error);
    assert_eq!(status, CatalogIndexedStatus::Failed);
    mark_source_import_file_result(
        &store,
        &file,
        inventory_generation,
        status,
        Some(&error.to_string()),
    )
    .unwrap();

    assert_eq!(
        store
            .list_pending_source_import_files(source.provider, &file.source_root)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(store.source_import_file_counts().unwrap().failed, 1);
}

#[test]
fn provider_sqlite_lock_is_pending_until_the_lock_is_released() {
    let temp = tempfile::tempdir().unwrap();
    let source_path = temp.path().join("provider.sqlite");
    let source = explicit_path_source(CaptureProvider::Hermes, source_path.clone());
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let file = SourceImportFile {
        provider: source.provider,
        source_format: source.source_format.to_owned(),
        import_revision: source.import_revision,
        source_root: source_path.display().to_string(),
        source_path: source_path.display().to_string(),
        file_size_bytes: 17,
        file_modified_at_ms: 23,
        observed_at_ms: 29,
        metadata: json!({}),
    };
    let inventory_generation = inventory_source_file(&store, &file);

    let lock = rusqlite::Connection::open(&source_path).unwrap();
    lock.execute_batch(
        "PRAGMA journal_mode = DELETE;
         CREATE TABLE state(value INTEGER NOT NULL);
         INSERT INTO state VALUES (1);
         BEGIN EXCLUSIVE;
         UPDATE state SET value = 2;",
    )
    .unwrap();
    let reader = rusqlite::Connection::open(&source_path).unwrap();
    reader.busy_timeout(std::time::Duration::ZERO).unwrap();
    let sqlite_error = reader
        .query_row("SELECT value FROM state", [], |row| row.get::<_, i64>(0))
        .unwrap_err();
    let error = anyhow::Error::new(CaptureError::Sqlite(sqlite_error));
    assert_eq!(import_error_scope(&error), ImportFailureScope::Source);
    let status = import_error_status(&error);
    assert_eq!(status, CatalogIndexedStatus::Failed);
    mark_source_import_file_result(
        &store,
        &file,
        inventory_generation,
        status,
        Some(&error.to_string()),
    )
    .unwrap();
    assert_eq!(
        store
            .list_pending_source_import_files(source.provider, &file.source_root)
            .unwrap()
            .len(),
        1
    );

    lock.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        reader
            .query_row("SELECT value FROM state", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    mark_source_import_file_result(
        &store,
        &file,
        inventory_generation,
        CatalogIndexedStatus::Indexed,
        None,
    )
    .unwrap();
    assert!(store
        .list_pending_source_import_files(source.provider, &file.source_root)
        .unwrap()
        .is_empty());
}
