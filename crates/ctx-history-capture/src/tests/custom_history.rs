use super::support::*;

#[test]
fn codex_history_import_is_prompt_only_summary_fidelity_and_idempotent() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-history.jsonl");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_codex_history_jsonl(
        &fixture,
        &mut store,
        CodexHistoryImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-23T15:30:00Z".parse().unwrap(),
            ..CodexHistoryImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 2);
    assert_eq!(first.imported_events, 3);
    assert_eq!(first.imported_edges, 0);
    assert!(!store.event_search_projection_needs_backfill().unwrap());

    let second = import_codex_history_jsonl(
        &fixture,
        &mut store,
        CodexHistoryImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-23T15:30:00Z".parse().unwrap(),
            ..CodexHistoryImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_events, 3);

    let session_id = provider_session_uuid(CaptureProvider::Codex, "codex-history-session-1");
    let session = store.get_session(session_id).unwrap();
    assert_eq!(session.sync.fidelity, Fidelity::SummaryOnly);
    assert_eq!(
        session.sync.metadata["source_format"].as_str(),
        Some("codex_history_jsonl")
    );
    assert_eq!(
        session.sync.metadata["metadata"]["source_fidelity"].as_str(),
        Some("prompt_log_only")
    );
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].sync.fidelity, Fidelity::SummaryOnly);
    assert_eq!(events[0].role, Some(EventRole::User));
    assert_eq!(events[0].event_type, EventType::Message);
    assert_eq!(
        events[0].sync.metadata["source_format"].as_str(),
        Some("codex_history_jsonl")
    );
    let cursor = store
        .get_sync_cursor(
            None,
            &CodexHistoryImportOptions::default().machine_id,
            &provider_cursor_stream(CaptureProvider::Codex, "codex_history_jsonl"),
        )
        .unwrap()
        .unwrap();
    assert_eq!(cursor.cursor, "line:3");
}

