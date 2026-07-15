#[test]
fn codex_partial_append_defers_then_shrink_replacement_removes_stale_material() {
    let temp = tempdir();
    let root = temp.path().join("sessions");
    let file = root.join("session.jsonl");
    fs::create_dir_all(&root).unwrap();
    let header = jsonl(json!({
        "timestamp": "2026-07-14T12:00:00Z",
        "type": "session_meta",
        "payload": {"id": "codex-partial", "timestamp": "2026-07-14T12:00:00Z", "cwd": "/repo"}
    }));
    let initial_message = jsonl(json!({
        "timestamp": "2026-07-14T12:00:01Z",
        "type": "response_item",
        "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "keep me"}]}
    }));
    fs::write(&file, format!("{header}{initial_message}")).unwrap();
    let source = explicit_path_source(CaptureProvider::Codex, root);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let (_, initial_checkpoint) = run_single_fresh_unit(&mut store, source.clone());
    assert!(!store.has_pending_provider_file_publications().unwrap());

    let completed_tail = serde_json::to_string(&json!({
        "timestamp": "2026-07-14T12:00:02Z",
        "type": "response_item",
        "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "remove me after shrink"}]}
    }))
    .unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(&file)
        .unwrap()
        .write_all(completed_tail.as_bytes())
        .unwrap();
    let inventory = inventory_import_sources(&store, vec![source.clone()], false).unwrap();
    let plan = ImportPlan::build(&store, inventory.sources).unwrap();
    let slice = plan
        .select_slice(&store, ImportWorkClass::Fresh, plan.fresh_units)
        .unwrap();
    let selected = &slice.sources[0];
    let source_plan = &plan.sources[selected.source_index];
    let deferred = import_selected_source(
        &mut store,
        &source_plan.source,
        None,
        &selected.preinventory,
        &selected.work,
    )
    .unwrap();
    assert_eq!(deferred.summary, ProviderImportSummary::default());
    assert_eq!(deferred.completed_units, 0);
    assert_eq!(deferred.completed_bytes, 0);
    assert_eq!(deferred.deferred_units, 1);
    assert_eq!(deferred.post_import_inventory_generation, None);
    let retained = store
        .provider_file_checkpoint(ProviderFileCheckpointKey {
            provider: initial_checkpoint.provider,
            source_format: &initial_checkpoint.source_format,
            source_root: &initial_checkpoint.source_root,
            source_path: &initial_checkpoint.source_path,
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        retained.committed_byte_offset,
        initial_checkpoint.committed_byte_offset
    );
    assert!(!store.has_pending_provider_file_publications().unwrap());

    fs::OpenOptions::new()
        .append(true)
        .open(&file)
        .unwrap()
        .write_all(b"\n")
        .unwrap();
    let (_, completed_checkpoint) = run_single_fresh_unit(&mut store, source.clone());
    assert_eq!(
        completed_checkpoint.committed_byte_offset,
        fs::metadata(&file).unwrap().len()
    );
    assert!(serde_json::to_string(&store.export_archive().unwrap())
        .unwrap()
        .contains("remove me after shrink"));
    assert!(!store.has_pending_provider_file_publications().unwrap());

    fs::write(&file, format!("{header}{initial_message}")).unwrap();
    let (_, replacement_checkpoint) = run_single_fresh_unit(&mut store, source.clone());
    assert_eq!(
        replacement_checkpoint.committed_byte_offset,
        fs::metadata(&file).unwrap().len()
    );
    let archive = serde_json::to_string(&store.export_archive().unwrap()).unwrap();
    assert!(archive.contains("keep me"));
    assert!(!archive.contains("remove me after shrink"));
    assert!(!store.has_pending_provider_file_publications().unwrap());
    assert_unchanged_source_has_no_work(&store, source);
}

#[test]
fn mixed_codex_append_slice_reports_only_completed_bytes() {
    let temp = tempdir();
    let root = temp.path().join("sessions");
    write_valid_codex_session(&root, "a-complete");
    write_valid_codex_session(&root, "b-deferred");
    let completed_file = root.join("a-complete.jsonl");
    let deferred_file = root.join("b-deferred.jsonl");
    let source = explicit_path_source(CaptureProvider::Codex, root);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let inventory = inventory_import_sources(&store, vec![source.clone()], false).unwrap();
    let plan = ImportPlan::build(&store, inventory.sources).unwrap();
    let slice = plan
        .select_slice(&store, ImportWorkClass::Fresh, plan.fresh_units)
        .unwrap();
    let selected = &slice.sources[0];
    let source_plan = &plan.sources[selected.source_index];
    let initial = import_selected_source(
        &mut store,
        &source_plan.source,
        None,
        &selected.preinventory,
        &selected.work,
    )
    .unwrap();
    assert_eq!(initial.completed_units, 2);
    assert_eq!(initial.deferred_units, 0);

    fs::OpenOptions::new()
        .append(true)
        .open(&completed_file)
        .unwrap()
        .write_all(
            jsonl(json!({
                "timestamp": "2026-07-14T12:00:02Z",
                "type": "response_item",
                "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "complete tail"}]}
            }))
            .as_bytes(),
        )
        .unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(&deferred_file)
        .unwrap()
        .write_all(
            serde_json::to_string(&json!({
                "timestamp": "2026-07-14T12:00:02Z",
                "type": "response_item",
                "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "partial tail"}]}
            }))
            .unwrap()
            .as_bytes(),
        )
        .unwrap();

    let inventory = inventory_import_sources(&store, vec![source], false).unwrap();
    let plan = ImportPlan::build(&store, inventory.sources).unwrap();
    assert_eq!(plan.fresh_units, 2);
    let slice = plan
        .select_slice(&store, ImportWorkClass::Fresh, plan.fresh_units)
        .unwrap();
    let selected = &slice.sources[0];
    let expected_completed_bytes = match &selected.work {
        SelectedImportWork::Catalog(work) => {
            work.iter()
                .find(|work| Path::new(&work.session.source_path) == completed_file)
                .unwrap()
                .estimated_bytes
        }
        SelectedImportWork::SourceFiles(_) => panic!("Codex tree must select catalog work"),
    };
    let source_plan = &plan.sources[selected.source_index];
    let outcome = import_selected_source(
        &mut store,
        &source_plan.source,
        None,
        &selected.preinventory,
        &selected.work,
    )
    .unwrap();

    assert_eq!(outcome.completed_units, 1);
    assert_eq!(outcome.deferred_units, 1);
    assert_eq!(outcome.completed_bytes, expected_completed_bytes);
}

#[test]
fn rejected_codex_growth_keeps_later_growth_append_capable() {
    let temp = tempdir();
    let root = temp.path().join("sessions");
    let file = root.join("session.jsonl");
    fs::create_dir_all(&root).unwrap();
    let header = jsonl(json!({
        "timestamp": "2026-07-14T12:00:00Z",
        "type": "session_meta",
        "payload": {"id": "codex-rejected-growth", "timestamp": "2026-07-14T12:00:00Z", "cwd": "/repo"}
    }));
    let initial_message = jsonl(json!({
        "timestamp": "2026-07-14T12:00:01Z",
        "type": "response_item",
        "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "initial"}]}
    }));
    let initial = format!("{header}{initial_message}");
    fs::write(&file, &initial).unwrap();
    let source = explicit_path_source(CaptureProvider::Codex, root);
    let db_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&db_path).unwrap();
    let (_, initial_checkpoint) = run_single_fresh_unit(&mut store, source.clone());

    let rejected = concat!(
        r#"{"timestamp":"2026-07-14T12:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":["#,
        "\n"
    );
    fs::write(&file, format!("{initial}{rejected}")).unwrap();
    let inventory = inventory_import_sources(&store, vec![source.clone()], false).unwrap();
    let plan = ImportPlan::build(&store, inventory.sources).unwrap();
    let slice = plan
        .select_slice(&store, ImportWorkClass::Fresh, plan.fresh_units)
        .unwrap();
    let selected = &slice.sources[0];
    let first_reason = match &selected.work {
        SelectedImportWork::Catalog(work) => work[0].reason,
        SelectedImportWork::SourceFiles(_) => panic!("Codex tree must select catalog work"),
    };
    assert_eq!(first_reason, ImportPendingReason::FreshAppend);
    let source_plan = &plan.sources[selected.source_index];
    let rejected_outcome = import_selected_source(
        &mut store,
        &source_plan.source,
        None,
        &selected.preinventory,
        &selected.work,
    )
    .unwrap();
    assert_eq!(rejected_outcome.summary.failed, 1);
    assert_eq!(
        store
            .provider_file_checkpoint(ProviderFileCheckpointKey {
                provider: initial_checkpoint.provider,
                source_format: &initial_checkpoint.source_format,
                source_root: &initial_checkpoint.source_root,
                source_path: &initial_checkpoint.source_path,
            })
            .unwrap()
            .unwrap()
            .committed_byte_offset,
        initial_checkpoint.committed_byte_offset
    );
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let post_rejection_state: (String, i64, i64, i64) = conn
        .query_row(
            r#"
            SELECT catalog.indexed_status,
                   catalog.file_size_bytes,
                   catalog.indexed_file_size_bytes,
                   EXISTS (
                       SELECT 1
                       FROM sessions AS material_session
                       JOIN capture_sources AS source
                         ON source.id = material_session.capture_source_id
                       WHERE material_session.provider = catalog.provider
                         AND material_session.external_session_id = catalog.external_session_id
                         AND source.provider = catalog.provider
                         AND source.source_format = 'codex_session_jsonl'
                         AND source.raw_source_path = catalog.source_path
                   )
            FROM catalog_sessions AS catalog
            WHERE catalog.source_path = ?1
            "#,
            [file.to_str().unwrap()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(post_rejection_state.0, "completed_with_rejections");
    assert_eq!(post_rejection_state.1, post_rejection_state.2);
    assert_eq!(post_rejection_state.3, 1);
    drop(conn);

    let accepted = jsonl(json!({
        "timestamp": "2026-07-14T12:00:03Z",
        "type": "response_item",
        "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "accepted after rejection"}]}
    }));
    fs::write(&file, format!("{initial}{rejected}{accepted}")).unwrap();
    let inventory = inventory_import_sources(&store, vec![source], false).unwrap();
    let plan = ImportPlan::build(&store, inventory.sources).unwrap();
    let slice = plan
        .select_slice(&store, ImportWorkClass::Fresh, plan.fresh_units)
        .unwrap();
    let selected = &slice.sources[0];
    let second_reason = match &selected.work {
        SelectedImportWork::Catalog(work) => work[0].reason,
        SelectedImportWork::SourceFiles(_) => panic!("Codex tree must select catalog work"),
    };
    assert_eq!(second_reason, ImportPendingReason::FreshAppend);
    let source_plan = &plan.sources[selected.source_index];
    let accepted_outcome = import_selected_source(
        &mut store,
        &source_plan.source,
        None,
        &selected.preinventory,
        &selected.work,
    )
    .unwrap();
    assert_eq!(accepted_outcome.summary.failed, 1);
    assert_eq!(accepted_outcome.summary.imported_events, 1);
    assert_eq!(
        store
            .provider_file_checkpoint(ProviderFileCheckpointKey {
                provider: initial_checkpoint.provider,
                source_format: &initial_checkpoint.source_format,
                source_root: &initial_checkpoint.source_root,
                source_path: &initial_checkpoint.source_path,
            })
            .unwrap()
            .unwrap()
            .committed_byte_offset,
        initial_checkpoint.committed_byte_offset
    );
}

#[test]
fn deferred_source_file_reports_zero_completion_and_post_import_generation() {
    let temp = tempdir();
    let file = temp.path().join("session.jsonl");
    fs::write(
        &file,
        format!(
            "{}{}",
            jsonl(json!({
                "timestamp": "2026-07-14T12:00:00Z",
                "type": "session_meta",
                "payload": {"id": "codex-source-file-deferred", "timestamp": "2026-07-14T12:00:00Z", "cwd": "/repo"}
            })),
            jsonl(json!({
                "timestamp": "2026-07-14T12:00:01Z",
                "type": "response_item",
                "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "initial"}]}
            }))
        ),
    )
    .unwrap();
    let source = explicit_path_source(CaptureProvider::Codex, file.clone());
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    run_single_fresh_unit(&mut store, source.clone());

    let unterminated_tail = serde_json::to_string(&json!({
        "timestamp": "2026-07-14T12:00:02Z",
        "type": "response_item",
        "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "wait for newline"}]}
    }))
    .unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(&file)
        .unwrap()
        .write_all(unterminated_tail.as_bytes())
        .unwrap();

    let inventory = inventory_import_sources(&store, vec![source], false).unwrap();
    let plan = ImportPlan::build(&store, inventory.sources).unwrap();
    let slice = plan
        .select_slice(&store, ImportWorkClass::Fresh, plan.fresh_units)
        .unwrap();
    let selected = &slice.sources[0];
    assert!(matches!(&selected.work, SelectedImportWork::SourceFiles(_)));
    let pre_import_generation = selected.preinventory.inventory_generation().unwrap();
    let source_plan = &plan.sources[selected.source_index];
    let outcome = import_selected_source(
        &mut store,
        &source_plan.source,
        None,
        &selected.preinventory,
        &selected.work,
    )
    .unwrap();

    assert_eq!(outcome.completed_units, 0);
    assert_eq!(outcome.completed_bytes, 0);
    assert_eq!(outcome.deferred_units, 1);
    assert!(outcome.post_import_inventory_generation > Some(pre_import_generation));
}

#[test]
fn append_source_failure_after_mutation_preserves_error_and_allows_other_work() {
    let temp = tempdir();
    let root = temp.path().join("sessions");
    let file = root.join("session.jsonl");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        &file,
        format!(
            "{}{}",
            jsonl(json!({
                "timestamp": "2026-07-14T12:00:00Z",
                "type": "session_meta",
                "payload": {"id": "codex-recovery-required", "timestamp": "2026-07-14T12:00:00Z", "cwd": "/repo"}
            })),
            jsonl(json!({
                "timestamp": "2026-07-14T12:00:01Z",
                "type": "response_item",
                "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "initial"}]}
            }))
        ),
    )
    .unwrap();
    let source = explicit_path_source(CaptureProvider::Codex, root.clone());
    let db_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&db_path).unwrap();
    run_single_fresh_unit(&mut store, source.clone());

    fs::OpenOptions::new()
        .append(true)
        .open(&file)
        .unwrap()
        .write_all(
            jsonl(json!({
                "timestamp": "2026-07-14T12:00:02Z",
                "type": "response_item",
                "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "durable before injected failure"}]}
            }))
            .as_bytes(),
        )
        .unwrap();
    let inventory = inventory_import_sources(&store, vec![source.clone()], false).unwrap();
    let plan = ImportPlan::build(&store, inventory.sources).unwrap();
    let slice = plan
        .select_slice(&store, ImportWorkClass::Fresh, plan.fresh_units)
        .unwrap();
    let selected = &slice.sources[0];
    let source_plan = &plan.sources[selected.source_index];
    let catalog_outcome_before = rusqlite::Connection::open(&db_path)
        .unwrap()
        .query_row(
            "SELECT indexed_status, indexed_error FROM catalog_sessions WHERE source_path = ?1",
            [file.display().to_string()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .unwrap();
    inject_append_source_failure_after_mutation();
    let error = import_selected_source(
        &mut store,
        &source_plan.source,
        None,
        &selected.preinventory,
        &selected.work,
    )
    .unwrap_err();

    assert!(publication_recovery_required(&error));
    assert!(publication_recovery_maintenance_warning(&error).is_none());
    assert_eq!(import_error_scope(&error), ImportFailureScope::Source);
    assert_eq!(
        error.to_string(),
        "invalid capture payload: injected append source failure after publication mutation"
    );
    assert!(store.has_pending_provider_file_publications().unwrap());
    assert_eq!(
        rusqlite::Connection::open(&db_path)
            .unwrap()
            .query_row(
                "SELECT indexed_status, indexed_error FROM catalog_sessions WHERE source_path = ?1",
                [file.display().to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .unwrap(),
        catalog_outcome_before,
        "recovery-required errors must not record an ordinary source outcome"
    );

    let pi_root = temp.path().join("pi-sessions");
    fs::create_dir_all(&pi_root).unwrap();
    fs::write(
        pi_root.join("session.jsonl"),
        format!(
            "{}{}",
            jsonl(json!({
                "type": "session", "id": "pi-unrelated", "timestamp": "2026-07-14T12:00:00Z"
            })),
            jsonl(json!({
                "type": "message", "id": "pi-unrelated-user", "timestamp": "2026-07-14T12:00:01Z",
                "message": {"role": "user", "content": "unrelated work continues"}
            }))
        ),
    )
    .unwrap();
    let unrelated = explicit_path_source(CaptureProvider::Pi, pi_root);
    let (unrelated_summary, _) = run_single_fresh_unit(&mut store, unrelated);
    assert!(unrelated_summary.imported_events > 0);

    fs::remove_file(&file).unwrap();
    let source_root = root.to_str().unwrap();
    let tombstone_generation = store
        .allocate_catalog_inventory_generation(CaptureProvider::Codex, source_root)
        .unwrap();
    store
        .mark_catalog_source_missing_paths_stale(
            CaptureProvider::Codex,
            source_root,
            &[],
            utc_now().timestamp_millis(),
            tombstone_generation,
        )
        .unwrap();
    let work = store
        .list_provider_file_publication_retirement_work(1)
        .unwrap();
    assert_eq!(work.len(), 1);
    let recovery = recover_provider_file_publication_retirement(&store, &work[0], true).unwrap();
    assert!(recovery.completed);
    assert!(recovery.made_durable_progress);
    assert!(recovery.maintenance_warnings.is_empty());
    assert!(!store.has_pending_provider_file_publications().unwrap());
}

#[test]
fn provider_finalize_cleanup_warning_maps_to_import_summary_maintenance() {
    let mut summary = ProviderImportSummary::default();
    push_publication_maintenance_warning(
        &mut summary,
        ProviderFileMaintenanceWarning::StagingCleanupDeferred {
            publication_id: "publication-1".to_owned(),
            operation: "remove-directory",
        },
    );

    assert_eq!(summary.maintenance_warnings.len(), 1);
    assert_eq!(
        summary.maintenance_warnings[0].kind,
        ProviderImportMaintenanceKind::ImportInterruptedAfterCommit
    );
    assert!(summary.maintenance_warnings[0]
        .error
        .contains("staging cleanup deferred during remove-directory"));
}

#[test]
fn codex_full_rescan_retries_one_superseded_result_generation() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let source_path = temp.path().join("sessions");
    write_valid_codex_session(&source_path, "resume-superseded-once");
    let source = explicit_path_source(CaptureProvider::Codex, source_path.clone());
    let source_root = source_path.display().to_string();
    let superseded = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let callback_superseded = Arc::clone(&superseded);
    let callback_db = db_path.clone();
    let progress: CodexSessionImportProgressCallback = Arc::new(move |progress| {
        if progress.done && !callback_superseded.swap(true, std::sync::atomic::Ordering::SeqCst) {
            let competing = Store::open(&callback_db).unwrap();
            competing
                .allocate_catalog_inventory_generation(CaptureProvider::Codex, &source_root)
                .unwrap();
        }
    });
    let mut store = Store::open(&db_path).unwrap();

    let summary = import_one_source_inner(
        &mut store,
        &source,
        Some(progress),
        false,
        true,
        &SourcePreinventory::None,
    )
    .unwrap();

    assert!(superseded.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(
        summary
            .imported_sessions
            .saturating_add(summary.skipped_sessions),
        1
    );
}

#[test]
fn codex_supersession_retry_exhaustion_stays_system_scoped() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let source_path = temp.path().join("sessions");
    write_valid_codex_session(&source_path, "resume-superseded-always");
    let source = explicit_path_source(CaptureProvider::Codex, source_path.clone());
    let source_root = source_path.display().to_string();
    let callback_db = db_path.clone();
    let superseded = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let callback_superseded = Arc::clone(&superseded);
    let progress: CodexSessionImportProgressCallback = Arc::new(move |progress| {
        if progress.done {
            let competing = Store::open(&callback_db).unwrap();
            competing
                .allocate_catalog_inventory_generation(CaptureProvider::Codex, &source_root)
                .unwrap();
            callback_superseded.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    });
    let mut store = Store::open(&db_path).unwrap();

    let error = import_one_source_inner(
        &mut store,
        &source,
        Some(progress),
        false,
        true,
        &SourcePreinventory::None,
    )
    .expect_err("three superseded result generations must exhaust the retry budget");

    assert!(superseded.load(std::sync::atomic::Ordering::SeqCst) >= 3);
    assert!(is_inventory_superseded(&error));
    assert_eq!(import_error_scope(&error), ImportFailureScope::System);
}
