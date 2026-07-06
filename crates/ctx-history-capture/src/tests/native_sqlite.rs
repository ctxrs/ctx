use super::support::*;

#[test]

fn native_opencode_imports_read_only_sqlite() {
    let temp = tempdir();
    let fixture = write_opencode_smoke_db(&temp, false);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-06-24T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0);
    assert_eq!(summary.imported_sessions, 2);
    assert_eq!(summary.imported_events, 3);
    assert_eq!(summary.imported_edges, 1);
    let parent_id = stored_provider_session_id(&store, CaptureProvider::OpenCode, "opencode-root");
    let child_id = stored_provider_session_id(&store, CaptureProvider::OpenCode, "opencode-child");
    assert_eq!(
        store.get_session(child_id).unwrap().parent_session_id,
        Some(parent_id)
    );
    let events = store.events_for_session(parent_id).unwrap();
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    assert_eq!(
        events[0].sync.metadata["source_format"].as_str(),
        Some(OPENCODE_SQLITE_SOURCE_FORMAT)
    );
}
#[test]
fn native_kilo_imports_opencode_derived_sqlite_fixture_idempotently() {
    let temp = tempdir();
    let fixture = provider_history_fixture("kilo/kilo.db");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_kilo_sqlite(
        &fixture,
        &mut store,
        KiloSqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-04T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..KiloSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 2);

    let session_id = stored_provider_session_id(&store, CaptureProvider::Kilo, "kilo-root");
    let session = store.get_session(session_id).unwrap();
    assert_eq!(session.provider, CaptureProvider::Kilo);
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[0].sync.metadata["source_format"].as_str(),
        Some(KILO_SQLITE_SOURCE_FORMAT)
    );
    assert_eq!(
        events[0].payload["body"]["session_message_seq"].as_i64(),
        Some(1)
    );
    assert_eq!(
        events[1].payload["body"]["session_message_seq"].as_i64(),
        Some(2)
    );

    let second = import_kilo_sqlite(
        &fixture,
        &mut store,
        KiloSqliteImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            ..KiloSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 2);
}
#[test]
fn native_warp_imports_sqlite_fixture_idempotently() {
    let temp = tempdir();
    let fixture = provider_history_fixture("warp/v1/warp.sqlite");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_warp_sqlite(
        &fixture,
        &mut store,
        WarpSqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-05T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..WarpSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 4);

    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Warp, "warp-conversation-1");
    let session = store.get_session(session_id).unwrap();
    assert_eq!(session.provider, CaptureProvider::Warp);
    let rendered_session = serde_json::to_string(&session.sync.metadata).unwrap();
    assert!(rendered_session.contains("Sanitized Warp Agent"));
    assert!(rendered_session.contains("has_server_conversation_token"));
    assert!(!rendered_session.contains("redacted-token-not-imported"));

    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 4);
    assert_eq!(events[0].role, Some(EventRole::User));
    assert_eq!(events[1].role, Some(EventRole::Assistant));
    assert_eq!(events[2].event_type, EventType::ToolCall);
    assert_eq!(events[3].event_type, EventType::ToolOutput);
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("warp sqlite oracle prompt"));
    assert!(rendered.contains("Warp sqlite oracle answer"));
    assert!(rendered.contains("warp_sqlite"));
    assert!(store
        .search_event_hits("Warp sqlite oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Warp)));

    let second = import_warp_sqlite(
        &fixture,
        &mut store,
        WarpSqliteImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            ..WarpSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 4);
}

#[test]
fn native_warp_import_reads_committed_wal_content() {
    let temp = tempdir();
    let fixture = provider_history_fixture("warp/v1/warp.sqlite");
    let live_db = temp.path().join("warp-live.sqlite");
    fs::copy(&fixture, &live_db).unwrap();
    let writer = Connection::open(&live_db).unwrap();
    writer.pragma_update(None, "journal_mode", "WAL").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    let conversation_data = json!({
        "agent_name": "Warp WAL Agent",
        "server_conversation_token": "redacted-token-not-imported"
    })
    .to_string();
    writer
        .execute(
            "update agent_conversations set conversation_data = ?1 where conversation_id = ?2",
            rusqlite::params![conversation_data, "warp-conversation-1"],
        )
        .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_warp_sqlite(
        &live_db,
        &mut store,
        WarpSqliteImportOptions {
            source_path: Some(live_db.clone()),
            allow_partial_failures: true,
            ..WarpSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 4);
    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Warp, "warp-conversation-1");
    let session = store.get_session(session_id).unwrap();
    let rendered_session = serde_json::to_string(&session.sync.metadata).unwrap();
    assert!(rendered_session.contains("Warp WAL Agent"));
    assert!(!rendered_session.contains("redacted-token-not-imported"));
    drop(writer);
}

#[test]
fn native_warp_rejects_changed_schema_before_querying() {
    let temp = tempdir();
    let db = temp.path().join("warp-missing-task.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE agent_conversations (
                id INTEGER PRIMARY KEY,
                conversation_id TEXT NOT NULL,
                conversation_data TEXT NOT NULL,
                last_modified_at TEXT NOT NULL
            );
            CREATE TABLE agent_tasks (
                id INTEGER PRIMARY KEY,
                conversation_id TEXT NOT NULL,
                task_id TEXT NOT NULL,
                last_modified_at TEXT NOT NULL
            );",
    )
    .unwrap();
    drop(conn);

    let err = import_warp_sqlite(
        &db,
        &mut Store::open(temp.path().join("work.sqlite")).unwrap(),
        WarpSqliteImportOptions::default(),
    )
    .unwrap_err();

    assert!(err
        .to_string()
        .contains("Warp agent_tasks table missing required column(s): task"));
}

