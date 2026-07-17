#[test]
fn manifested_completion_requires_exact_post_import_observation() {
    let temp = tempfile::tempdir().unwrap();
    let source_path = temp.path().join("sessions");
    fs::create_dir(&source_path).unwrap();
    fs::write(source_path.join("messages.jsonl"), b"{}\n").unwrap();
    fs::write(source_path.join("meta.json"), b"{}\n").unwrap();
    let companion_modified = fs::metadata(source_path.join("meta.json"))
        .unwrap()
        .modified()
        .unwrap();
    let source = explicit_path_source(CaptureProvider::MistralVibe, source_path.clone());
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let (files, inventory_generation) = finalized_source_import_preinventory(&store, &source);
    let mut import_file = |_store: &mut Store, _pending_source: &SourceInfo| {
        let meta_path = source_path.join("meta.json");
        fs::write(&meta_path, b"[]\n").unwrap();
        fs::File::options()
            .write(true)
            .open(&meta_path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(companion_modified))
            .unwrap();
        Ok(successful_file_summary())
    };

    let summary = import_manifested_source_with_importer(
        &mut store,
        &source,
        ManifestedImportOptions::new(Some(&files), Some(inventory_generation), false, None),
        &mut import_file,
    )
    .unwrap();

    assert_eq!(summary.imported_events, 1);
    assert_eq!(summary.completed_units, 0);
    assert_eq!(summary.deferred_units, 0);
    assert!(summary.post_import_inventory_generation.is_some());
    let pending = store
        .list_pending_source_import_files(source.provider, source.path.to_str().unwrap())
        .unwrap();
    assert_eq!(pending.len(), 1, "changed companion must remain pending");
    assert_eq!(store.source_import_file_counts().unwrap().indexed, 0);
}

#[test]
fn manifested_current_result_reports_exact_completion_and_post_import_generation() {
    let temp = tempdir();
    let source_path = temp.path().join("sessions");
    fs::create_dir(&source_path).unwrap();
    fs::write(source_path.join("messages.jsonl"), b"{}\n").unwrap();
    let source = explicit_path_source(CaptureProvider::MistralVibe, source_path);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let (files, inventory_generation) = finalized_source_import_preinventory(&store, &source);
    assert_eq!(files.len(), 1);
    let mut import_file =
        |_store: &mut Store, _pending_source: &SourceInfo| Ok(successful_file_summary());

    let outcome = import_manifested_source_with_importer(
        &mut store,
        &source,
        ManifestedImportOptions::new(Some(&files), Some(inventory_generation), false, None),
        &mut import_file,
    )
    .unwrap();

    assert_eq!(outcome.completed_units, 1);
    assert_eq!(outcome.deferred_units, 0);
    assert_eq!(
        outcome.post_import_inventory_generation,
        Some(inventory_generation)
    );
    assert!(outcome.post_import_preinventory.is_none());
    assert_eq!(outcome.imported_events, 1);
}

