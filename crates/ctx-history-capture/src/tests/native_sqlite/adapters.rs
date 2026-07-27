use crate::tests::support::fixtures::sqlite::write_hermes_smoke_db;
use crate::tests::support::paths::{provider_history_fixture, tempdir};
use crate::tests::support::provider_state::stored_provider_session_id;
use crate::tests::support::source_snapshot::sqlite_file_snapshot;
use crate::{
    import_hermes_sqlite, import_openclaw_history, import_warp_sqlite, HermesSqliteImportOptions,
    OpenClawImportOptions, WarpSqliteImportOptions, MAX_OPENCLAW_SESSION_INDEX_BYTES,
};
use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EventRole, EventType};
use ctx_history_store::Store;
use rusqlite::Connection;
use serde_json::json;
use std::fs;

#[test]
fn native_warp_imports_sqlite_fixture_idempotently() {
    let temp = tempdir();
    let fixture = provider_history_fixture("warp/v1/warp.sqlite");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let options = WarpSqliteImportOptions {
        machine_id: "test-machine".into(),
        source_path: Some(fixture.clone()),
        imported_at: DateTime::parse_from_rfc3339("2026-07-05T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
        ..WarpSqliteImportOptions::default()
    };
    let first = import_warp_sqlite(&fixture, &mut store, options.clone()).unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 3);

    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Warp, "warp-conversation-1");
    let session = store.get_session(session_id).unwrap();
    assert_eq!(session.provider, CaptureProvider::Warp);
    let rendered_session = serde_json::to_string(&session.sync.metadata).unwrap();
    assert!(rendered_session.contains("Sanitized Warp Agent"));
    assert!(rendered_session.contains("server_conversation_token_present"));
    assert!(!rendered_session.contains("warp-server-token-fixture"));

    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].role, Some(EventRole::User));
    assert_eq!(events[1].role, Some(EventRole::Assistant));
    assert_eq!(events[2].event_type, EventType::ToolCall);
    assert!(events
        .iter()
        .all(|event| event.event_type != EventType::ToolOutput));
    assert!(store.runs_for_session(session_id).unwrap().is_empty());
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("warp sqlite oracle prompt"));
    assert!(rendered.contains("Warp sqlite oracle answer"));
    assert!(rendered.contains("warp_sqlite"));
    assert!(store
        .search_event_hits("Warp sqlite oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Warp)));

    let second = import_warp_sqlite(&fixture, &mut store, options).unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 0);
    assert_eq!(second.skipped_events, 0);
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
        "server_conversation_token": "warp-server-token-preserved"
    })
    .to_string();
    writer
        .execute(
            "update agent_conversations set conversation_data = ?1 where conversation_id = ?2",
            rusqlite::params![conversation_data, "warp-conversation-1"],
        )
        .unwrap();
    let before_import = sqlite_file_snapshot(&live_db);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_warp_sqlite(
        &live_db,
        &mut store,
        WarpSqliteImportOptions {
            source_path: Some(live_db.clone()),
            ..WarpSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 3);
    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Warp, "warp-conversation-1");
    let session = store.get_session(session_id).unwrap();
    let rendered_session = serde_json::to_string(&session.sync.metadata).unwrap();
    assert!(rendered_session.contains("Warp WAL Agent"));
    assert!(rendered_session.contains("server_conversation_token_present"));
    assert!(!rendered_session.contains("warp-server-token-preserved"));
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 3);
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    assert!(events
        .iter()
        .all(|event| event.event_type != EventType::ToolOutput));
    assert!(store.runs_for_session(session_id).unwrap().is_empty());
    assert_eq!(sqlite_file_snapshot(&live_db), before_import);
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