#[test]
fn native_hermes_rejects_out_of_range_message_timestamp() {
    let temp = tempdir();
    let fixture = write_hermes_smoke_db(&temp);
    let conn = Connection::open(&fixture).unwrap();
    conn.execute(
        "update messages set timestamp = ?1 where content = 'bad timestamp'",
        [1.0e300_f64],
    )
    .unwrap();
    drop(conn);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_hermes_sqlite(
        &fixture,
        &mut store,
        HermesSqliteImportOptions {
            allow_partial_failures: true,
            ..HermesSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1);
    assert!(summary.failures[0]
        .error
        .contains("Hermes message timestamp"));
    assert_eq!(summary.imported_events, 1);
}

#[cfg(unix)]
#[test]
fn native_opencode_normalizer_rejects_symlinked_sqlite() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let fixture = write_opencode_smoke_db(&temp, false);
    let link = temp.path().join("linked-opencode.db");
    symlink(&fixture, &link).unwrap();

    let err = normalize_opencode_sqlite(
        &link,
        &ProviderAdapterContext::default(),
        &OPENCODE_SQLITE_DIALECT,
    )
    .unwrap_err();
    assert!(matches!(
        err,
        CaptureError::InvalidProviderTranscriptPath { path, reason }
            if path.ends_with("linked-opencode.db")
                && reason == "symlinked provider transcript files are rejected"
    ));
}

#[test]
fn native_opencode_synthesizes_session_message_seq_when_missing() {
    let temp = tempdir();
    let fixture = write_opencode_session_message_without_seq_db(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            allow_partial_failures: true,
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);

    let session_id =
        stored_provider_session_id(&store, CaptureProvider::OpenCode, "opencode-no-seq");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[0].payload["body"]["session_message_seq"].as_i64(),
        Some(1)
    );
    assert_eq!(
        events[1].payload["body"]["session_message_seq"].as_i64(),
        Some(2)
    );
    assert_ne!(events[0].id, events[1].id);
}

#[test]
fn native_opencode_rejects_negative_session_message_seq() {
    let temp = tempdir();
    let fixture = write_opencode_smoke_db(&temp, false);
    let conn = Connection::open(&fixture).unwrap();
    conn.execute(
        "update session_message set seq = -1 where id = 'msg-user'",
        [],
    )
    .unwrap();
    drop(conn);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            allow_partial_failures: true,
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1);
    assert!(summary.failures[0]
        .error
        .contains("OpenCode session_message seq must be nonnegative"));
    assert_eq!(summary.imported_events, 2);
    let session_id = stored_provider_session_id(&store, CaptureProvider::OpenCode, "opencode-root");
    let events = store.events_for_session(session_id).unwrap();
    assert!(events.iter().all(|event| {
        event.payload["body"]["session_message_seq"]
            .as_i64()
            .is_some_and(|seq| seq >= 0)
    }));
}

