fn tempdir() -> tempfile::TempDir {
    let temp_root = fs::canonicalize(std::env::temp_dir())
        .expect("system temporary directory should be canonicalizable");
    tempfile::Builder::new()
        .prefix("ctx-native-import-")
        .tempdir_in(temp_root)
        .unwrap()
}

#[test]
fn append_completeness_probe_rejects_oversized_unterminated_line_at_limit() {
    let temp = tempdir();
    let source_path = temp.path().join("oversized.jsonl");
    let file = fs::File::create(&source_path).unwrap();
    file.set_len((MAX_PROVIDER_JSONL_LINE_BYTES as u64).saturating_mul(4))
        .unwrap();

    let error =
        provider_jsonl_range_has_complete_line(&source_path, 0, file.metadata().unwrap().len())
            .unwrap_err();

    assert!(error
        .to_string()
        .contains("provider JSONL line exceeds max bytes"));
}

#[test]
fn staged_append_completion_preserves_accepted_content_status() {
    let mut summary = ProviderImportSummary::default();
    summary.failed = 1;
    summary.mark_retained_existing_content();
    assert_eq!(
        provider_summary_import_status(&summary),
        CatalogIndexedStatus::CompletedWithRejections
    );

    let completion =
        encode_staged_append_completion(summary, None, Some("a".repeat(64)), 123).unwrap();
    let staged = decode_staged_append_completion(completion).unwrap();
    assert_eq!(staged.source_prefix_sha256, Some("a".repeat(64)));
    let (restored, _, _) = staged.into_restored();

    assert_eq!(
        provider_summary_import_status(&restored),
        CatalogIndexedStatus::CompletedWithRejections
    );
}

#[test]
fn codex_preinventory_failures_survive_when_catalog_has_no_pending_sessions() {
    let temp = tempdir();
    let source_path = temp.path().join("sessions");
    fs::create_dir_all(&source_path).unwrap();
    let source = explicit_path_source(CaptureProvider::Codex, source_path);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let source_root = source.path.display().to_string();
    let inventory_generation = store
        .allocate_catalog_inventory_generation(CaptureProvider::Codex, &source_root)
        .unwrap();
    assert!(store
        .complete_catalog_inventory_generation(
            CaptureProvider::Codex,
            &source_root,
            inventory_generation,
        )
        .unwrap());
    let catalog = CatalogSummary {
        failed_sessions: 1,
        failures: vec![ProviderImportFailure {
            line: 0,
            error: "catalog-only rejection".to_owned(),
        }],
        ..CatalogSummary::default()
    };

    let record = import_record_for_source(&source);
    let summary = import_incremental_codex_session_tree(
        &mut store,
        &source,
        &record,
        None,
        Some(&catalog),
        Some(inventory_generation),
        false,
        None,
    )
    .unwrap();

    assert_eq!(summary.failed, 1);
    assert_eq!(summary.failures, catalog.failures);
    assert_eq!(summary.completed_units, 0);
    assert_eq!(summary.deferred_units, 0);
    assert_eq!(summary.post_import_inventory_generation, None);
}

fn write_valid_codex_session(root: &Path, session_id: &str) {
    fs::create_dir_all(root).unwrap();
    fs::write(
        root.join(format!("{session_id}.jsonl")),
        format!(
            "{{\"timestamp\":\"2026-07-13T12:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"{session_id}\",\"timestamp\":\"2026-07-13T12:00:00Z\",\"cwd\":\"/repo\",\"originator\":\"codex-cli\",\"source\":\"cli\"}}}}\n\
             {{\"timestamp\":\"2026-07-13T12:00:01Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"supersession retry oracle\"}}]}}}}\n"
        ),
    )
    .unwrap();
}

