use super::*;

fn codex_output_session(session_id: &str) -> String {
    [
        json!({
            "timestamp": "2026-07-27T00:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": session_id,
                "timestamp": "2026-07-27T00:00:00Z",
                "cwd": "/workspace",
                "source": "cli"
            }
        }),
        json!({
            "timestamp": "2026-07-27T00:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "exec_command",
                "call_id": "cold-output",
                "arguments": {"cmd": "git status"}
            }
        }),
        json!({
            "timestamp": "2026-07-27T00:00:02Z",
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": "cold-output",
                "output": "Process exited with code 1\ncold replay output"
            }
        }),
    ]
    .into_iter()
    .map(|row| format!("{row}\n"))
    .collect()
}

fn core_counts(path: &Path) -> (i64, i64, i64) {
    let connection = Connection::open(path).unwrap();
    (
        connection
            .query_row("select count(*) from sessions", [], |row| row.get(0))
            .unwrap(),
        connection
            .query_row("select count(*) from events", [], |row| row.get(0))
            .unwrap(),
        connection
            .query_row("select count(*) from sync_cursors", [], |row| row.get(0))
            .unwrap(),
    )
}

#[test]
fn cold_core_then_pro_replay_matches_control_and_is_failure_independent() {
    let temp = tempdir();
    let source_root = temp.path().join("codex-sessions");
    fs::create_dir_all(&source_root).unwrap();
    fs::write(
        source_root.join("session.jsonl"),
        codex_output_session("019f5a54-67de-7422-9841-e9872df75f50"),
    )
    .unwrap();
    let source = explicit_path_source(CaptureProvider::Codex, source_root.clone());

    let control_path = temp.path().join("control.sqlite");
    let mut control_store = Store::open(&control_path).unwrap();
    let control_inventory =
        inventory_import_sources(&control_store, vec![source.clone()], false, false, false)
            .unwrap();
    let control_plan = control_inventory.sources.into_iter().next().unwrap();
    let control_sink = Arc::new(RecordingProOutputSink::new(false));
    let control = import_one_source_with_profile(
        &mut control_store,
        &control_plan.source,
        None,
        false,
        &control_plan.preinventory,
        &ImportProfile::CoreAndPro(control_sink.clone()),
    )
    .unwrap();
    drop(control_store);

    let cold_path = temp.path().join("cold.sqlite");
    let context = ProviderAdapterContext::default();
    let outcome = build_codex_cold_store(CodexColdStoreOptions {
        source_path: source_root,
        target_store_path: cold_path.clone(),
        machine_id: context.machine_id,
        imported_at: context.imported_at,
        history_record: None,
        max_source_files: None,
        max_total_bytes: None,
        prompt_history: None,
    })
    .unwrap();
    let CodexColdStoreOutcome::Installed { summary, .. } = outcome else {
        panic!("fresh target must install");
    };
    assert_eq!(summary.imported_sessions, control.imported_sessions);
    assert_eq!(summary.imported_events, control.imported_events);
    assert_eq!(core_counts(&cold_path), core_counts(&control_path));

    let replay_sink = Arc::new(RecordingProOutputSink::new(false));
    let replay_profile = ImportProfile::ProReplayOnly(replay_sink.clone());
    let before = core_counts(&cold_path);
    let mut cold_store = Store::open(&cold_path).unwrap();
    import_one_source_with_profile(
        &mut cold_store,
        &source,
        None,
        false,
        &SourcePreinventory::None,
        &replay_profile,
    )
    .unwrap();
    assert_eq!(core_counts(&cold_path), before);
    assert_eq!(
        replay_sink.output_records.load(Ordering::SeqCst),
        control_sink.output_records.load(Ordering::SeqCst)
    );
    let replay_pages = replay_sink.pages.load(Ordering::SeqCst);
    drop(cold_store);

    let mut restarted = Store::open(&cold_path).unwrap();
    import_one_source_with_profile(
        &mut restarted,
        &source,
        None,
        false,
        &SourcePreinventory::None,
        &replay_profile,
    )
    .unwrap();
    assert_eq!(replay_sink.pages.load(Ordering::SeqCst), replay_pages);
    assert_eq!(core_counts(&cold_path), before);

    let failing_sink = Arc::new(RecordingProOutputSink::new(true));
    let failed_replay = import_one_source_with_profile(
        &mut restarted,
        &source,
        None,
        false,
        &SourcePreinventory::None,
        &ImportProfile::ProReplayOnly(failing_sink.clone()),
    );
    assert!(failed_replay.is_ok(), "{failed_replay:?}");
    assert!(failing_sink.behind.load(Ordering::SeqCst));
    assert_eq!(core_counts(&cold_path), before);
}