#[test]
fn native_opencode_rejects_out_of_range_message_timestamp() {
    let temp = tempdir();
    let fixture = write_opencode_smoke_db(&temp, false);
    let conn = Connection::open(&fixture).unwrap();
    let data_without_payload_time = json!({"text": "bad timestamp fallback"}).to_string();
    conn.execute(
        "update session_message set time_created = ?1, data = ?2 where id = 'msg-user'",
        rusqlite::params![i64::MAX, data_without_payload_time],
    )
    .unwrap();
    drop(conn);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            allow_partial_failures: true,
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1);
    assert!(summary.failures[0]
        .error
        .contains("OpenCode session_message time_created"));
    assert_eq!(summary.imported_events, 2);
}

#[test]
fn native_opencode_rejects_oversized_sqlite_text_value() {
    let temp = tempdir();
    let fixture = write_opencode_smoke_db(&temp, false);
    let conn = Connection::open(&fixture).unwrap();
    let oversized_data = format!(
        "{{\"time\":{{\"created\":1782259200000}},\"text\":\"{}\"}}",
        "x".repeat(MAX_PROVIDER_SQLITE_VALUE_BYTES + 1)
    );
    conn.execute(
        "update session_message set data = ?1 where id = 'msg-user'",
        [&oversized_data],
    )
    .unwrap();
    drop(conn);

    let err = import_opencode_sqlite(
        &fixture,
        &mut Store::open(temp.path().join("work.sqlite")).unwrap(),
        OpenCodeSqliteImportOptions::default(),
    )
    .unwrap_err();

    assert!(
        err.to_string().contains("too big"),
        "unexpected error: {err}"
    );
}

#[test]
fn native_opencode_reports_malformed_and_corrupt_db() {
    let temp = tempdir();
    let malformed = write_opencode_smoke_db(&temp, true);
    let corrupt = temp.path().join("corrupt-opencode.db");
    fs::write(&corrupt, b"not sqlite").unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &malformed,
        &mut store,
        OpenCodeSqliteImportOptions {
            allow_partial_failures: true,
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(summary.failed, 1);
    assert!(summary.failures[0].error.contains("invalid JSON"));

    let err = import_opencode_sqlite(&corrupt, &mut store, OpenCodeSqliteImportOptions::default())
        .unwrap_err();
    assert!(err.to_string().contains("not a database"));
}

#[test]
fn native_opencode_rejects_empty_current_schema_without_model_column() {
    let temp = tempdir();
    let fixture = write_opencode_current_schema_db(&temp, false);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            allow_partial_failures: true,
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1);
    assert!(summary.failures[0]
        .error
        .contains("no real conversational message rows"));
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn native_opencode_imports_legacy_message_table_when_session_message_is_absent() {
    let temp = tempdir();
    let fixture = write_opencode_current_schema_db(&temp, true);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            allow_partial_failures: true,
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);

    let session_id = stored_provider_session_id(&store, CaptureProvider::OpenCode, "current-root");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].sync.metadata["source_format"].as_str(),
        Some(OPENCODE_SQLITE_SOURCE_FORMAT)
    );
    assert!(events[0].payload.to_string().contains("legacy hello"));
}

#[test]
fn native_opencode_falls_back_when_session_message_is_metadata_only() {
    let temp = tempdir();
    let fixture = write_opencode_session_message_metadata_with_legacy_message_db(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            allow_partial_failures: true,
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);
    let session_id = store
        .session_by_external_session(CaptureProvider::OpenCode, "strict-root")
        .unwrap()
        .unwrap()
        .id;
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 1);
    assert!(events[0]
        .payload
        .to_string()
        .contains("legacy fallback prompt"));
    let session = store.get_session(session_id).unwrap();
    assert_eq!(
        session.sync.metadata["metadata"]["legacy_projection"]["selected_message_table"].as_str(),
        Some("message")
    );
}

