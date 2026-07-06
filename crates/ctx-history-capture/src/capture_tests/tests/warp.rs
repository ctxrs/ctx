#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn native_warp_imports_sqlite_fixture_idempotently() {
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

    let session_id = provider_session_uuid(CaptureProvider::Warp, "warp-conversation-1");
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
pub(crate) fn native_warp_import_reads_committed_wal_content() {
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
    let session_id = provider_session_uuid(CaptureProvider::Warp, "warp-conversation-1");
    let session = store.get_session(session_id).unwrap();
    let rendered_session = serde_json::to_string(&session.sync.metadata).unwrap();
    assert!(rendered_session.contains("Warp WAL Agent"));
    assert!(!rendered_session.contains("redacted-token-not-imported"));
    drop(writer);
}

#[test]
pub(crate) fn native_warp_rejects_changed_schema_before_querying() {
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
