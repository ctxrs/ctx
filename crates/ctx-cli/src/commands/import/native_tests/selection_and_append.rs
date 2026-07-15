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

    let summary = import_incremental_codex_session_tree(
        &mut store,
        &source,
        new_id(),
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
fn catalog_batch_returns_committed_sibling_before_remaining_error() {
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

    let result = import_selected_source(
        &mut store,
        &source_plan.source,
        None,
        &selected.preinventory,
        &selected.work,
    )
    .unwrap();

    assert_eq!(result.outcome.completed_units, 1);
    assert!(result.outcome.summary.imported_events > 0);
    assert!(result.outcome.made_durable_progress());
    let _error = result
        .remaining_error
        .expect("the missing sibling must remain an error");
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
    let summary = import_selected_source(
        store,
        &source_plan.source,
        None,
        &selected.preinventory,
        &selected.work,
    )
    .unwrap();
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
    (summary.outcome.summary, checkpoint)
}

fn assert_unchanged_source_has_no_work(store: &Store, source: SourceInfo) {
    let inventory = inventory_import_sources(store, vec![source], false).unwrap();
    assert!(inventory.failures.is_empty());
    let plan = ImportPlan::build(store, inventory.sources).unwrap();
    assert_eq!(plan.fresh_units, 0);
    assert_eq!(plan.recovery_units, 0);
}

#[test]
fn resume_keeps_healthy_unchanged_codex_and_pi_units_terminal() {
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