#[test]
fn catalog_batch_commits_one_sibling_before_reporting_later_error() {
    let temp = tempdir();
    let source_path = temp.path().join("sessions");
    write_valid_codex_session(&source_path, "a-good");
    write_valid_codex_session(&source_path, "z-retry");
    let source = explicit_path_source(CaptureProvider::Codex, source_path.clone());
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let inventory = inventory_import_sources(&store, vec![source], false).unwrap();
    let plan = ImportPlan::build(&store, inventory.sources).unwrap();
    assert_eq!(plan.fresh_units, 2);
    let slice = plan
        .select_slice(&store, ImportWorkClass::Fresh, 2)
        .unwrap();
    let selected = &slice.sources[0];
    fs::remove_file(source_path.join("z-retry.jsonl")).unwrap();
    let source_plan = &plan.sources[selected.source_index];

    let mut completed = None;
    for _ in 0..32 {
        let result = import_selected_source(
            &mut store,
            &source_plan.source,
            None,
            &selected.preinventory,
            &selected.work,
        )
        .unwrap();
        let remaining_error = result.remaining_error;
        if result.outcome.completed_units == 1 {
            completed = Some((result.outcome, remaining_error));
            break;
        }
        assert_eq!(result.outcome.deferred_units, 1);
        assert!(result.outcome.made_durable_progress());
        assert!(remaining_error.is_none());
    }
    let (outcome, remaining_error) =
        completed.expect("healthy sibling must publish after bounded phases");

    assert_eq!(outcome.completed_units, 1);
    assert!(outcome.summary.imported_events > 0);
    assert!(outcome.made_durable_progress());
    let _error = remaining_error.expect("the missing sibling must remain an error");

    let inventory = inventory_import_sources(
        &store,
        vec![explicit_path_source(CaptureProvider::Codex, source_path)],
        false,
    )
    .unwrap();
    let plan = ImportPlan::build(&store, inventory.sources).unwrap();
    assert_eq!(plan.fresh_units, 0);
    assert_eq!(plan.recovery_units, 0);
}

fn jsonl(value: serde_json::Value) -> String {
    format!("{}\n", serde_json::to_string(&value).unwrap())
}

fn run_single_fresh_unit(
    store: &mut Store,
    source: SourceInfo,
) -> (ProviderImportSummary, ProviderFileCheckpoint) {
    let inventory = inventory_import_sources(store, vec![source], false).unwrap();
    assert!(inventory.failures.is_empty());
    let plan = ImportPlan::build(store, inventory.sources).unwrap();
    assert_eq!(plan.fresh_units, 1);
    assert_eq!(plan.recovery_units, 0);
    let slice = plan
        .select_slice(store, ImportWorkClass::Fresh, plan.fresh_units)
        .unwrap();
    assert_eq!(slice.units, 1);
    let selected = &slice.sources[0];
    let selected_bytes = selected.stats.bytes;
    let source_file_work = matches!(&selected.work, SelectedImportWork::SourceFiles(_));
    let pre_import_inventory_generation = selected.preinventory.inventory_generation();
    let (provider, source_format, source_root, source_path) = match &selected.work {
        SelectedImportWork::Catalog(work) => {
            let unit = &work[0].session;
            (
                unit.provider,
                unit.source_format.clone(),
                unit.source_root.clone(),
                unit.source_path.clone(),
            )
        }
        SelectedImportWork::SourceFiles(work) => {
            let unit = &work[0].file;
            (
                unit.provider,
                unit.source_format.clone(),
                unit.source_root.clone(),
                unit.source_path.clone(),
            )
        }
    };
    let source_plan = &plan.sources[selected.source_index];
    let mut completed = None;
    for _ in 0..32 {
        let result = import_selected_source(
            store,
            &source_plan.source,
            None,
            &selected.preinventory,
            &selected.work,
        )
        .unwrap_or_else(|error| {
            panic!(
                "bounded import for {} failed: {error:#}",
                source_plan.source.path.display()
            )
        });
        assert!(result.remaining_error.is_none());
        if result.completed_units == 1 {
            completed = Some(result.outcome);
            break;
        }
        assert_eq!(result.deferred_units, 1);
    }
    let summary = completed.expect("bounded publication phases must converge");
    let checkpoint = store
        .provider_file_checkpoint(ProviderFileCheckpointKey {
            provider,
            source_format: &source_format,
            source_root: &source_root,
            source_path: &source_path,
        })
        .unwrap()
        .expect("completed append-capable unit must persist a checkpoint");
    assert_eq!(summary.completed_units, 1);
    assert_eq!(summary.completed_bytes, selected_bytes);
    assert_eq!(summary.deferred_units, 0);
    if source_file_work {
        if source_uses_import_file_manifest(&source_plan.source) {
            assert_eq!(
                summary.post_import_inventory_generation, pre_import_inventory_generation,
                "manifested completion must stay on its selected generation"
            );
        } else {
            assert!(
                summary.post_import_inventory_generation > pre_import_inventory_generation,
                "whole-source completion must return its committed post-import generation"
            );
        }
        assert_eq!(
            summary
                .post_import_preinventory
                .as_ref()
                .and_then(SourcePreinventory::inventory_generation),
            summary.post_import_inventory_generation,
            "the scheduler cache must receive the complete committed observation"
        );
    } else {
        assert_eq!(summary.post_import_inventory_generation, None);
        assert!(summary.post_import_preinventory.is_none());
    }
    (summary.summary, checkpoint)
}

