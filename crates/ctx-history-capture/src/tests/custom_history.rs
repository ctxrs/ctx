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

    let session_id = provider_import_session_id_for_path(
        CaptureProvider::Codex,
        "codex_history_jsonl",
        &fixture,
        "codex-history-session-1",
    );
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
    let source_path = fixture.display().to_string();
    let cursor = store
        .get_sync_cursor(
            None,
            &CodexHistoryImportOptions::default().machine_id,
            &provider_source_cursor_stream(
                CaptureProvider::Codex,
                "codex_history_jsonl",
                Some(&source_path),
            ),
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
fn custom_history_jsonl_imports_valid_records_reports_rejections_and_remains_retryable() {
    let temp = tempdir();
    let fixture = custom_history_fixture("malformed-mixed.jsonl");
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

    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);
    assert_eq!(summary.failed, 1);
    assert_eq!(store.capture_source_count().unwrap(), 1);
    let conn = rusqlite::Connection::open(temp.path().join("work.sqlite")).unwrap();
    let sessions: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
        .unwrap();
    let events: i64 = conn
        .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(sessions, 1);
    assert_eq!(events, 1);
    let cursors: i64 = conn
        .query_row("SELECT COUNT(*) FROM sync_cursors", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        cursors, 0,
        "a rejected record must keep the source retryable"
    );

    drop(conn);
    let retry = import_custom_history_jsonl_v1(
        &fixture,
        &mut store,
        CustomHistoryJsonlV1ImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-23T13:11:00Z".parse().unwrap(),
            ..CustomHistoryJsonlV1ImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(retry.imported_sessions, 0);
    assert_eq!(retry.imported_events, 0);
    assert_eq!(retry.skipped_sessions, 1);
    assert_eq!(retry.skipped_events, 1);
    assert_eq!(retry.failed, 1);
    assert!(retry.accepted_content_records > 0);
    let conn = rusqlite::Connection::open(temp.path().join("work.sqlite")).unwrap();
    let cursors: i64 = conn
        .query_row("SELECT COUNT(*) FROM sync_cursors", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        cursors, 0,
        "retry must not advance past the rejected record"
    );
}

#[test]
fn custom_history_semantic_event_rejection_keeps_independent_valid_event() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let input = concat!(
        r#"{"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"}"#,
        "\n",
        r#"{"record_type":"source","source_id":"src","provider_key":"semantic-agent","source_format":"semantic-v1","cursor":{"after":{"stream":"semantic:src","cursor":"2","observed_at":"2026-07-13T12:00:02Z"}}}"#,
        "\n",
        r#"{"record_type":"session","source_id":"src","session_id":"valid","started_at":"2026-07-13T12:00:00Z"}"#,
        "\n",
        r#"{"record_type":"event","source_id":"src","session_id":"valid","event_index":0,"event_type":"message","role":"user","occurred_at":"2026-07-13T12:00:00Z","payload":{"text":"accepted semantic event"}}"#,
        "\n",
        r#"{"record_type":"event","source_id":"src","session_id":"missing","event_index":1,"event_type":"message","role":"assistant","occurred_at":"2026-07-13T12:00:01Z","payload":{"text":"rejected semantic event"}}"#,
        "\n",
    );

    let summary = import_custom_history_jsonl_v1_reader(
        std::io::Cursor::new(input),
        &mut store,
        CustomHistoryJsonlV1ImportOptions::default(),
    )
    .unwrap();

    assert_eq!(summary.imported_sessions, 1, "{:?}", summary.failures);
    assert_eq!(summary.imported_events, 1, "{:?}", summary.failures);
    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert!(summary.failures[0]
        .error
        .contains("event references unknown session `missing`"));
    assert_eq!(
        store
            .events_for_session(store.list_sessions().unwrap()[0].id)
            .unwrap()
            .len(),
        1
    );
    let conn = rusqlite::Connection::open(temp.path().join("work.sqlite")).unwrap();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM sync_cursors", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn custom_history_semantic_rejection_isolated_across_sources() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let input = concat!(
        r#"{"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"}"#,
        "\n",
        r#"{"record_type":"source","source_id":"bad","provider_key":"semantic-agent","source_format":"semantic-v1"}"#,
        "\n",
        r#"{"record_type":"session","source_id":"bad","session_id":"unused","started_at":"2026-07-13T12:00:00Z"}"#,
        "\n",
        r#"{"record_type":"event","source_id":"bad","session_id":"missing","event_index":0,"event_type":"message","role":"user","occurred_at":"2026-07-13T12:00:00Z","payload":{"text":"bad source event"}}"#,
        "\n",
        r#"{"record_type":"source","source_id":"good","provider_key":"semantic-agent","source_format":"semantic-v1"}"#,
        "\n",
        r#"{"record_type":"session","source_id":"good","session_id":"kept","started_at":"2026-07-13T12:00:01Z"}"#,
        "\n",
        r#"{"record_type":"event","source_id":"good","session_id":"kept","event_index":0,"event_type":"message","role":"user","occurred_at":"2026-07-13T12:00:01Z","payload":{"text":"good source event"}}"#,
        "\n",
    );

    let summary = import_custom_history_jsonl_v1_reader(
        std::io::Cursor::new(input),
        &mut store,
        CustomHistoryJsonlV1ImportOptions::default(),
    )
    .unwrap();

    assert_eq!(summary.imported_sessions, 1, "{:?}", summary.failures);
    assert_eq!(summary.imported_events, 1, "{:?}", summary.failures);
    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    let conn = rusqlite::Connection::open(temp.path().join("work.sqlite")).unwrap();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM sessions", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        1,
        "unused scaffolding from the rejected source must not persist"
    );
}

