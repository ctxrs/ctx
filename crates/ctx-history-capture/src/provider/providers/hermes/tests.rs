use std::path::Path;

use rusqlite::Connection;
use serde_json::Value;

use super::{
    layout::HermesSchema,
    sqlite::{
        HermesFrontier, HermesNativeRecord, HermesPhase, HermesRowReader, HERMES_FRONTIER_VERSION,
        HERMES_LOCATOR_KIND,
    },
    *,
};
use crate::test_support_paths::tempdir;

fn create_fixture(path: &Path, session: &str) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (
            id TEXT PRIMARY KEY,
            source TEXT NOT NULL,
            parent_session_id TEXT,
            started_at REAL NOT NULL
        );
        CREATE TABLE messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            role TEXT NOT NULL,
            content TEXT,
            tool_call_id TEXT,
            tool_name TEXT,
            timestamp REAL NOT NULL,
            finish_reason TEXT,
            active INTEGER NOT NULL DEFAULT 1,
            compacted INTEGER NOT NULL DEFAULT 0
        );",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sessions (id, source, started_at) VALUES (?1, 'acp', 1782259200.0)",
        [session],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages
         (session_id, role, content, timestamp, finish_reason)
         VALUES (?1, 'assistant', 'ordinary core message', 1782259201.0, 'stop')",
        [session],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages
         (session_id, role, content, tool_call_id, tool_name, timestamp, finish_reason)
         VALUES (?1, 'tool', 'successful private bytes', 'call-success', 'shell',
                 1782259202.0, 'success')",
        [session],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages
         (session_id, role, content, tool_call_id, tool_name, timestamp, finish_reason)
         VALUES (?1, 'tool', 'failed diagnostic bytes', 'call-failure', 'shell',
                 1782259203.0, 'error')",
        [session],
    )
    .unwrap();
}
#[test]
fn provider_frontier_and_locator_are_exact_and_versioned() {
    let frontier = HermesFrontier {
        phase: HermesPhase::Messages,
        next_ordinal: 42,
        rowid: -7,
    };
    assert_eq!(
        HermesFrontier::decode(&frontier.encode()).unwrap(),
        frontier
    );
    assert_eq!(HERMES_FRONTIER_VERSION, 1);
    assert_eq!(HERMES_LOCATOR_KIND, "hermes-sqlite-row-v1");
    assert!(HermesFrontier::decode(&frontier.encode()[..16]).is_err());
}

#[test]
fn minimum_sqlite_rowid_is_distinct_from_the_initial_frontier() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("state.db");
    create_fixture(&path, "minimum-rowid-session");
    Connection::open(&path)
        .unwrap()
        .execute(
            "UPDATE sessions SET rowid = ?1 WHERE id = 'minimum-rowid-session'",
            [i64::MIN],
        )
        .unwrap();
    Connection::open(&path)
        .unwrap()
        .execute(
            "INSERT INTO messages
             (id, session_id, role, content, timestamp)
             VALUES (?1, 'minimum-rowid-session', 'assistant', 'minimum rowid message', 1782259199.0)",
            [i64::MIN],
        )
        .unwrap();

    let conn = crate::provider::sqlite::open_provider_sqlite_readonly(
        crate::test_provider_sqlite_data_root(),
        &path,
    )
    .unwrap();
    let schema = HermesSchema::detect(&conn).unwrap();
    let mut reader = HermesRowReader::new(&conn, &schema).unwrap();
    let mut frontier = HermesFrontier::initial();
    let mut session_rowids = Vec::new();
    let mut message_rowids = Vec::new();
    let mut reached_eof = false;
    for _ in 0..16 {
        let Some(row) = reader.next(frontier).unwrap() else {
            reached_eof = true;
            break;
        };
        if row.locator.phase == HermesPhase::Sessions {
            session_rowids.push(row.locator.rowid);
        } else {
            message_rowids.push(row.locator.rowid);
        }
        frontier = row.next_frontier;
    }
    assert!(
        reached_eof,
        "minimum rowid must not restart the session scan"
    );
    assert_eq!(session_rowids, vec![i64::MIN]);
    assert_eq!(
        message_rowids
            .iter()
            .filter(|rowid| **rowid == i64::MIN)
            .count(),
        1
    );
    drop(reader);
    drop(conn);
}

#[test]
fn row_reader_scans_sessions_then_messages_and_rejects_before_hydration() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("state.db");
    create_fixture(&path, "reader-session");
    let oversized = i64::try_from(crate::MAX_PROVIDER_SQLITE_VALUE_BYTES)
        .unwrap()
        .saturating_add(1);
    Connection::open(&path)
        .unwrap()
        .execute(
            "INSERT INTO messages (session_id, role, content, timestamp)
             VALUES ('reader-session', 'assistant', zeroblob(?1), 1782259400.0)",
            [oversized],
        )
        .unwrap();

    let conn = crate::provider::sqlite::open_provider_sqlite_readonly(
        crate::test_provider_sqlite_data_root(),
        &path,
    )
    .unwrap();
    let schema = HermesSchema::detect(&conn).unwrap();
    let mut reader = HermesRowReader::new(&conn, &schema).unwrap();
    let mut frontier = HermesFrontier::initial();
    let mut phases = Vec::new();
    let mut rejected = 0;
    while let Some(row) = reader.next(frontier).unwrap() {
        phases.push(row.locator.phase);
        rejected += usize::from(matches!(row.record, HermesNativeRecord::Rejected(_)));
        frontier = row.next_frontier;
    }
    assert_eq!(phases.first(), Some(&HermesPhase::Sessions));
    assert!(phases[1..]
        .iter()
        .all(|phase| *phase == HermesPhase::Messages));
    assert_eq!(rejected, 1);
    assert_eq!(reader.session_hydration_queries, 1);
}

#[test]
fn result_content_uses_only_the_tool_content_column_without_a_size_cap() {
    let long = "x".repeat(crate::PROVIDER_MAX_TEXT_CHARS + 19);
    assert_eq!(
        hermes_normalized_result_content("tool", &Value::String(long.clone())),
        Some(long)
    );
    assert_eq!(
        hermes_normalized_result_content("assistant", &Value::String("not a result".into())),
        None
    );
    assert_eq!(hermes_normalized_result_content("tool", &Value::Null), None);
}