fn leave_mutated_pi_publication(store: &mut Store, source: &SourceInfo) -> (SourceImportFile, u64) {
    run_single_fresh_unit(store, source.clone());
    let template = store
        .export_archive()
        .unwrap()
        .events
        .into_iter()
        .next()
        .expect("Pi fixture must import an event");
    let inventory = inventory_import_sources(store, vec![source.clone()], false).unwrap();
    let (file, generation) = match &inventory.sources[0].preinventory {
        SourcePreinventory::SourceImportFiles {
            files,
            inventory_generation,
        } => (files[0].clone(), *inventory_generation),
        other => panic!("unexpected Pi inventory: {other:?}"),
    };
    assert_eq!(
        store
            .schedule_source_import_explicit_rescan(file.provider, &file.source_root, generation,)
            .unwrap(),
        1
    );
    let scope = store
        .begin_provider_file_publication(
            file.provider,
            ProviderFileInventoryObservation::SourceImport {
                source_format: &file.source_format,
                update: SourceImportFileIndexUpdate {
                    source_root: &file.source_root,
                    source_path: &file.source_path,
                    file_size_bytes: file.file_size_bytes,
                    file_modified_at_ms: file.file_modified_at_ms,
                    import_revision: file.import_revision,
                    inventory_generation: generation,
                    metadata: &file.metadata,
                    indexed_at_ms: utc_now().timestamp_millis(),
                },
            },
            provider_canonical_material_source_format(file.provider, &file.source_format).unwrap(),
            ProviderFilePublicationKind::Replacement,
            utc_now().timestamp_millis(),
        )
        .unwrap();
    while !store
        .prepare_provider_file_publication_slice(&scope, 64)
        .unwrap()
        .complete
    {}
    let mut mutation = template;
    mutation.id = new_id();
    mutation.seq = 40_000;
    mutation.dedupe_key = None;
    store
        .with_provider_file_publication_writes(&scope, |store| store.upsert_event(&mutation))
        .unwrap();
    assert!(matches!(
        store.abort_provider_file_publication(scope).unwrap(),
        std::ops::ControlFlow::Break(None)
    ));
    (file, generation)
}

fn write_pi_source(root: &Path, id: &str) -> SourceInfo {
    fs::create_dir_all(root).unwrap();
    fs::write(
        root.join("session.jsonl"),
        format!(
            "{}{}",
            jsonl(json!({"type": "session", "id": id, "timestamp": "2026-07-14T12:00:00Z"})),
            jsonl(json!({
                "type": "message", "id": format!("{id}-user"),
                "timestamp": "2026-07-14T12:00:01Z",
                "message": {"role": "user", "content": id}
            }))
        ),
    )
    .unwrap();
    explicit_path_source(CaptureProvider::Pi, root.to_path_buf())
}

fn finish_all_retirement_work(store: &Store) {
    for _ in 0..64 {
        let work = store
            .list_provider_file_publication_retirement_work(1)
            .unwrap();
        if work.is_empty() {
            return;
        }
        recover_provider_file_publication_retirement(store, &work[0], true).unwrap();
    }
    panic!("bounded publication retirement did not converge");
}