#[test]
fn custom_history_only_invalid_content_persists_no_scaffolding() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let input = concat!(
        r#"{"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"}"#,
        "\n",
        r#"{"record_type":"source","source_id":"src","provider_key":"semantic-agent","source_format":"semantic-v1"}"#,
        "\n",
        r#"{"record_type":"session","source_id":"src","session_id":"unused","started_at":"2026-07-13T12:00:00Z"}"#,
        "\n",
        r#"{"record_type":"event","source_id":"src","session_id":"missing","event_index":0,"event_type":"message","role":"user","occurred_at":"2026-07-13T12:00:00Z","payload":{"text":"invalid only event"}}"#,
        "\n",
    );

    let summary = import_custom_history_jsonl_v1_reader(
        std::io::Cursor::new(input),
        &mut store,
        CustomHistoryJsonlV1ImportOptions::default(),
    )
    .unwrap();

    assert_eq!(summary.imported_sessions, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_events, 0, "{:?}", summary.failures);
    assert_eq!(summary.accepted_content_records, 0);
    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    let conn = rusqlite::Connection::open(temp.path().join("work.sqlite")).unwrap();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM sessions", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM events", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn custom_history_edges_cross_shared_transaction_batch_boundary() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let mut records = vec![
        json!({"record_type": "manifest", "schema_version": "ctx-history-jsonl-v1"}),
        json!({
            "record_type": "source",
            "source_id": "src",
            "provider_key": "edge-agent",
            "source_format": "edge-v1"
        }),
    ];
    for index in 0..70 {
        records.push(json!({
            "record_type": "session",
            "source_id": "src",
            "session_id": format!("session-{index}"),
            "started_at": "2026-07-13T12:00:00Z"
        }));
        records.push(json!({
            "record_type": "event",
            "source_id": "src",
            "session_id": format!("session-{index}"),
            "event_index": 0,
            "event_type": "message",
            "role": "user",
            "occurred_at": "2026-07-13T12:00:00Z",
            "payload": {"text": format!("edge batch event {index}")}
        }));
    }
    for index in 1..70 {
        records.push(json!({
            "record_type": "edge",
            "source_id": "src",
            "from_session_id": "session-0",
            "to_session_id": format!("session-{index}"),
            "edge_type": "spawned",
            "edge_id": format!("edge-{index}"),
            "confidence": "explicit"
        }));
    }
    let input = records
        .into_iter()
        .map(|record| serde_json::to_string(&record).unwrap())
        .collect::<Vec<_>>()
        .join("\n");

    let summary = import_custom_history_jsonl_v1_reader(
        std::io::Cursor::new(input),
        &mut store,
        CustomHistoryJsonlV1ImportOptions::default(),
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_events, 70);
    assert_eq!(summary.imported_edges, 69);
    store.checkpoint_wal_truncate_required().unwrap();
}

#[test]
fn codex_history_jsonl_skips_oversized_line_and_imports_remaining_records() {
    let temp = tempdir();
    let path = temp.path().join("codex-history-oversized.jsonl");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(
        jsonl_line(json!({
            "session_id": "codex-history-oversized",
            "ts": 1783170000,
            "text": "before oversized codex history"
        }))
        .as_bytes(),
    );
    bytes.extend_from_slice(&oversized_jsonl_line());
    bytes.extend_from_slice(
        jsonl_line(json!({
            "session_id": "codex-history-oversized",
            "ts": 1783170001,
            "text": "after oversized codex history"
        }))
        .as_bytes(),
    );
    fs::write(&path, bytes).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_codex_history_jsonl(
        &path,
        &mut store,
        CodexHistoryImportOptions {
            source_path: Some(path.clone()),
            imported_at: "2026-07-04T13:30:00Z".parse().unwrap(),
            ..CodexHistoryImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.skipped, 1);
    assert_eq!(summary.skipped_events, 1);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);
    let session_id = provider_import_session_id_for_path(
        CaptureProvider::Codex,
        "codex_history_jsonl",
        &path,
        "codex-history-oversized",
    );
    let events = store.events_for_session(session_id).unwrap();
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("before oversized codex history"));
    assert!(rendered.contains("after oversized codex history"));
}