#[test]
fn manifested_130_unit_drain_keeps_one_generation_across_three_slices() {
    let temp = tempdir();
    let source_path = temp.path().join("sessions");
    fs::create_dir(&source_path).unwrap();
    for index in 0..130 {
        let session_path = source_path.join(format!("session-{index:03}"));
        fs::create_dir(&session_path).unwrap();
        fs::write(session_path.join("messages.jsonl"), b"{}\n").unwrap();
    }
    let source = explicit_path_source(CaptureProvider::MistralVibe, source_path);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let (files, inventory_generation) = finalized_source_import_preinventory(&store, &source);
    assert_eq!(files.len(), 130);
    let plan = ImportPlan::build(
        &store,
        vec![PlannedImportSource {
            source: source.clone(),
            stats: SourceStats::default(),
            preinventory: SourcePreinventory::SourceImportFiles {
                files: files.clone(),
                inventory_generation,
            },
        }],
    )
    .unwrap();
    assert_eq!(plan.fresh_units, 130);
    let mut completed = 0usize;
    let mut slices = 0usize;
    let mut import_file =
        |_store: &mut Store, _pending_source: &SourceInfo| Ok(successful_file_summary());

    loop {
        let slice = plan
            .select_slice(&store, ImportWorkClass::Fresh, IMPORT_SLICE_MAX_UNITS)
            .unwrap();
        if slice.is_empty() {
            break;
        }
        slices += 1;
        let selected = &slice.sources[0];
        let outcome = import_manifested_source_with_importer(
            &mut store,
            &source,
            ManifestedImportOptions::new(
                Some(&files),
                Some(inventory_generation),
                false,
                Some(&selected.work),
            ),
            &mut import_file,
        )
        .unwrap();
        assert_eq!(
            outcome.post_import_inventory_generation,
            Some(inventory_generation)
        );
        completed += outcome.completed_units;
    }

    assert_eq!(slices, 3);
    assert_eq!(completed, 130);
    assert!(store
        .list_source_import_file_work(
            source.provider,
            source.path.to_str().unwrap(),
            ImportWorkClass::Fresh,
            1,
        )
        .unwrap()
        .is_empty());
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
    let selected = final_observation_system_error::<()>(Err(observation_error))
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
    let (files, inventory_generation) = finalized_source_import_preinventory(&store, &source);
    let mut import_file = |_store: &mut Store, _pending_source: &SourceInfo| {
        fs::remove_dir_all(&source_path).unwrap();
        Err(anyhow::Error::new(CaptureError::SystemInvariant(
            "original manifested system failure",
        )))
    };

    let error = import_manifested_source_with_importer(
        &mut store,
        &source,
        ManifestedImportOptions::new(Some(&files), Some(inventory_generation), false, None),
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
    let (files, inventory_generation) = finalized_source_import_preinventory(&store, &source);
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
        ManifestedImportOptions::new(Some(&files), Some(inventory_generation), false, None),
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
fn manifested_terminal_file_failure_preserves_sibling_success() {
    let temp = tempdir();
    let source_path = temp.path().join("sessions");
    fs::create_dir(&source_path).unwrap();
    fs::write(source_path.join("good.jsonl"), b"{}\n").unwrap();
    fs::write(source_path.join("bad.jsonl"), b"{}\n").unwrap();
    let source = explicit_path_source(CaptureProvider::Pi, source_path);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let (files, inventory_generation) = finalized_source_import_preinventory(&store, &source);
    let mut import_file = |_store: &mut Store, pending_source: &SourceInfo| {
        if pending_source.path.ends_with("bad.jsonl") {
            return Err(anyhow::Error::new(
                CaptureError::InvalidProviderTranscriptPath {
                    path: pending_source.path.clone(),
                    reason: "deterministic malformed fixture",
                },
            ));
        }
        Ok(successful_file_summary())
    };

    let outcome = import_manifested_source_with_importer(
        &mut store,
        &source,
        ManifestedImportOptions::new(Some(&files), Some(inventory_generation), false, None),
        &mut import_file,
    )
    .unwrap();

    assert_eq!(outcome.completed_units, 2);
    assert_eq!(outcome.summary.imported_events, 1);
    assert_eq!(outcome.summary.failed, 1);
    assert_eq!(outcome.summary.failures.len(), 1);
    assert!(outcome.summary.failures[0].error.contains("bad.jsonl"));
}

#[test]
fn manifested_retryable_file_failure_preserves_original_error_after_sibling_success() {
    let temp = tempdir();
    let source_path = temp.path().join("sessions");
    fs::create_dir(&source_path).unwrap();
    fs::write(source_path.join("good.jsonl"), b"{}\n").unwrap();
    fs::write(source_path.join("retry.jsonl"), b"{}\n").unwrap();
    let source = explicit_path_source(CaptureProvider::Pi, source_path);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let (files, inventory_generation) = finalized_source_import_preinventory(&store, &source);
    let mut import_file = |_store: &mut Store, pending_source: &SourceInfo| {
        if pending_source.path.ends_with("retry.jsonl") {
            return Err(anyhow::Error::new(CaptureError::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "retry this exact file",
            ))));
        }
        Ok(successful_file_summary())
    };

    let error = import_manifested_source_with_importer(
        &mut store,
        &source,
        ManifestedImportOptions::new(Some(&files), Some(inventory_generation), false, None),
        &mut import_file,
    )
    .expect_err("a retryable manifested file failure must remain retryable");

    let partial = error
        .downcast::<ProviderImportBatchError>()
        .expect("sibling success must be attached to the remaining retryable error");
    let (outcome, error) = partial.into_parts();
    assert_eq!(outcome.completed_units, 1);
    assert_eq!(outcome.summary.imported_events, 1);
    assert!(outcome.made_durable_progress());
    assert!(matches!(
        error.downcast_ref::<CaptureError>(),
        Some(CaptureError::Io(source)) if source.kind() == std::io::ErrorKind::TimedOut
    ));
    assert!(error.to_string().contains("retry this exact file"));
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
    let (files, inventory_generation) = finalized_source_import_preinventory(&store, &source);
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

    let outcome = import_manifested_source_with_importer(
        &mut store,
        &source,
        ManifestedImportOptions::new(Some(&files), Some(inventory_generation), false, None),
        &mut import_file,
    )
    .unwrap();

    assert!(writer.is_some());
    assert_eq!(outcome.completed_units, 0);
    assert_eq!(outcome.deferred_units, 0);
    assert!(outcome.post_import_inventory_generation.is_some());
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
    let (files, inventory_generation) = finalized_source_import_preinventory(&store, &source);
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

    let outcome = import_manifested_source_with_importer(
        &mut store,
        &source,
        ManifestedImportOptions::new(Some(&files), Some(inventory_generation), false, None),
        &mut import_file,
    )
    .unwrap();

    assert_eq!(outcome.completed_units, 0);
    assert!(outcome.post_import_inventory_generation.is_some());
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
    let (files, inventory_generation) = finalized_source_import_preinventory(&store, &source);
    let mut import_file = |_store: &mut Store, _pending_source: &SourceInfo| {
        fs::remove_file(&old_path).unwrap();
        fs::write(&new_path, b"{}\n").unwrap();
        Ok(successful_file_summary())
    };

    let outcome = import_manifested_source_with_importer(
        &mut store,
        &source,
        ManifestedImportOptions::new(Some(&files), Some(inventory_generation), false, None),
        &mut import_file,
    )
    .unwrap();

    assert_eq!(outcome.completed_units, 0);
    assert!(outcome.post_import_inventory_generation.is_some());
    let pending = store
        .list_pending_source_import_files(source.provider, source.path.to_str().unwrap())
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].source_path, old_path.display().to_string());
    let _ = finalized_source_import_preinventory(&store, &source);
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
fn removed_manifest_unit_reports_empty_source_after_reobservation() {
    let temp = tempdir();
    let source_path = temp.path().join("sessions");
    fs::create_dir(&source_path).unwrap();
    let removed_path = source_path.join("session.jsonl");
    fs::write(&removed_path, b"{}\n").unwrap();
    let source = explicit_path_source(CaptureProvider::Pi, source_path);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let (files, inventory_generation) = finalized_source_import_preinventory(&store, &source);
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
        ManifestedImportOptions::new(Some(&files), Some(inventory_generation), false, None),
        &mut import_file,
    )
    .unwrap();

    assert_eq!(summary.summary, ProviderImportSummary::default());
    assert_eq!(summary.completed_units, 0);
    assert_eq!(summary.deferred_units, 0);
    assert!(summary.post_import_inventory_generation.is_some());
    let counts = store.source_import_file_counts().unwrap();
    assert_eq!(counts.stale, 0);
    assert_eq!(counts.failed, 0);
    assert_eq!(counts.rejected, 0);
    let inventory = inventory_import_sources(&store, vec![source], false).unwrap();
    assert!(inventory.sources.is_empty());
    assert_eq!(inventory.failures.len(), 1);
    assert!(inventory.failures[0].error.contains("no importable"));
    let counts = store.source_import_file_counts().unwrap();
    assert_eq!(counts.stale, 0);
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
    persist_source_material(&store, &file);
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

