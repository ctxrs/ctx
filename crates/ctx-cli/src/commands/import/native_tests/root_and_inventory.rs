fn persist_indexed_root(store: &Store, source: &SourceInfo) -> (SourceImportFile, u64) {
    let (_, file) = observe_source_root(source).unwrap();
    let persisted =
        persist_new_source_import_observation(store, source, std::slice::from_ref(&file)).unwrap();
    let inventory_generation = persisted.inventory_generation;
    let source_root = file.source_root.clone();
    persist_source_material(store, &file);
    let changed = store
        .mark_source_import_file_indexed(
            source.provider,
            SourceImportFileIndexUpdate {
                source_root: &source_root,
                source_path: &source_root,
                file_size_bytes: file.file_size_bytes,
                file_modified_at_ms: file.file_modified_at_ms,
                import_revision: source.import_revision,
                inventory_generation,
                metadata: &file.metadata,
                indexed_at_ms: 1,
            },
        )
        .unwrap();
    assert_eq!(
        changed, 1,
        "indexed observation must match its inventory row"
    );
    assert!(
        store
            .list_pending_source_import_files(source.provider, &source_root)
            .unwrap()
            .is_empty(),
        "indexed observation must no longer be pending"
    );
    (file, inventory_generation)
}

fn persist_source_material(store: &Store, file: &SourceImportFile) {
    let material_source_format =
        provider_canonical_material_source_format(file.provider, &file.source_format)
            .unwrap_or(&file.source_format);
    store
        .upsert_capture_source(&CaptureSource {
            id: new_id(),
            descriptor: CaptureSourceDescriptor {
                kind: CaptureSourceKind::ProviderImport,
                provider: file.provider,
                machine_id: "native-import-test".to_owned(),
                process_id: None,
                cwd: None,
                raw_source_path: Some(file.source_path.clone()),
                source_format: Some(material_source_format.to_owned()),
                source_root: Some(file.source_root.clone()),
                source_identity: None,
                external_session_id: None,
            },
            started_at: utc_now(),
            ended_at: None,
            sync: SyncMetadata {
                visibility: Visibility::LocalOnly,
                fidelity: Fidelity::Imported,
                sync_state: SyncState::LocalOnly,
                sync_version: 0,
                deleted_at: None,
                metadata: json!({}),
            },
        })
        .unwrap();
}

fn inventory_source_file(store: &Store, file: &SourceImportFile) -> u64 {
    let inventory_generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(inventory_generation, std::slice::from_ref(file))
        .unwrap();
    inventory_generation
}

fn successful_file_summary() -> ProviderImportSummary {
    let mut summary = ProviderImportSummary::default();
    summary.imported_events = 1;
    summary
}

#[test]
fn unchanged_root_source_skips_provider_normalization() {
    let temp = tempdir();
    let source_path = temp.path().join("state.db");
    fs::write(&source_path, b"").unwrap();
    let source = explicit_path_source(CaptureProvider::CodeBuddy, source_path.clone());
    assert!(!source_uses_import_file_manifest(&source));
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let (file, inventory_generation) = persist_indexed_root(&store, &source);

    let lock_attempted = std::sync::atomic::AtomicBool::new(false);
    let summary = import_one_source_inner_with_pre_lock_hook(
        &mut store,
        &source,
        None,
        false,
        false,
        &SourcePreinventory::SourceRoot {
            file,
            inventory_generation,
        },
        || lock_attempted.store(true, std::sync::atomic::Ordering::SeqCst),
    )
    .unwrap();

    assert_eq!(summary.imported_events, 0);
    assert_eq!(summary.failed, 0);
    assert!(
        !lock_attempted.load(std::sync::atomic::Ordering::SeqCst),
        "a complete preinventory should skip before global import admission"
    );
}

