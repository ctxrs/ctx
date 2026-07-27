use super::*;

fn write_cli_rovodev_session(root: &Path, id: &str, messages: &[Value]) -> PathBuf {
    let session = root.join(id);
    fs::create_dir_all(&session).unwrap();
    fs::write(
        session.join("metadata.json"),
        serde_json::to_vec(&json!({
            "session_id": id,
            "created_at": "2026-07-25T11:00:00Z",
            "workspace_path": "/workspace/rovodev"
        }))
        .unwrap(),
    )
    .unwrap();
    write_cli_rovodev_context(&session, id, messages);
    session
}

fn write_cli_rovodev_context(session: &Path, id: &str, messages: &[Value]) {
    fs::write(
        session.join("session_context.json"),
        serde_json::to_vec(&json!({
            "session_id": id,
            "message_history": messages
        }))
        .unwrap(),
    )
    .unwrap();
}

fn cli_rovodev_message(role: &str, content: &str) -> Value {
    json!({"role": role, "content": content})
}

fn cli_rovodev_output(id: &str, content: &str) -> Value {
    json!({
        "role": "user",
        "content": [{
            "type": "tool_result",
            "tool_use_id": id,
            "content": content,
            "status": "success"
        }]
    })
}

fn import_cli_root_with_profile(
    store: &mut Store,
    source: &SourceInfo,
    profile: &ImportProfile,
) -> ProviderImportSummary {
    let inventory =
        inventory_import_sources(store, vec![source.clone()], false, false, false).unwrap();
    assert!(inventory.failures.is_empty());
    assert_eq!(inventory.sources.len(), 1);
    let plan = inventory.sources.into_iter().next().unwrap();
    import_one_source_for_search_refresh_with_profile(
        store,
        &plan.source,
        None,
        &plan.preinventory,
        profile,
    )
    .unwrap()
}

fn first_cli_provider_event(
    store: &Store,
    provider: CaptureProvider,
    external_session_id: &str,
) -> uuid::Uuid {
    let session = store
        .session_by_external_session(provider, external_session_id)
        .unwrap()
        .unwrap();
    store.events_for_session(session.id).unwrap()[0].id
}