#[test]
fn setup_drain_imports_later_complete_files_despite_an_incomplete_earlier_file() {
    let temp = tempdir();
    let source_root = temp.path().join("pi-fair-source");
    let source = write_pi_source(&source_root, "partial-first");
    let data_root = temp.path().join("data");
    let args = crate::ImportArgs {
        provider: Some(NativeProviderArg::Pi),
        path: Some(source.path.clone()),
        history_source: None,
        history_source_manifest: Vec::new(),
        reset_cursor: false,
        format: None,
        all: false,
        resume: false,
        no_daemon: true,
        json: false,
        progress: ProgressArg::None,
    };
    let run = |data_root: PathBuf| {
        crate::commands::import::run_import_internal(
            &args,
            data_root,
            &mut serde_json::Map::new(),
            crate::commands::import::ImportRunOptions {
                progress: ProgressArg::None,
                json: false,
                print_human: false,
                allow_empty_sources: false,
                include_history_source_plugins: false,
                operation: "setup",
            },
        )
        .unwrap()
    };
    let baseline = run(data_root.clone());
    assert_eq!(baseline.totals.fresh_units_pending, 0, "{baseline:?}");
    fs::OpenOptions::new()
        .append(true)
        .open(source_root.join("session.jsonl"))
        .unwrap()
        .write_all(br#"{"type":"message","id":"partial""#)
        .unwrap();
    fs::write(
        source_root.join("later.jsonl"),
        format!(
            "{}{}",
            jsonl(json!({
                "type": "session",
                "id": "complete-later",
                "timestamp": "2026-07-14T12:00:00Z"
            })),
            jsonl(json!({
                "type": "message",
                "id": "complete-later-message",
                "timestamp": "2026-07-14T12:00:01Z",
                "message": {"role": "user", "content": "complete later foreground content"}
            }))
        ),
    )
    .unwrap();

    let report = run(data_root.clone());

    assert_eq!(report.totals.fresh_units_processed, 1, "{report:?}");
    assert_eq!(report.totals.fresh_units_pending, 1, "{report:?}");
    let store = Store::open(ctx_history_core::database_path(data_root)).unwrap();
    assert!(serde_json::to_string(&store.export_archive().unwrap())
        .unwrap()
        .contains("complete later foreground content"));
    assert!(!store.has_pending_provider_file_publications().unwrap());
}