#[test]
fn unchanged_root_source_still_repairs_event_search_backfill() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let source_path = temp.path().join("state.db");
    fs::write(&source_path, b"").unwrap();
    let source = explicit_path_source(CaptureProvider::CodeBuddy, source_path);
    assert!(!source_uses_import_file_manifest(&source));
    let store = Store::open(&db_path).unwrap();
    let (file, inventory_generation) = persist_indexed_root(&store, &source);
    let event = Event {
        id: new_id(),
        seq: 1,
        history_record_id: None,
        session_id: None,
        run_id: None,
        event_type: EventType::Message,
        role: Some(EventRole::User),
        occurred_at: utc_now(),
        capture_source_id: None,
        payload: json!({"text": "unchanged root backfill oracle"}),
        payload_blob_id: None,
        dedupe_key: None,
        sync: SyncMetadata {
            visibility: Visibility::LocalOnly,
            fidelity: Fidelity::Imported,
            sync_state: SyncState::LocalOnly,
            sync_version: 0,
            deleted_at: None,
            metadata: json!({}),
        },
    };
    store.upsert_event(&event).unwrap();
    drop(store);
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute("DELETE FROM event_search", []).unwrap();
    drop(conn);
    let mut store = Store::open(&db_path).unwrap();
    assert!(store.event_search_projection_needs_backfill().unwrap());

    import_one_source_for_search_refresh(
        &mut store,
        &source,
        None,
        &SourcePreinventory::SourceRoot {
            file,
            inventory_generation,
        },
    )
    .unwrap();

    assert!(!store.event_search_projection_needs_backfill().unwrap());
    assert_eq!(
        store
            .search_event_hits("unchanged root backfill oracle", 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn changed_root_source_does_not_skip_provider_normalization() {
    let temp = tempdir();
    let source_path = temp.path().join("state.db");
    fs::write(&source_path, b"").unwrap();
    let source = explicit_path_source(CaptureProvider::Hermes, source_path.clone());
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    persist_indexed_root(&store, &source);
    std::fs::write(&source_path, b"not a sqlite database").unwrap();
    let (_, changed) = observe_source_root(&source).unwrap();
    let persisted =
        persist_new_source_import_observation(&store, &source, std::slice::from_ref(&changed))
            .unwrap();
    let inventory_generation = persisted.inventory_generation;

    let result = import_one_source_for_search_refresh(
        &mut store,
        &source,
        None,
        &SourcePreinventory::SourceRoot {
            file: changed,
            inventory_generation,
        },
    );

    assert!(
        result.is_err(),
        "changed source must reach the Hermes adapter"
    );
}

#[test]
fn full_rescan_does_not_skip_unchanged_root_source() {
    let temp = tempdir();
    let source_path = temp.path().join("state.db");
    std::fs::write(&source_path, b"not a sqlite database").unwrap();
    let source = explicit_path_source(CaptureProvider::Hermes, source_path);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let (file, inventory_generation) = persist_indexed_root(&store, &source);

    let result = import_one_source_inner(
        &mut store,
        &source,
        None,
        false,
        true,
        &SourcePreinventory::SourceRoot {
            file,
            inventory_generation,
        },
    );

    assert!(result.is_err(), "full rescan must reach the Hermes adapter");
}

#[test]
fn stale_root_plan_skips_after_newer_completion_wins_bulk_lock() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("work.sqlite");
    let source_path = temp.path().join("state.db");
    fs::write(&source_path, b"not a sqlite database").unwrap();
    let source = explicit_path_source(CaptureProvider::Hermes, source_path.clone());
    let (stats, observation) = observe_source_root(&source).unwrap();
    let lock_store = Store::open(&db_path).unwrap();
    let old_generation = inventory_source_file(&lock_store, &observation);
    let plan = ImportPlan::build(
        &lock_store,
        vec![PlannedImportSource {
            source: source.clone(),
            stats,
            preinventory: SourcePreinventory::SourceRoot {
                file: observation.clone(),
                inventory_generation: old_generation,
            },
        }],
    )
    .unwrap();
    assert_eq!(plan.fresh_units, 1);
    let guard = lock_store.begin_event_search_bulk_mode().unwrap();
    let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
    let waiting_db_path = db_path.clone();
    let importer = std::thread::spawn(move || {
        let mut import_store = Store::open(waiting_db_path).unwrap();
        let progress = ProgressReporter::new(ProgressArg::None, false, "test-import", 0);
        let mut totals = ImportTotals::default();
        let mut imported_sources = Vec::new();
        let mut successful_sources = BTreeSet::new();
        let mut failed_sources = BTreeSet::new();
        let mut execution_state = ImportExecutionState::for_plan(&plan);
        let result = execute_import_plan_class_for_report_with_pre_lock_hook(
            &mut import_store,
            &plan,
            &mut execution_state,
            ImportWorkClass::Fresh,
            plan.fresh_units,
            &progress,
            ImportRunOptions {
                progress: ProgressArg::None,
                json: false,
                print_human: false,
                allow_empty_sources: false,
                include_history_source_plugins: false,
                operation: "test-import",
            },
            &mut totals,
            &mut imported_sources,
            &mut successful_sources,
            &mut failed_sources,
            || waiting_tx.send(()).unwrap(),
        );
        (result, totals, imported_sources)
    });
    waiting_rx.recv().unwrap();

    let new_generation = inventory_source_file(&lock_store, &observation);
    persist_source_material(&lock_store, &observation);
    mark_source_import_file_result(
        &lock_store,
        &observation,
        new_generation,
        CatalogIndexedStatus::Indexed,
        None,
    )
    .unwrap();
    lock_store.finish_event_search_bulk_mode(&guard).unwrap();
    drop(guard);

    let (result, totals, imported_sources) = importer.join().unwrap();
    result.unwrap();
    assert_eq!(totals.fresh_units_processed, 0);
    assert_eq!(totals.failed_sources, 0);
    assert!(imported_sources.is_empty());
}

#[test]
fn waiting_root_plan_reobserves_a_change_before_skipping() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let source_path = temp.path().join("pi.jsonl");
    fs::write(
        &source_path,
        format!(
            "{}{}",
            jsonl(json!({
                "type": "session",
                "id": "pi-waiting-change",
                "timestamp": "2026-07-14T12:00:00Z"
            })),
            jsonl(json!({
                "type": "message",
                "id": "pi-waiting-change-user",
                "timestamp": "2026-07-14T12:00:01Z",
                "message": {"role": "user", "content": "initial"}
            }))
        ),
    )
    .unwrap();
    let source = explicit_path_source(CaptureProvider::Pi, source_path.clone());
    let mut lock_store = Store::open(&db_path).unwrap();
    let (_, initial_checkpoint) = run_single_fresh_unit(&mut lock_store, source.clone());
    fs::OpenOptions::new()
        .append(true)
        .open(&source_path)
        .unwrap()
        .write_all(
            jsonl(json!({
                "type": "message",
                "id": "pi-waiting-change-assistant",
                "timestamp": "2026-07-14T12:00:02Z",
                "message": {"role": "assistant", "content": "first pending append"}
            }))
            .as_bytes(),
        )
        .unwrap();
    let inventory = inventory_import_sources(&lock_store, vec![source.clone()], false).unwrap();
    let plan = ImportPlan::build(&lock_store, inventory.sources).unwrap();
    assert_eq!(plan.fresh_units, 1);
    let guard = lock_store.begin_event_search_bulk_mode().unwrap();
    let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
    let waiting_db_path = db_path.clone();
    let importer = std::thread::spawn(move || {
        let mut import_store = Store::open(waiting_db_path).unwrap();
        let progress = ProgressReporter::new(ProgressArg::None, false, "test-import", 0);
        let mut totals = ImportTotals::default();
        let mut imported_sources = Vec::new();
        let mut successful_sources = BTreeSet::new();
        let mut failed_sources = BTreeSet::new();
        let mut execution_state = ImportExecutionState::for_plan(&plan);
        let result = execute_import_plan_class_for_report_with_pre_lock_hook(
            &mut import_store,
            &plan,
            &mut execution_state,
            ImportWorkClass::Fresh,
            plan.fresh_units,
            &progress,
            ImportRunOptions {
                progress: ProgressArg::None,
                json: false,
                print_human: false,
                allow_empty_sources: false,
                include_history_source_plugins: false,
                operation: "test-import",
            },
            &mut totals,
            &mut imported_sources,
            &mut successful_sources,
            &mut failed_sources,
            || waiting_tx.send(()).unwrap(),
        );
        (result, totals)
    });
    waiting_rx.recv().unwrap();

    fs::OpenOptions::new()
        .append(true)
        .open(&source_path)
        .unwrap()
        .write_all(
            jsonl(json!({
                "type": "message",
                "id": "pi-waiting-change-user-2",
                "timestamp": "2026-07-14T12:00:03Z",
                "message": {"role": "user", "content": "second append while waiting"}
            }))
            .as_bytes(),
        )
        .unwrap();
    let current_len = fs::metadata(&source_path).unwrap().len();
    lock_store.finish_event_search_bulk_mode(&guard).unwrap();
    drop(guard);

    let (result, totals) = importer.join().unwrap();
    result.unwrap();
    assert_eq!(totals.fresh_units_processed, 1);
    assert_eq!(totals.failed_sources, 0);
    assert!(totals.imported_events > 0);
    assert!(lock_store
        .list_source_import_file_work(
            source.provider,
            &initial_checkpoint.source_root,
            ImportWorkClass::Fresh,
            10,
        )
        .unwrap()
        .is_empty());
    assert!(lock_store
        .list_source_import_file_work(
            source.provider,
            &initial_checkpoint.source_root,
            ImportWorkClass::Recovery,
            10,
        )
        .unwrap()
        .is_empty());
    let current_checkpoint = lock_store
        .provider_file_checkpoint(ProviderFileCheckpointKey {
            provider: initial_checkpoint.provider,
            source_format: &initial_checkpoint.source_format,
            source_root: &initial_checkpoint.source_root,
            source_path: &initial_checkpoint.source_path,
        })
        .unwrap()
        .unwrap();
    assert_eq!(current_checkpoint.committed_byte_offset, current_len);
}