#[test]
fn custom_history_jsonl_skips_oversized_record_and_imports_remaining_records() {
    let temp = tempdir();
    let path = temp.path().join("oversized-custom.jsonl");
    let mut bytes = Vec::new();
    for line in [
        jsonl_line(json!({
            "record_type": "manifest",
            "schema_version": "ctx-history-jsonl-v1"
        })),
        jsonl_line(json!({
            "record_type": "source",
            "source_id": "src",
            "provider_key": "custom-agent",
            "source_format": "demo"
        })),
        jsonl_line(json!({
            "record_type": "session",
            "source_id": "src",
            "session_id": "run",
            "started_at": "2026-07-04T13:00:00Z"
        })),
        jsonl_line(json!({
            "record_type": "event",
            "source_id": "src",
            "session_id": "run",
            "event_index": 0,
            "event_type": "message",
            "role": "user",
            "occurred_at": "2026-07-04T13:00:01Z",
            "preview": "before oversized custom history"
        })),
    ] {
        bytes.extend_from_slice(line.as_bytes());
    }
    bytes.extend_from_slice(&oversized_jsonl_line());
    bytes.extend_from_slice(
        jsonl_line(json!({
            "record_type": "event",
            "source_id": "src",
            "session_id": "run",
            "event_index": 1,
            "event_type": "message",
            "role": "assistant",
            "occurred_at": "2026-07-04T13:00:02Z",
            "preview": "after oversized custom history"
        }))
        .as_bytes(),
    );
    fs::write(&path, bytes).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_custom_history_jsonl_v1(
        &path,
        &mut store,
        CustomHistoryJsonlV1ImportOptions {
            source_path: Some(path.clone()),
            imported_at: "2026-07-04T13:30:00Z".parse().unwrap(),
            ..CustomHistoryJsonlV1ImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.skipped, 1);
    assert_eq!(summary.skipped_events, 1);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);
    let session_id = provider_session_uuid(
        CaptureProvider::Custom,
        &custom_history_internal_session_id("custom-agent", "src", "run"),
    );
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 2);
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("before oversized custom history"));
    assert!(rendered.contains("after oversized custom history"));
}

#[test]
fn custom_history_jsonl_preview_preserves_payload_and_metadata() {
    let temp = tempdir();
    let fixture = temp.path().join("preview-preserves-payload.jsonl");
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
        events[0].payload["body"]["raw"].as_str(),
        Some("unindexed-raw-payload-token")
    );
    assert_eq!(
        events[0].sync.metadata["metadata"]["ctx_history_jsonl_v1"]["preview"].as_str(),
        Some("bounded searchable preview text")
    );
}

#[test]
fn custom_history_jsonl_imports_payload_text() {
    let temp = tempdir();
    let fixture = temp.path().join("payload-event.jsonl");
    fs::write(
        &fixture,
        [
            r#"{"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"}"#,
            r#"{"record_type":"source","source_id":"src","provider_key":"payload-agent","source_format":"demo"}"#,
            r#"{"record_type":"session","source_id":"src","session_id":"run","started_at":"2026-06-23T14:00:00Z"}"#,
            r#"{"record_type":"event","source_id":"src","session_id":"run","event_index":0,"event_type":"message","role":"assistant","occurred_at":"2026-06-23T14:00:01Z","payload":{"text":"custompayloadimport local payload text"}}"#,
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
        &custom_history_internal_session_id("payload-agent", "src", "run"),
    );
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 1);
    assert!(events[0]
        .payload
        .to_string()
        .contains("custompayloadimport local payload text"));

    let hits = store.search_event_hits("custompayloadimport", 10).unwrap();
    assert!(hits.iter().any(|hit| hit.event_id == events[0].id));
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
fn provider_fixture_replay_imports_valid_rows_and_reports_malformed_rows() {
    let temp = tempdir();
    let fixture = provider_fixture("malformed-mixed.jsonl");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary =
        import_provider_fixture_jsonl(&fixture, &mut store, fixed_import_options(fixture.clone()))
            .unwrap();

    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);
    assert_eq!(summary.failed, 1);
    assert!(summary.accepted_content_records > 0);
    assert_eq!(summary.failures.len(), 1);
    assert_eq!(summary.failures[0].line, 3);
    let session_id =
        provider_fixture_session_id(CaptureProvider::Codex, "malformed-mixed-session", &fixture);
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
    let source_path = fixture.display().to_string();
    let cursor_stream = provider_source_cursor_stream(
        CaptureProvider::Codex,
        "normalized_provider_fixture_jsonl",
        Some(&source_path),
    );
    assert!(store
        .get_sync_cursor(None, "test-machine", &cursor_stream)
        .unwrap()
        .is_none());
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