fn finish_selected_source(store: &mut Store, plan: &ImportPlan, selected: &SelectedImportSource) {
    let source = &plan.sources[selected.source_index];
    for _ in 0..32 {
        let result = import_selected_source(
            store,
            &source.source,
            None,
            &selected.preinventory,
            &selected.work,
        )
        .unwrap();
        if result.completed_units == selected.work.unit_count() {
            return;
        }
        assert!(result.deferred_units > 0);
    }
    panic!("bounded selected source did not converge");
}

#[test]
fn publication_owner_omitted_by_filter_resumes_before_requested_source() {
    let temp = tempdir();
    let source_a = write_pi_source(&temp.path().join("source-a"), "owner-a");
    let source_b = write_pi_source(&temp.path().join("source-b"), "requested-b");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    leave_mutated_pi_publication(&mut store, &source_a);

    let inventory = inventory_import_sources(&store, vec![source_b], false).unwrap();
    assert_eq!(inventory.sources.len(), 2);
    assert_eq!(inventory.sources[0].source.path, source_a.path);
    assert!(has_provider_file_publication_work(&store).unwrap());
    let plan = ImportPlan::build(&store, inventory.sources).unwrap();
    let mut state = ImportExecutionState::for_plan(&plan);
    let executable = plan
        .select_slice_for_execution_with_pre_lock_hook(
            &store,
            ImportWorkClass::Recovery,
            1,
            &mut state,
            || {},
        )
        .unwrap()
        .unwrap();
    assert_eq!(executable.slice.sources[0].source_index, 0);
    store
        .finish_event_search_bulk_mode(&executable.bulk_guard)
        .unwrap();
    finish_selected_source(&mut store, &plan, &executable.slice.sources[0]);
    assert!(!store.has_pending_provider_file_publications().unwrap());

    let fresh = plan
        .select_slice(&store, ImportWorkClass::Fresh, 1)
        .unwrap();
    assert_eq!(fresh.sources[0].source_index, 1);
    finish_selected_source(&mut store, &plan, &fresh.sources[0]);
}

#[test]
fn unchanged_owner_omitted_by_filter_is_not_retired() {
    let temp = tempdir();
    let source_a = write_pi_source(&temp.path().join("source-a"), "owner-a");
    let source_b = write_pi_source(&temp.path().join("source-b"), "requested-b");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    leave_mutated_pi_publication(&mut store, &source_a);
    assert_eq!(
        store
            .provider_file_publication_retirement_work_count()
            .unwrap(),
        0
    );

    let inventory = inventory_import_sources(&store, vec![source_b], false).unwrap();
    let plan = ImportPlan::build(&store, inventory.sources).unwrap();
    let mut state = ImportExecutionState::for_plan(&plan);
    let executable = plan
        .select_slice_for_execution_with_pre_lock_hook(
            &store,
            ImportWorkClass::Recovery,
            1,
            &mut state,
            || {},
        )
        .unwrap()
        .unwrap();
    assert!(executable.slice.retirements.is_empty());
    assert_eq!(
        store
            .provider_file_publication_retirement_work_count()
            .unwrap(),
        0
    );
    store
        .finish_event_search_bulk_mode(&executable.bulk_guard)
        .unwrap();
}