#[test]
fn waiting_manifest_plan_reobserves_a_new_unit_before_skipping() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let source_path = temp.path().join("sessions");
    fs::create_dir(&source_path).unwrap();
    fs::write(source_path.join("messages.jsonl"), b"not json\n").unwrap();
    let source = explicit_path_source(CaptureProvider::MistralVibe, source_path.clone());
    let files = collect_source_import_files(&source).unwrap();
    let lock_store = Store::open(&db_path).unwrap();
    let persisted = persist_new_source_import_observation(&lock_store, &source, &files).unwrap();
    for file in &files {
        mark_source_import_file_result(
            &lock_store,
            file,
            persisted.inventory_generation,
            CatalogIndexedStatus::Indexed,
            None,
        )
        .unwrap();
    }
    fs::write(
        source_path.join("messages.jsonl"),
        b"first pending change\n",
    )
    .unwrap();
    let files = collect_source_import_files(&source).unwrap();
    let persisted = persist_new_source_import_observation(&lock_store, &source, &files).unwrap();
    let mut import_store = Store::open(&db_path).unwrap();
    let guard = lock_store.begin_event_search_bulk_mode().unwrap();
    let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
    let waiting_source = source.clone();
    let importer = std::thread::spawn(move || {
        import_one_source_inner_with_pre_lock_hook(
            &mut import_store,
            &waiting_source,
            None,
            false,
            false,
            &SourcePreinventory::SourceImportFiles {
                files,
                inventory_generation: persisted.inventory_generation,
            },
            || waiting_tx.send(()).unwrap(),
        )
    });
    waiting_rx.recv().unwrap();

    let added_dir = source_path.join("added");
    fs::create_dir(&added_dir).unwrap();
    fs::write(added_dir.join("messages.jsonl"), b"not json\n").unwrap();
    lock_store.finish_event_search_bulk_mode(&guard).unwrap();
    drop(guard);

    assert!(
        importer.join().unwrap().is_err(),
        "a newly discovered unit must reach its provider adapter after the wait"
    );
}