#[test]
fn provider_owned_root_scheduler_preserves_siblings_routes_and_pro_state() {
    let temp = tempdir();
    let root = temp.path().join("rovodev-sessions");
    let alpha = write_cli_rovodev_session(
        &root,
        "alpha",
        &[
            cli_rovodev_message("user", "alpha core"),
            cli_rovodev_output("alpha-call-1", "alpha output one"),
        ],
    );
    write_cli_rovodev_session(
        &root,
        "beta",
        &[
            cli_rovodev_message("user", "beta core"),
            cli_rovodev_output("beta-call-1", "beta output one"),
        ],
    );
    let source = explicit_path_source(CaptureProvider::RovoDev, root.clone());
    let db_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&db_path).unwrap();
    let sink = Arc::new(RecordingProOutputSink::new(false));
    let profile = ImportProfile::CoreAndPro(sink.clone());

    let cold = import_cli_root_with_profile(&mut store, &source, &profile);
    assert_eq!(cold.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(cold.imported_sessions, 2);
    let alpha_event = first_cli_provider_event(&store, CaptureProvider::RovoDev, "alpha");
    let beta_event = first_cli_provider_event(&store, CaptureProvider::RovoDev, "beta");
    assert!(store.authorized_source_route_for_event(alpha_event).is_ok());
    assert!(store.authorized_source_route_for_event(beta_event).is_ok());
    assert_single_root_schedule_row(&db_path, &source);
    let cold_progress = sink.progress();
    assert_eq!(cold_progress.len(), 2);
    assert!(cold_progress.values().all(|progress| progress.terminal));
    assert_eq!(sink.output_records.load(Ordering::SeqCst), 2);

    let pages_after_cold = sink.pages.load(Ordering::SeqCst);
    let noop = import_cli_root_with_profile(&mut store, &source, &profile);
    assert_eq!(noop.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(sink.pages.load(Ordering::SeqCst), pages_after_cold);
    assert_eq!(sink.progress(), cold_progress);
    assert_eq!(sink.output_records.load(Ordering::SeqCst), 2);
    assert_single_root_schedule_row(&db_path, &source);

    write_cli_rovodev_context(
        &alpha,
        "alpha",
        &[
            cli_rovodev_message("user", "alpha core"),
            cli_rovodev_output("alpha-call-1", "alpha output one"),
            cli_rovodev_message("assistant", "alpha append"),
            cli_rovodev_output("alpha-call-2", "alpha output two"),
        ],
    );
    let appended = import_cli_root_with_profile(&mut store, &source, &profile);
    assert_eq!(appended.work_result(), ProviderImportWorkResult::Changed);
    assert!(store.authorized_source_route_for_event(alpha_event).is_ok());
    assert!(store.authorized_source_route_for_event(beta_event).is_ok());
    let appended_progress = sink.progress();
    assert_eq!(appended_progress.len(), 2);
    assert_eq!(
        cold_progress
            .iter()
            .filter(|(identity, progress)| appended_progress.get(*identity) == Some(*progress))
            .count(),
        1,
        "only the appended sibling may advance private output state"
    );
    assert_eq!(sink.output_records.load(Ordering::SeqCst), 3);
    assert_single_root_schedule_row(&db_path, &source);

    fs::remove_dir_all(alpha).unwrap();
    let deleted_one = import_cli_root_with_profile(&mut store, &source, &profile);
    assert_eq!(deleted_one.work_result(), ProviderImportWorkResult::Changed);
    assert!(store
        .authorized_source_route_for_event(alpha_event)
        .is_err());
    assert!(store.authorized_source_route_for_event(beta_event).is_ok());
    assert_eq!(sink.progress(), appended_progress);
    assert_eq!(sink.output_records.load(Ordering::SeqCst), 3);
    assert_single_root_schedule_row(&db_path, &source);

    fs::remove_dir_all(&root).unwrap();
    let missing_source = explicit_path_source(CaptureProvider::RovoDev, root);
    assert!(!missing_source.exists);
    let inventory =
        crate::commands::import::inventory_available_sources(&store, &[missing_source]).unwrap();
    assert_eq!(inventory.sources.len(), 1);
    let plan = inventory.sources.into_iter().next().unwrap();
    assert!(matches!(plan.preinventory, SourcePreinventory::None));
    let deleted_root = import_one_source_for_search_refresh_with_profile(
        &mut store,
        &plan.source,
        None,
        &plan.preinventory,
        &profile,
    )
    .unwrap();
    assert_eq!(
        deleted_root.work_result(),
        ProviderImportWorkResult::Changed
    );
    assert!(store.authorized_source_route_for_event(beta_event).is_err());
    assert_eq!(sink.progress(), appended_progress);
    assert_eq!(sink.output_records.load(Ordering::SeqCst), 3);

    let stable_inventory =
        crate::commands::import::inventory_available_sources(&store, &[plan.source]).unwrap();
    assert_eq!(stable_inventory.sources.len(), 1);
    let stable_plan = stable_inventory.sources.into_iter().next().unwrap();
    let pages_before_stable = sink.pages.load(Ordering::SeqCst);
    let stable = import_one_source_for_search_refresh_with_profile(
        &mut store,
        &stable_plan.source,
        None,
        &stable_plan.preinventory,
        &profile,
    )
    .unwrap();
    assert_eq!(stable.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(sink.pages.load(Ordering::SeqCst), pages_before_stable);
    assert_eq!(sink.progress(), appended_progress);
    assert_eq!(sink.output_records.load(Ordering::SeqCst), 3);
}

#[test]
fn known_empty_provider_owned_root_is_scheduled_for_route_retirement() {
    let temp = tempdir();
    let root = temp.path().join("rovodev-sessions");
    let session = write_cli_rovodev_session(
        &root,
        "empty-root",
        &[cli_rovodev_message("user", "retire from empty root")],
    );
    let source = explicit_path_source(CaptureProvider::RovoDev, root.clone());
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let first = import_cli_root_with_profile(&mut store, &source, &ImportProfile::CoreOnly);
    assert_eq!(first.work_result(), ProviderImportWorkResult::Changed);
    let event = first_cli_provider_event(&store, CaptureProvider::RovoDev, "empty-root");

    fs::remove_dir_all(session).unwrap();
    let mut empty_source = explicit_path_source(CaptureProvider::RovoDev, root);
    empty_source.status = ctx_history_capture::ProviderSourceStatus::Empty;
    let inventory =
        crate::commands::import::inventory_available_sources(&store, &[empty_source]).unwrap();
    assert_eq!(inventory.sources.len(), 1);
    let plan = inventory.sources.into_iter().next().unwrap();
    let retired =
        import_one_source_for_search_refresh(&mut store, &plan.source, None, &plan.preinventory)
            .unwrap();
    assert_eq!(retired.work_result(), ProviderImportWorkResult::Changed);
    assert!(store.authorized_source_route_for_event(event).is_err());
}

#[test]
fn deepagents_manifest_stays_pending_until_pro_output_catches_up() {
    let temp = tempdir();
    let source = deepagents_manifest_source(&temp, true);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    inventory_source_import_files(&store, &source, false).unwrap();
    let failing_sink = Arc::new(RecordingProOutputSink::new(true));
    let failing_profile = ImportProfile::CoreAndPro(failing_sink.clone());

    let behind = import_one_source_for_search_refresh_with_profile(
        &mut store,
        &source,
        None,
        &SourcePreinventory::SourceImportManifest,
        &failing_profile,
    )
    .unwrap();

    assert!(behind.work_remaining);
    assert_eq!(behind.imported_events, 1);
    assert_eq!(behind.failed, 2);
    assert_eq!(
        behind.terminal_outcome(),
        ProviderImportTerminalOutcome::CoreCursorCommitted
    );
    assert!(failing_sink.behind.load(Ordering::SeqCst));
    assert_eq!(
        store
            .list_pending_source_import_files(source.provider, &source.path.display().to_string())
            .unwrap()
            .len(),
        1
    );

    let caught_up_sink = Arc::new(RecordingProOutputSink::new(false));
    let caught_up_profile = ImportProfile::CoreAndPro(caught_up_sink.clone());
    let caught_up = import_one_source_for_search_refresh_with_profile(
        &mut store,
        &source,
        None,
        &SourcePreinventory::SourceImportManifest,
        &caught_up_profile,
    )
    .unwrap();
    assert!(!caught_up.work_remaining);
    assert_eq!(caught_up.failed, 1);
    assert!(caught_up.has_accepted_content());
    assert!(caught_up_sink.pages.load(Ordering::SeqCst) > 0);
    assert!(store
        .list_pending_source_import_files(source.provider, &source.path.display().to_string())
        .unwrap()
        .is_empty());
}

#[test]
fn unchanged_codex_catalog_session_replays_on_first_pro_activation_without_core_rewrite() {
    let temp = tempdir();
    let source_root = temp.path().join("codex-sessions");
    fs::create_dir_all(&source_root).unwrap();
    let session_path = source_root.join("session.jsonl");
    let transcript = [
        json!({
            "timestamp": "2026-07-23T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": "019f92f1-0000-7000-8000-000000000001",
                "timestamp": "2026-07-23T12:00:00Z",
                "cwd": "/workspace/ctx",
                "originator": "codex-cli"
            }
        }),
        json!({
            "timestamp": "2026-07-23T12:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "exec_command",
                "call_id": "catalog-call",
                "arguments": "{\"cmd\":\"printf catalog\"}"
            }
        }),
        json!({
            "timestamp": "2026-07-23T12:00:02Z",
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": "catalog-call",
                "output": "unchanged catalog output"
            }
        }),
    ]
    .into_iter()
    .map(|record| format!("{record}\n"))
    .collect::<String>();
    fs::write(&session_path, transcript).unwrap();

    let source = explicit_path_source(CaptureProvider::Codex, source_root.clone());
    let db_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&db_path).unwrap();
    let first_inventory =
        inventory_import_sources(&store, vec![source.clone()], false, false, false).unwrap();
    assert_eq!(first_inventory.catalog.parsed_sessions, 1);
    let first_plan = first_inventory.sources.into_iter().next().unwrap();
    let first = import_one_source_for_search_refresh(
        &mut store,
        &first_plan.source,
        None,
        &first_plan.preinventory,
    )
    .unwrap();
    assert_eq!(first.failed, 0);
    assert!(first.imported_sessions > 0);
    assert!(store
        .list_pending_catalog_sessions(CaptureProvider::Codex, &source_root.display().to_string())
        .unwrap()
        .is_empty());

    let second_inventory =
        inventory_import_sources(&store, vec![source.clone()], true, true, false).unwrap();
    assert_eq!(second_inventory.catalog.cached_sessions, 1);
    assert_eq!(second_inventory.catalog.parsed_sessions, 0);
    let available_catalog_rows: i64 = Connection::open(&db_path)
        .unwrap()
        .query_row(
            "select count(*) from catalog_sessions where source_path = ?1 and is_stale = 0",
            [session_path.display().to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(available_catalog_rows, 1);
    let second_plan = second_inventory.sources.into_iter().next().unwrap();
    let core_sessions_before: i64 = Connection::open(&db_path)
        .unwrap()
        .query_row("select count(*) from sessions", [], |row| row.get(0))
        .unwrap();
    let core_events_before: i64 = Connection::open(&db_path)
        .unwrap()
        .query_row("select count(*) from events", [], |row| row.get(0))
        .unwrap();
    let sink = Arc::new(RecordingProOutputSink::new(false));
    let profile = ImportProfile::CoreAndPro(sink.clone());

    let catch_up = import_one_source_for_search_refresh_with_profile(
        &mut store,
        &second_plan.source,
        None,
        &second_plan.preinventory,
        &profile,
    )
    .unwrap();

    assert_eq!(catch_up.failed, 0);
    assert_eq!(
        Connection::open(&db_path)
            .unwrap()
            .query_row("select count(*) from sessions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        core_sessions_before
    );
    assert_eq!(
        Connection::open(&db_path)
            .unwrap()
            .query_row("select count(*) from events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        core_events_before
    );
    assert!(sink.observations.load(Ordering::SeqCst) > 0);
    assert!(sink.pages.load(Ordering::SeqCst) > 0);
    assert!(!sink.behind.load(Ordering::SeqCst));
    assert!(store
        .list_pending_catalog_sessions(CaptureProvider::Codex, &source_root.display().to_string())
        .unwrap()
        .is_empty());
}