#[test]
fn custom_history_jsonl_imports_full_shape_and_is_idempotent() {
    let temp = tempdir();
    let fixture = custom_history_fixture("basic.jsonl");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_custom_history_jsonl_v1(
        &fixture,
        &mut store,
        CustomHistoryJsonlV1ImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-23T12:10:00Z".parse().unwrap(),
            ..CustomHistoryJsonlV1ImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 2);
    assert_eq!(first.imported_events, 2);
    assert_eq!(first.imported_edges, 2);

    let root_provider_session_id =
        custom_history_internal_session_id("demo-agent", "demo-source", "demo-session");
    let child_provider_session_id =
        custom_history_internal_session_id("demo-agent", "demo-source", "demo-session-worker");
    let root_id = provider_session_uuid(CaptureProvider::Custom, &root_provider_session_id);
    let child_id = provider_session_uuid(CaptureProvider::Custom, &child_provider_session_id);
    let root = store.get_session(root_id).unwrap();
    let child = store.get_session(child_id).unwrap();
    assert_eq!(root.provider, CaptureProvider::Custom);
    assert_eq!(child.parent_session_id, Some(root_id));
    assert!(root
        .sync
        .metadata
        .to_string()
        .contains("\"provider_key\":\"demo-agent\""));
    let events = store.events_for_session(root_id).unwrap();
    assert_eq!(events.len(), 2);
    assert!(events[0].payload.to_string().contains("Add a parser test."));

    let conn = rusqlite::Connection::open(temp.path().join("work.sqlite")).unwrap();
    let touched: i64 = conn
        .query_row("SELECT COUNT(*) FROM files_touched", [], |row| row.get(0))
        .unwrap();
    assert_eq!(touched, 1);
    let spawned_edges: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM session_edges WHERE edge_type = 'spawned'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(spawned_edges, 1);
    let cursor_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sync_cursors WHERE stream LIKE 'provider:custom:demo-agent:%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(cursor_count, 1);
    let cursor: String = conn
        .query_row(
            "SELECT cursor FROM sync_cursors WHERE stream LIKE 'provider:custom:demo-agent:%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(cursor, "5");
    let raw_cursor_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sync_cursors WHERE stream = 'demo-agent:demo-source'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(raw_cursor_count, 0);
    drop(conn);

    let second = import_custom_history_jsonl_v1(
        &fixture,
        &mut store,
        CustomHistoryJsonlV1ImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-23T12:10:00Z".parse().unwrap(),
            ..CustomHistoryJsonlV1ImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.imported_edges, 0);
    assert_eq!(second.skipped_events, 2);
    assert_eq!(second.skipped_edges, 2);
}

#[test]
fn custom_history_jsonl_reader_import_persists_normalized_cursor() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let input = [
            r#"{"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"}"#,
            r#"{"record_type":"source","source_id":"src","provider_key":"stream-agent","source_format":"stream-v1","cursor":{"after":{"stream":"native-stream","cursor":"{\"message_id\":7}","observed_at":"2026-07-01T12:00:00Z"}}}"#,
            r#"{"record_type":"session","source_id":"src","session_id":"run","started_at":"2026-07-01T11:59:00Z"}"#,
            r#"{"record_type":"event","source_id":"src","session_id":"run","event_index":0,"event_type":"message","role":"assistant","occurred_at":"2026-07-01T12:00:00Z","preview":"stream import marker"}"#,
        ]
        .join("\n");

    let summary = import_custom_history_jsonl_v1_reader(
        std::io::Cursor::new(input.into_bytes()),
        &mut store,
        CustomHistoryJsonlV1ImportOptions {
            source_path: Some(PathBuf::from("plugin://stream-agent/default")),
            imported_at: "2026-07-01T12:01:00Z".parse().unwrap(),
            ..CustomHistoryJsonlV1ImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);
    let cursor = store
        .get_sync_cursor(
            None,
            &CustomHistoryJsonlV1ImportOptions::default().machine_id,
            &custom_history_jsonl_v1_cursor_stream("stream-agent", "src", "stream-v1"),
        )
        .unwrap()
        .unwrap();
    assert_eq!(cursor.cursor, r#"{"message_id":7}"#);
}

#[test]
fn custom_history_jsonl_reader_persists_source_only_cursor() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let input = [
            r#"{"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"}"#,
            r#"{"record_type":"source","source_id":"src","provider_key":"stream-agent","source_format":"stream-v1","cursor":{"after":{"stream":"native-stream","cursor":"{\"message_id\":9}","observed_at":"2026-07-01T12:02:00Z"}}}"#,
        ]
        .join("\n");

    let summary = import_custom_history_jsonl_v1_reader(
        std::io::Cursor::new(input.into_bytes()),
        &mut store,
        CustomHistoryJsonlV1ImportOptions {
            imported_at: "2026-07-01T12:03:00Z".parse().unwrap(),
            ..CustomHistoryJsonlV1ImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    let cursor = store
        .get_sync_cursor(
            None,
            &CustomHistoryJsonlV1ImportOptions::default().machine_id,
            &custom_history_jsonl_v1_cursor_stream("stream-agent", "src", "stream-v1"),
        )
        .unwrap()
        .unwrap();
    assert_eq!(cursor.cursor, r#"{"message_id":9}"#);
}

#[test]
fn custom_history_jsonl_malformed_import_is_atomic_by_default() {
    let temp = tempdir();
    let fixture = custom_history_fixture("malformed-partial.jsonl");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_custom_history_jsonl_v1(
        &fixture,
        &mut store,
        CustomHistoryJsonlV1ImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-23T13:10:00Z".parse().unwrap(),
            ..CustomHistoryJsonlV1ImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert_eq!(summary.failed, 1);
    assert_eq!(store.capture_source_count().unwrap(), 0);
    let conn = rusqlite::Connection::open(temp.path().join("work.sqlite")).unwrap();
    let sessions: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
        .unwrap();
    let events: i64 = conn
        .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(sessions, 0);
    assert_eq!(events, 0);
}

#[test]
fn custom_history_jsonl_rejects_oversized_line() {
    let temp = tempdir();
    let path = temp.path().join("oversized-custom.jsonl");
    write_oversized_jsonl_line(&path);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let err = import_custom_history_jsonl_v1(
        &path,
        &mut store,
        CustomHistoryJsonlV1ImportOptions::default(),
    )
    .unwrap_err();

    assert!(err.to_string().contains("provider JSONL line exceeds"));
    assert_eq!(store.capture_source_count().unwrap(), 0);
}

#[test]
fn custom_history_jsonl_preview_overrides_raw_payload_for_searchable_event_payload() {
    let temp = tempdir();
    let fixture = temp.path().join("preview-overrides-payload.jsonl");
    fs::write(
            &fixture,
            [
                r#"{"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"}"#,
                r#"{"record_type":"source","source_id":"src","provider_key":"preview-agent","source_format":"demo"}"#,
                r#"{"record_type":"session","source_id":"src","session_id":"run","started_at":"2026-06-23T14:00:00Z"}"#,
                r#"{"record_type":"event","source_id":"src","session_id":"run","event_index":0,"event_type":"message","role":"assistant","occurred_at":"2026-06-23T14:00:01Z","payload":{"raw":"unindexed-raw-payload-token"},"preview":"bounded searchable preview text"}"#,
            ]
            .join("\n"),
        )
        .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_custom_history_jsonl_v1(
        &fixture,
        &mut store,
        CustomHistoryJsonlV1ImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-23T14:10:00Z".parse().unwrap(),
            ..CustomHistoryJsonlV1ImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    let session_id = provider_session_uuid(
        CaptureProvider::Custom,
        &custom_history_internal_session_id("preview-agent", "src", "run"),
    );
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].payload["body"],
        json!({ "text": "bounded searchable preview text" })
    );
    assert!(!events[0]
        .payload
        .to_string()
        .contains("unindexed-raw-payload-token"));
    assert_eq!(
        events[0].sync.metadata["metadata"]["ctx_history_jsonl_v1"]["raw_payload"]["raw"].as_str(),
        Some("unindexed-raw-payload-token")
    );
}

#[test]
fn custom_history_jsonl_namespaces_provider_keys_to_avoid_collisions() {
    let temp = tempdir();
    let fixture = temp.path().join("same-native-ids.jsonl");
    fs::write(
            &fixture,
            [
                r#"{"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"}"#,
                r#"{"record_type":"source","source_id":"src","provider_key":"alpha","source_format":"demo"}"#,
                r#"{"record_type":"session","source_id":"src","session_id":"same","started_at":"2026-06-23T14:00:00Z"}"#,
                r#"{"record_type":"event","source_id":"src","session_id":"same","event_index":0,"event_type":"message","role":"user","occurred_at":"2026-06-23T14:00:01Z","payload":{"text":"alpha text"}}"#,
                r#"{"record_type":"source","source_id":"src-2","provider_key":"beta","source_format":"demo"}"#,
                r#"{"record_type":"session","source_id":"src-2","session_id":"same","started_at":"2026-06-23T14:01:00Z"}"#,
                r#"{"record_type":"event","source_id":"src-2","session_id":"same","event_index":0,"event_type":"message","role":"user","occurred_at":"2026-06-23T14:01:01Z","payload":{"text":"beta text"}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_custom_history_jsonl_v1(
        &fixture,
        &mut store,
        CustomHistoryJsonlV1ImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-23T14:10:00Z".parse().unwrap(),
            ..CustomHistoryJsonlV1ImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 2);
    assert_eq!(summary.imported_events, 2);
    let alpha_session = provider_session_uuid(
        CaptureProvider::Custom,
        &custom_history_internal_session_id("alpha", "src", "same"),
    );
    let beta_session = provider_session_uuid(
        CaptureProvider::Custom,
        &custom_history_internal_session_id("beta", "src-2", "same"),
    );
    assert_ne!(alpha_session, beta_session);
    assert!(store
        .events_for_session(alpha_session)
        .unwrap()
        .iter()
        .any(|event| event.payload.to_string().contains("alpha text")));
    assert!(store
        .events_for_session(beta_session)
        .unwrap()
        .iter()
        .any(|event| event.payload.to_string().contains("beta text")));
}

#[test]
fn custom_history_jsonl_hashes_delimited_identifiers_without_collisions() {
    let temp = tempdir();
    let fixture = temp.path().join("delimited-identifiers.jsonl");
    fs::write(
            &fixture,
            [
                r#"{"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"}"#,
                r#"{"record_type":"source","source_id":"a:b","provider_key":"delim-agent","source_format":"demo"}"#,
                r#"{"record_type":"session","source_id":"a:b","session_id":"c","started_at":"2026-06-23T14:00:00Z"}"#,
                r#"{"record_type":"event","source_id":"a:b","session_id":"c","event_index":0,"event_type":"message","role":"user","occurred_at":"2026-06-23T14:00:01Z","payload":{"text":"left text"}}"#,
                r#"{"record_type":"source","source_id":"a","provider_key":"delim-agent","source_format":"demo"}"#,
                r#"{"record_type":"session","source_id":"a","session_id":"b:c","started_at":"2026-06-23T14:01:00Z"}"#,
                r#"{"record_type":"event","source_id":"a","session_id":"b:c","event_index":0,"event_type":"message","role":"user","occurred_at":"2026-06-23T14:01:01Z","payload":{"text":"right text"}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_custom_history_jsonl_v1(
        &fixture,
        &mut store,
        CustomHistoryJsonlV1ImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-23T14:10:00Z".parse().unwrap(),
            ..CustomHistoryJsonlV1ImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 2);
    assert_eq!(summary.imported_events, 2);
    let left_session = provider_session_uuid(
        CaptureProvider::Custom,
        &custom_history_internal_session_id("delim-agent", "a:b", "c"),
    );
    let right_session = provider_session_uuid(
        CaptureProvider::Custom,
        &custom_history_internal_session_id("delim-agent", "a", "b:c"),
    );
    assert_ne!(left_session, right_session);
    assert!(store
        .events_for_session(left_session)
        .unwrap()
        .iter()
        .any(|event| event.payload.to_string().contains("left text")));
    assert!(store
        .events_for_session(right_session)
        .unwrap()
        .iter()
        .any(|event| event.payload.to_string().contains("right text")));
}

#[test]
fn custom_history_jsonl_dedupes_explicit_parent_child_edge_from_session_parent() {
    let temp = tempdir();
    let fixture = temp.path().join("duplicate-parent-child.jsonl");
    fs::write(
            &fixture,
            [
                r#"{"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"}"#,
                r#"{"record_type":"source","source_id":"src","provider_key":"edge-agent","source_format":"demo"}"#,
                r#"{"record_type":"session","source_id":"src","session_id":"root","started_at":"2026-06-23T15:00:00Z"}"#,
                r#"{"record_type":"session","source_id":"src","session_id":"child","parent_session_id":"root","started_at":"2026-06-23T15:00:01Z"}"#,
                r#"{"record_type":"edge","source_id":"src","from_session_id":"root","to_session_id":"child","edge_type":"parent_child","edge_id":"explicit-parent","occurred_at":"2026-06-23T15:00:02Z"}"#,
            ]
            .join("\n"),
        )
        .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_custom_history_jsonl_v1(
        &fixture,
        &mut store,
        CustomHistoryJsonlV1ImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-23T15:10:00Z".parse().unwrap(),
            ..CustomHistoryJsonlV1ImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_edges, 1);
    assert_eq!(summary.skipped_edges, 1);
    let conn = rusqlite::Connection::open(temp.path().join("work.sqlite")).unwrap();
    let parent_child_edges: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM session_edges WHERE edge_type = 'parent_child'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(parent_child_edges, 1);
}

#[test]
fn provider_fixture_replay_rejects_malformed_lines_without_partial_import_by_default() {
    let temp = tempdir();
    let fixture = provider_fixture("malformed-partial.jsonl");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary =
        import_provider_fixture_jsonl(&fixture, &mut store, fixed_import_options(fixture.clone()))
            .unwrap();

    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert_eq!(summary.failed, 1);
    let session_id = provider_session_uuid(CaptureProvider::Codex, "malformed-partial-session");
    assert!(store.events_for_session(session_id).unwrap().is_empty());
}

#[test]
fn provider_fixture_replay_allows_explicit_partial_import() {
    let temp = tempdir();
    let fixture = provider_fixture("malformed-partial.jsonl");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let mut options = fixed_import_options(fixture.clone());
    options.allow_partial_failures = true;

    let summary = import_provider_fixture_jsonl(&fixture, &mut store, options).unwrap();

    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);
    assert_eq!(summary.failed, 1);
    assert_eq!(summary.failures.len(), 1);
    assert_eq!(summary.failures[0].line, 3);
    let session_id = provider_session_uuid(CaptureProvider::Codex, "malformed-partial-session");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 2);
    assert!(events[0]
        .payload
        .to_string()
        .contains("Valid event before malformed line."));
    assert!(events[1]
        .payload
        .to_string()
        .contains("Valid event after malformed line."));
}

#[test]
fn provider_fixture_replay_rejects_expected_provider_mismatch() {
    let temp = tempdir();
    let fixture = provider_fixture("claude.jsonl");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let mut options = fixed_import_options(fixture.clone());
    options.expected_provider = Some(CaptureProvider::Codex);

    let summary = import_provider_fixture_jsonl(fixture, &mut store, options).unwrap();

    assert_eq!(summary.imported, 0);
    assert_eq!(summary.failed, 2);
    assert!(summary.failures.iter().all(|failure| failure
        .error
        .contains("has provider `claude` but expected `codex`")));
}