#[test]
fn waiting_empty_manifest_plan_imports_a_new_unit() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let source_path = temp.path().join("sessions");
    fs::create_dir(&source_path).unwrap();
    let source = explicit_path_source(CaptureProvider::MistralVibe, source_path.clone());
    let files = collect_source_import_files(&source).unwrap();
    assert!(files.is_empty());
    let lock_store = Store::open(&db_path).unwrap();
    let persisted = persist_new_source_import_observation(&lock_store, &source, &files).unwrap();
    let mut import_store = Store::open(&db_path).unwrap();
    let guard = lock_store.begin_event_search_bulk_mode().unwrap();
    let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
    let waiting_source = source.clone();
    let importer = std::thread::spawn(move || {
        import_one_source_inner_with_pre_lock_hook(
            &mut import_store,
            &waiting_source,
            None,
            false,
            false,
            &SourcePreinventory::SourceImportFiles {
                files,
                inventory_generation: persisted.inventory_generation,
            },
            || waiting_tx.send(()).unwrap(),
        )
    });
    waiting_rx.recv().unwrap();

    fs::write(source_path.join("messages.jsonl"), b"not json\n").unwrap();
    lock_store.finish_event_search_bulk_mode(&guard).unwrap();
    drop(guard);

    let error = importer
        .join()
        .unwrap()
        .expect_err("the newly added malformed file must reach the provider adapter");
    assert!(
        !error.to_string().contains("no importable"),
        "the waiter must replace its empty manifest with the current unit"
    );
}