#[test]
fn changed_owner_with_advanced_generation_tombstones_then_reinventories() {
    let temp = tempdir();
    let source = write_pi_source(&temp.path().join("source-a"), "owner-a");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let (owner_file, owner_generation) = leave_mutated_pi_publication(&mut store, &source);
    let advanced_generation = store
        .allocate_source_import_inventory_generation(source.provider, &owner_file.source_root)
        .unwrap();
    assert!(advanced_generation > owner_generation);
    fs::OpenOptions::new()
        .append(true)
        .open(&owner_file.source_path)
        .unwrap()
        .write_all(
            jsonl(json!({
                "type": "message", "id": "changed-after-marker",
                "timestamp": "2026-07-14T12:00:02Z",
                "message": {"role": "assistant", "content": "changed"}
            }))
            .as_bytes(),
        )
        .unwrap();

    let inventory = inventory_import_sources(&store, Vec::new(), false).unwrap();
    assert_eq!(
        inventory.sources[0].preinventory.inventory_generation(),
        Some(owner_generation)
    );
    let plan = ImportPlan::build(&store, inventory.sources).unwrap();
    let mut state = ImportExecutionState::for_plan(&plan);
    let executable = plan
        .select_slice_for_execution_with_pre_lock_hook(
            &store,
            ImportWorkClass::Recovery,
            1,
            &mut state,
            || {},
        )
        .unwrap()
        .unwrap();
    assert_eq!(executable.slice.retirements.len(), 1);
    store
        .finish_event_search_bulk_mode(&executable.bulk_guard)
        .unwrap();
    finish_all_retirement_work(&store);
    assert!(!store.has_pending_provider_file_publications().unwrap());

    let pending_after_retirement = store
        .list_pending_source_import_files(source.provider, &owner_file.source_root)
        .unwrap();
    assert_eq!(
        pending_after_retirement.len(),
        1,
        "{:?}",
        store.source_import_file_counts().unwrap()
    );
    let fresh = plan
        .select_slice_for_execution_with_pre_lock_hook(
            &store,
            ImportWorkClass::Fresh,
            1,
            &mut state,
            || {},
        )
        .unwrap();
    let next = match fresh {
        Some(next) => next,
        None => plan
            .select_slice_for_execution_with_pre_lock_hook(
                &store,
                ImportWorkClass::Recovery,
                1,
                &mut state,
                || {},
            )
            .unwrap()
            .unwrap(),
    };
    assert_eq!(next.slice.sources.len(), 1);
    assert_eq!(next.slice.sources[0].work.unit_count(), 1);
    store
        .finish_event_search_bulk_mode(&next.bulk_guard)
        .unwrap();

    let later = inventory_import_sources(&store, vec![source], false).unwrap();
    let later_plan = ImportPlan::build(&store, later.sources).unwrap();
    assert_eq!(
        later_plan
            .fresh_units
            .saturating_add(later_plan.recovery_units),
        1
    );
}

#[test]
fn missing_owner_root_tombstones_and_retires() {
    let temp = tempdir();
    let source = write_pi_source(&temp.path().join("source-a"), "owner-a");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    leave_mutated_pi_publication(&mut store, &source);
    fs::remove_dir_all(&source.path).unwrap();

    let inventory = inventory_import_sources(&store, Vec::new(), false).unwrap();
    assert_eq!(inventory.sources.len(), 1);
    assert!(matches!(
        inventory.sources[0].preinventory,
        SourcePreinventory::SourceImportFiles { .. }
    ));
    assert_eq!(
        inventory.sources[0].source.source_format,
        "pi_session_jsonl"
    );
    let plan = ImportPlan::build(&store, inventory.sources).unwrap();
    let mut state = ImportExecutionState::for_plan(&plan);
    let executable = plan
        .select_slice_for_execution_with_pre_lock_hook(
            &store,
            ImportWorkClass::Recovery,
            1,
            &mut state,
            || {},
        )
        .unwrap()
        .unwrap();
    assert_eq!(executable.slice.retirements.len(), 1);
    store
        .finish_event_search_bulk_mode(&executable.bulk_guard)
        .unwrap();
    finish_all_retirement_work(&store);
    assert!(!store.has_pending_provider_file_publications().unwrap());
}

fn assert_unchanged_source_has_no_work(store: &Store, source: SourceInfo) {
    let inventory = inventory_import_sources(store, vec![source], false).unwrap();
    assert!(inventory.failures.is_empty());
    let plan = ImportPlan::build(store, inventory.sources).unwrap();
    assert_eq!(plan.fresh_units, 0);
    assert_eq!(plan.recovery_units, 0);
}