#[test]
fn native_opencode_rejects_malformed_authoritative_rows_without_legacy_fallback() {
    let temp = tempdir();
    let fixture = write_opencode_session_message_malformed_with_legacy_message_db(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            allow_partial_failures: true,
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert!(summary.failures[0].error.contains("invalid JSON"));
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn native_opencode_rejects_malformed_metadata_authoritative_rows_without_legacy_fallback() {
    let temp = tempdir();
    let fixture = write_opencode_session_message_metadata_bad_seq_with_legacy_message_db(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            allow_partial_failures: true,
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{summary:?}");
    assert!(summary.failures[0]
        .error
        .contains("OpenCode session_message seq must be nonnegative"));
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn native_opencode_rejects_tool_only_sqlite_rows() {
    let temp = tempdir();
    let fixture = write_opencode_tool_only_db(&temp, "opencode-tool-only.db");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            allow_partial_failures: true,
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1);
    assert!(summary.failures[0]
        .error
        .contains("no real conversational message rows"));
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn native_opencode_falls_back_when_session_entry_is_metadata_only() {
    let temp = tempdir();
    let fixture = write_opencode_session_entry_metadata_with_legacy_message_db(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_opencode_sqlite(
        &fixture,
        &mut store,
        OpenCodeSqliteImportOptions {
            allow_partial_failures: true,
            ..OpenCodeSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);
    let session_id = store
        .session_by_external_session(CaptureProvider::OpenCode, "strict-root")
        .unwrap()
        .unwrap()
        .id;
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 1);
    assert!(events[0]
        .payload
        .to_string()
        .contains("legacy fallback prompt"));
    let session = store.get_session(session_id).unwrap();
    assert_eq!(
        session.sync.metadata["metadata"]["legacy_projection"]["selected_message_table"].as_str(),
        Some("message")
    );
}

#[test]
fn native_kilo_rejects_metadata_only_sqlite_rows() {
    let temp = tempdir();
    let fixture = write_opencode_all_metadata_db(&temp, "kilo-all-metadata.db");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_kilo_sqlite(
        &fixture,
        &mut store,
        KiloSqliteImportOptions {
            allow_partial_failures: true,
            ..KiloSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1);
    assert!(summary.failures[0]
        .error
        .contains("no real conversational message rows"));
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn native_kilo_rejects_tool_only_sqlite_rows() {
    let temp = tempdir();
    let fixture = write_opencode_tool_only_db(&temp, "kilo-tool-only.db");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_kilo_sqlite(
        &fixture,
        &mut store,
        KiloSqliteImportOptions {
            allow_partial_failures: true,
            ..KiloSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1);
    assert!(summary.failures[0]
        .error
        .contains("no real conversational message rows"));
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn native_opencode_rejects_changed_message_schema_before_querying() {
    let temp = tempdir();
    let fixture = write_opencode_future_incomplete_schema_db(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let err = import_opencode_sqlite(&fixture, &mut store, OpenCodeSqliteImportOptions::default())
        .unwrap_err();

    assert!(err
        .to_string()
        .contains("OpenCode SQLite message table missing required column(s): data"));
}

#[test]
fn openclaw_import_ignores_oversized_session_index_sidecar() {
    let temp = tempdir();
    let root = temp.path().join("openclaw");
    let sessions = root.join("agents/personal-agent/sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("sessions.json"),
        vec![b'x'; MAX_OPENCLAW_SESSION_INDEX_BYTES + 1],
    )
    .unwrap();
    fs::write(
        sessions.join("openclaw-oversized-index.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "type": "session",
                "id": "openclaw-oversized-index",
                "timestamp": "2026-06-24T12:00:00Z",
                "cwd": "/workspace"
            }),
            json!({
                "type": "message",
                "id": "openclaw-oversized-index-user",
                "timestamp": "2026-06-24T12:00:01Z",
                "message": {"role": "user", "content": "oversized sidecar should not block import"}
            })
        ),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_openclaw_history(
        &root,
        &mut store,
        OpenClawImportOptions {
            allow_partial_failures: true,
            ..OpenClawImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);
    let session_id = stored_provider_session_id(
        &store,
        CaptureProvider::OpenClaw,
        "personal-agent/openclaw-oversized-index",
    );
    let session = store.get_session(session_id).unwrap();
    assert_eq!(
        session.external_session_id.as_deref(),
        Some("personal-agent/openclaw-oversized-index")
    );
}

#[test]
fn native_shelley_imports_sessions_messages_metadata_and_citations() {
    let temp = tempdir();
    let fixture = write_shelley_smoke_db(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_shelley_sqlite(
        &fixture,
        &mut store,
        ShelleySqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-06-24T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..ShelleySqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 3);
    assert_eq!(summary.imported_events, 4);
    assert_eq!(summary.imported_edges, 1);

    let parent_id = stored_provider_session_id(&store, CaptureProvider::Shelley, "shelley-root");
    let child_id = stored_provider_session_id(&store, CaptureProvider::Shelley, "shelley-child");
    assert_eq!(
        store.get_session(child_id).unwrap().parent_session_id,
        Some(parent_id)
    );
    assert!(store
        .get_session(parent_id)
        .unwrap()
        .sync
        .metadata
        .to_string()
        .contains("queued oracle"));

    let source = store
        .capture_source_by_external_session(CaptureProvider::Shelley, "shelley-root")
        .unwrap()
        .unwrap();
    assert_eq!(
        source.descriptor.raw_source_path.as_deref(),
        fixture.to_str()
    );
    assert_eq!(source.descriptor.provider, CaptureProvider::Shelley);

    let events = store.events_for_session(parent_id).unwrap();
    assert_eq!(events.len(), 3);
    let agent_event = events
        .iter()
        .find(|event| event.sync.metadata["metadata"]["message_id"].as_str() == Some("msg-agent"))
        .expect("Shelley agent event imported");
    let tool_result_event = events
        .iter()
        .find(|event| {
            event.sync.metadata["metadata"]["message_id"].as_str() == Some("msg-tool-result")
        })
        .expect("Shelley tool-result event imported");
    assert_eq!(agent_event.event_type, EventType::ToolCall);
    assert_eq!(tool_result_event.event_type, EventType::ToolOutput);
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("shelley search oracle"));
    assert!(rendered.contains("thinking through the search"));
    assert!(rendered.contains("tool call: bash"));
    assert!(rendered.contains("tool output oracle"));
    assert!(rendered.contains("claude-opus-4-7"));
    assert!(rendered.contains("https://api.anthropic.com/v1/messages"));
    let user_event = events
        .iter()
        .find(|event| event.sync.metadata["metadata"]["message_id"].as_str() == Some("msg-user"))
        .expect("Shelley user event imported");
    assert!(user_event
        .sync
        .metadata
        .to_string()
        .contains("conversation:shelley-root:sequence:1:message:msg-user"));

    let source_path = fixture.display().to_string();
    let cursor = store
        .get_sync_cursor(
            None,
            "test-machine",
            &provider_source_cursor_stream(
                CaptureProvider::Shelley,
                SHELLEY_SQLITE_SOURCE_FORMAT,
                Some(&source_path),
            ),
        )
        .unwrap()
        .unwrap();
    assert!(cursor
        .cursor
        .contains("conversation:shelley-root:sequence:3:message:msg-tool-result"));
}

#[test]
fn native_shelley_reimport_is_idempotent() {
    let temp = tempdir();
    let fixture = write_shelley_smoke_db(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_shelley_sqlite(
        &fixture,
        &mut store,
        ShelleySqliteImportOptions {
            allow_partial_failures: true,
            ..ShelleySqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(first.imported_events, 4);

    let second = import_shelley_sqlite(
        &fixture,
        &mut store,
        ShelleySqliteImportOptions {
            allow_partial_failures: true,
            ..ShelleySqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.imported_edges, 0);
    assert_eq!(second.skipped_sessions, 3);
    assert_eq!(second.skipped_events, 4);
    assert_eq!(second.skipped_edges, 1);
}

#[test]
fn native_shelley_handles_duplicate_sequences_and_nonchat_rows() {
    let temp = tempdir();
    let fixture = write_shelley_adversarial_db(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_shelley_sqlite(
        &fixture,
        &mut store,
        ShelleySqliteImportOptions {
            allow_partial_failures: true,
            ..ShelleySqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 5);

    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Shelley, "shelley-adversarial");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 5);
    assert_eq!(
        events
            .iter()
            .map(|event| event.id)
            .collect::<BTreeSet<_>>()
            .len(),
        5
    );
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("duplicate sequence first"));
    assert!(rendered.contains("duplicate sequence second"));
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::VcsChange));
    assert!(events
        .iter()
        .any(|event| event.sync.metadata["metadata"]["message_type"].as_str() == Some("warning")));

    let large = events
        .iter()
        .find(|event| event.sync.metadata["metadata"]["message_id"].as_str() == Some("msg-large"))
        .expect("large Shelley event imported");
    assert_eq!(large.payload["body"]["truncated"].as_bool(), Some(true));
    assert!(
        large.payload["body"]["text"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            <= PROVIDER_MAX_TEXT_CHARS
    );
}

#[test]
fn native_shelley_text_extraction_is_not_duplicate_or_unbounded() {
    let text = shelley_value_text(&json!({
        "Content": [
            {"Type": 2, "Text": "once"}
        ]
    }))
    .unwrap();
    assert_eq!(text, "once");

    let huge = "x".repeat(PROVIDER_MAX_TEXT_CHARS + 200);
    let text = shelley_value_text(&json!({
        "Content": [
            {"Type": 2, "Text": huge},
            {"Type": 2, "Text": "after cap"}
        ]
    }))
    .unwrap();
    assert_eq!(text.chars().count(), PROVIDER_MAX_TEXT_CHARS + 1);
    assert!(!text.contains("after cap"));
}

#[test]
fn native_shelley_event_index_uses_stable_message_identity() {
    let message = ShelleyMessageRow {
        rowid: 1,
        message_id: "msg-stable".to_owned(),
        conversation_id: "conv-stable".to_owned(),
        sequence_id: 42,
        entry_type: "user".to_owned(),
        llm_data: None,
        user_data: None,
        usage_data: None,
        created_at: None,
        display_data: None,
        excluded_from_context: false,
        generation: None,
        llm_api_url: None,
        model_name: None,
        forked_from_message_id: None,
    };
    let mut moved_row = message.clone();
    moved_row.rowid = 999;
    let mut duplicate_sequence = message.clone();
    duplicate_sequence.message_id = "msg-stable-other".to_owned();

    assert_eq!(
        shelley_event_index(&message),
        shelley_event_index(&moved_row)
    );
    assert_ne!(
        shelley_event_index(&message),
        shelley_event_index(&duplicate_sequence)
    );
}

#[test]
fn native_shelley_reports_malformed_and_corrupt_db() {
    let temp = tempdir();
    let malformed = write_shelley_malformed_db(&temp);
    let corrupt = temp.path().join("corrupt-shelley.db");
    fs::write(&corrupt, b"not sqlite").unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let err = import_shelley_sqlite(
        &malformed,
        &mut store,
        ShelleySqliteImportOptions::default(),
    )
    .unwrap_err();
    assert!(err
        .to_string()
        .contains("Shelley messages table missing required column(s): type"));

    let err = import_shelley_sqlite(&corrupt, &mut store, ShelleySqliteImportOptions::default())
        .unwrap_err();
    assert!(err.to_string().contains("not a database"));
}

#[test]
fn provider_sources_discovers_shelley_default_db() {
    let temp = tempdir();
    let db = temp.path().join(".config/shelley/shelley.db");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    fs::write(&db, b"not inspected by source probe").unwrap();

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Shelley);
    let source = sources
        .iter()
        .find(|source| source.source_format == SHELLEY_SQLITE_SOURCE_FORMAT)
        .unwrap_or_else(|| panic!("missing Shelley source in {sources:#?}"));
    assert_eq!(source.provider, CaptureProvider::Shelley);
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.import_support, ProviderImportSupport::Native);
    assert_eq!(source.path, db);
}