#[test]
fn atomic_root_observation_supersedes_an_abandoned_inventory_generation() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let source_path = temp.path().join("state.db");
    fs::write(&source_path, b"").unwrap();
    let source = explicit_path_source(CaptureProvider::CodeBuddy, source_path);
    let lock_store = Store::open(&db_path).unwrap();
    let (observation, old_generation) = persist_indexed_root(&lock_store, &source);
    let abandoned = lock_store
        .allocate_source_import_inventory_generation(source.provider, source.path.to_str().unwrap())
        .unwrap();
    let persisted = persist_new_source_import_observation(
        &lock_store,
        &source,
        std::slice::from_ref(&observation),
    )
    .unwrap();
    assert_eq!(persisted.inventory_generation, abandoned + 1);
    assert!(persisted.pending_files.is_empty());
    assert!(old_generation < abandoned);
}

#[test]
fn source_outcome_and_generation_commit_before_competing_inventory() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let source_path = temp.path().join("state.json");
    fs::write(&source_path, b"current").unwrap();
    let source = explicit_path_source(CaptureProvider::CodeBuddy, source_path);
    let store = Store::open(&db_path).unwrap();
    let (_, observation) = observe_source_root(&source).unwrap();
    let competing_source = source.clone();
    let competing_observation = observation.clone();
    let competing_store = Store::open(&db_path).unwrap();
    let (start_tx, start_rx) = std::sync::mpsc::channel();
    let (attempt_tx, attempt_rx) = std::sync::mpsc::channel();
    let competing = std::thread::spawn(move || {
        start_rx.recv().unwrap();
        attempt_tx.send(()).unwrap();
        persist_new_source_import_observation(
            &competing_store,
            &competing_source,
            std::slice::from_ref(&competing_observation),
        )
        .unwrap()
    });
    let outcome = SourceImportObservationOutcome {
        file: &observation,
        status: CatalogIndexedStatus::Indexed,
        error: None,
    };
    persist_source_material(&store, &observation);

    let committed = persist_source_import_observation_with_outcomes_and_hook(
        &store,
        &source,
        std::slice::from_ref(&observation),
        &[outcome],
        || {
            start_tx.send(()).unwrap();
            attempt_rx.recv().unwrap();
        },
    )
    .unwrap();
    let competing = competing.join().unwrap();

    assert!(competing.inventory_generation > committed.inventory_generation);
    assert!(store
        .list_pending_source_import_files(source.provider, &observation.source_root)
        .unwrap()
        .is_empty());
}

#[test]
fn stale_manifest_plan_skips_after_newer_completion_wins_bulk_lock() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("work.sqlite");
    let source_path = temp.path().join("sessions");
    fs::create_dir(&source_path).unwrap();
    fs::write(source_path.join("messages.jsonl"), b"not json\n").unwrap();
    let source = explicit_path_source(CaptureProvider::MistralVibe, source_path);
    assert!(source_uses_import_file_manifest(&source));
    let files = collect_source_import_files(&source).unwrap();
    assert_eq!(files.len(), 1);
    let lock_store = Store::open(&db_path).unwrap();
    let old_generation = lock_store
        .allocate_source_import_inventory_generation(source.provider, &files[0].source_root)
        .unwrap();
    persist_source_import_files(&lock_store, &source, old_generation, &files).unwrap();
    let mut import_store = Store::open(&db_path).unwrap();
    let guard = lock_store.begin_event_search_bulk_mode().unwrap();
    let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
    let waiting_source = source.clone();
    let waiting_files = files.clone();
    let importer = std::thread::spawn(move || {
        import_one_source_inner_with_pre_lock_hook(
            &mut import_store,
            &waiting_source,
            None,
            false,
            false,
            &SourcePreinventory::SourceImportFiles {
                files: waiting_files,
                inventory_generation: old_generation,
            },
            || waiting_tx.send(()).unwrap(),
        )
    });
    waiting_rx.recv().unwrap();

    let new_generation = lock_store
        .allocate_source_import_inventory_generation(source.provider, &files[0].source_root)
        .unwrap();
    persist_source_import_files(&lock_store, &source, new_generation, &files).unwrap();
    persist_source_material(&lock_store, &files[0]);
    mark_source_import_file_result(
        &lock_store,
        &files[0],
        new_generation,
        CatalogIndexedStatus::Indexed,
        None,
    )
    .unwrap();
    lock_store.finish_event_search_bulk_mode(&guard).unwrap();
    drop(guard);

    assert_eq!(
        importer.join().unwrap().unwrap(),
        ProviderImportSummary::default()
    );
}