#[test]
fn resume_does_not_reschedule_healthy_codex_and_pi_units() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let codex_root = temp.path().join("codex-sessions");
    write_valid_codex_session(&codex_root, "resume-codex");
    let codex = explicit_path_source(CaptureProvider::Codex, codex_root);
    run_single_fresh_unit(&mut store, codex.clone());

    let pi_root = temp.path().join("pi-sessions");
    fs::create_dir_all(&pi_root).unwrap();
    fs::write(
        pi_root.join("resume-pi.jsonl"),
        format!(
            "{}{}",
            jsonl(json!({
                "type": "session", "id": "resume-pi", "timestamp": "2026-07-14T12:00:00Z"
            })),
            jsonl(json!({
                "type": "message", "id": "resume-pi-user", "timestamp": "2026-07-14T12:00:01Z",
                "message": {"role": "user", "content": "resume pi"}
            }))
        ),
    )
    .unwrap();
    let pi = explicit_path_source(CaptureProvider::Pi, pi_root);
    run_single_fresh_unit(&mut store, pi.clone());

    let inventory = inventory_import_sources(&store, vec![codex, pi], true).unwrap();
    let plan = ImportPlan::build(&store, inventory.sources).unwrap();
    assert_eq!(plan.fresh_units, 0);
    assert_eq!(plan.recovery_units, 0);
}

#[test]
fn append_allowlist_native_paths_publish_checkpoints_and_noop_when_unchanged() {
    let cases = [
        (CaptureProvider::Codex, "codex-file"),
        (CaptureProvider::Codex, "codex-tree"),
        (CaptureProvider::Pi, "pi"),
        (CaptureProvider::Claude, "claude"),
        (CaptureProvider::Tabnine, "tabnine"),
    ];
    for (index, (provider, label)) in cases.into_iter().enumerate() {
        let temp = tempdir();
        let root = temp.path().join(label);
        let file = match (provider, label) {
            (CaptureProvider::Codex, "codex-file") => root.join("session.jsonl"),
            (CaptureProvider::Codex, _) => root.join("session.jsonl"),
            (CaptureProvider::Pi, _) => root.join("session.jsonl"),
            (CaptureProvider::Claude, _) => root.join("project/session.jsonl"),
            (CaptureProvider::Tabnine, _) => root.join("chats/session.jsonl"),
            _ => unreachable!(),
        };
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let contents = match provider {
            CaptureProvider::Codex => format!(
                "{}{}",
                jsonl(json!({
                    "timestamp": "2026-07-14T12:00:00Z",
                    "type": "session_meta",
                    "payload": {"id": format!("codex-{index}"), "timestamp": "2026-07-14T12:00:00Z", "cwd": "/repo"}
                })),
                jsonl(json!({
                    "timestamp": "2026-07-14T12:00:01Z",
                    "type": "response_item",
                    "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "codex checkpoint"}]}
                }))
            ),
            CaptureProvider::Pi => format!(
                "{}{}",
                jsonl(
                    json!({"type": "session", "id": "pi-checkpoint", "timestamp": "2026-07-14T12:00:00Z"})
                ),
                jsonl(json!({
                    "type": "message", "id": "pi-user", "timestamp": "2026-07-14T12:00:01Z",
                    "message": {"role": "user", "content": "pi checkpoint"}
                }))
            ),
            CaptureProvider::Claude => jsonl(json!({
                "sessionId": "claude-checkpoint", "timestamp": "2026-07-14T12:00:00Z",
                "type": "user", "uuid": "claude-user",
                "message": {"role": "user", "content": "claude checkpoint"}
            })),
            CaptureProvider::Tabnine => format!(
                "{}{}",
                jsonl(
                    json!({"sessionId": "tabnine-checkpoint", "startTime": "2026-07-14T12:00:00Z"})
                ),
                jsonl(json!({
                    "id": "tabnine-user", "timestamp": "2026-07-14T12:00:01Z",
                    "type": "user", "content": "tabnine checkpoint"
                }))
            ),
            _ => unreachable!(),
        };
        fs::write(&file, contents).unwrap();
        let source_path = if label == "codex-file" {
            file.clone()
        } else {
            root
        };
        let source = explicit_path_source(provider, source_path);
        let db_path = temp.path().join("work.sqlite");
        let mut store = Store::open(&db_path).unwrap();

        let (summary, checkpoint) = run_single_fresh_unit(&mut store, source.clone());
        assert!(summary.imported_events > 0, "{label}");
        assert_eq!(
            checkpoint.committed_byte_offset,
            fs::metadata(&file).unwrap().len()
        );
        drop(store);
        let store = Store::open(&db_path).unwrap();
        assert_unchanged_source_has_no_work(&store, source);
    }
}