fn imported_hermes_source() -> (tempfile::TempDir, SourceInfo, std::path::PathBuf) {
    let temp = tempdir();
    let source_path = temp.path().join("hermes.db");
    let source_db = Connection::open(&source_path).unwrap();
    source_db
        .execute_batch(
            "create table sessions (
                 id text primary key,
                 source text not null,
                 started_at real not null
             );
             create table messages (
                 id integer primary key,
                 session_id text not null,
                 role text not null,
                 content text,
                 tool_call_id text,
                 timestamp real not null,
                 finish_reason text
             );
             insert into sessions values ('session-id', 'acp', 1782259200.0);
             insert into messages values (
                 1, 'session-id', 'tool', 'complete output', 'call-id',
                 1782259201.0, 'success'
             );",
        )
        .unwrap();
    drop(source_db);

    let source = explicit_path_source(CaptureProvider::Hermes, source_path);
    let db_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&db_path).unwrap();
    let inventory =
        inventory_import_sources(&store, vec![source.clone()], false, false, false).unwrap();
    let plan = inventory.sources.into_iter().next().unwrap();
    let summary =
        import_one_source_for_search_refresh(&mut store, &plan.source, None, &plan.preinventory)
            .unwrap();
    assert!(summary.imported_sessions > 0);
    let cursor_count: i64 = Connection::open(&db_path)
        .unwrap()
        .query_row("select count(*) from sync_cursors", [], |row| row.get(0))
        .unwrap();
    assert!(cursor_count > 0);
    (temp, source, db_path)
}

#[test]
fn first_pro_activation_replays_an_unchanged_core_source_then_catch_up_is_a_noop() {
    let (_temp, source, db_path) = imported_hermes_source();
    let mut store = Store::open(&db_path).unwrap();
    let inventory =
        inventory_import_sources(&store, vec![source.clone()], false, false, false).unwrap();
    let plan = inventory.sources.into_iter().next().unwrap();
    assert!(store
        .list_pending_source_import_files(source.provider, &source.path.display().to_string())
        .unwrap()
        .is_empty());
    let core_events_before: i64 = Connection::open(&db_path)
        .unwrap()
        .query_row("select count(*) from events", [], |row| row.get(0))
        .unwrap();
    let sink = Arc::new(RecordingProOutputSink::new(false));
    let profile = ImportProfile::CoreAndPro(sink.clone());

    let summary = import_one_source_for_search_refresh_with_profile(
        &mut store,
        &plan.source,
        None,
        &plan.preinventory,
        &profile,
    )
    .unwrap();

    let core_events_after: i64 = Connection::open(&db_path)
        .unwrap()
        .query_row("select count(*) from events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(core_events_after, core_events_before);
    assert_eq!(summary.failed, 0);
    assert!(sink.observations.load(Ordering::SeqCst) > 0);
    assert!(sink.pages.load(Ordering::SeqCst) > 0);
    assert!(!sink.behind.load(Ordering::SeqCst));

    let pages_after_activation = sink.pages.load(Ordering::SeqCst);
    let second = import_one_source_for_search_refresh_with_profile(
        &mut store,
        &plan.source,
        None,
        &plan.preinventory,
        &profile,
    )
    .unwrap();
    assert_eq!(second.failed, 0);
    assert_eq!(
        sink.pages.load(Ordering::SeqCst),
        pages_after_activation,
        "an unchanged source at its terminal private cursor must not materialize another page"
    );
    assert_eq!(
        Connection::open(&db_path)
            .unwrap()
            .query_row("select count(*) from events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        core_events_before
    );
}

#[test]
fn pro_output_failure_does_not_fail_unchanged_core_import() {
    let (_temp, source, db_path) = imported_hermes_source();
    let mut store = Store::open(&db_path).unwrap();
    let inventory =
        inventory_import_sources(&store, vec![source.clone()], false, false, false).unwrap();
    let plan = inventory.sources.into_iter().next().unwrap();
    let core_events_before: i64 = Connection::open(&db_path)
        .unwrap()
        .query_row("select count(*) from events", [], |row| row.get(0))
        .unwrap();
    let sink = Arc::new(RecordingProOutputSink::new(true));
    let profile = ImportProfile::CoreAndPro(sink.clone());

    let result = import_one_source_for_search_refresh_with_profile(
        &mut store,
        &plan.source,
        None,
        &plan.preinventory,
        &profile,
    );

    assert!(result.is_ok(), "{result:?}");
    let core_events_after: i64 = Connection::open(&db_path)
        .unwrap()
        .query_row("select count(*) from events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(core_events_after, core_events_before);
    assert!(sink.observations.load(Ordering::SeqCst) > 0);
    assert!(sink.pages.load(Ordering::SeqCst) > 0);
    assert!(sink.behind.load(Ordering::SeqCst));
}
