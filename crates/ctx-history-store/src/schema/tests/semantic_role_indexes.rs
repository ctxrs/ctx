use rusqlite::Connection;

use super::fixtures::tempdir;
use crate::{Store, FINAL_SCHEMA_IDENTITY, SCHEMA_VERSION};

const PARTIAL_PREDICATE: &str = "WHERE event_type = 'message' AND deleted_at_ms IS NULL";
const PARTIAL_INDEXES: [(&str, &[&str]); 3] = [
    (
        "idx_events_live_message_role_occurred_seq",
        &["event_type", "role", "occurred_at_ms", "seq", "id"],
    ),
    (
        "idx_events_live_message_run_role_occurred_seq",
        &[
            "run_id",
            "event_type",
            "role",
            "occurred_at_ms",
            "seq",
            "id",
        ],
    ),
    (
        "idx_events_live_message_session_run_role_occurred_seq",
        &[
            "session_id",
            "run_id",
            "event_type",
            "role",
            "occurred_at_ms",
            "seq",
            "id",
        ],
    ),
];
const LEGACY_INDEXES: [&str; 3] = [
    "idx_events_role_occurred_seq",
    "idx_events_run_role_occurred_seq",
    "idx_events_session_run_role_occurred_seq",
];
type EventRow = (String, i64, String, Option<String>, String, Option<i64>);

fn index_sql(conn: &Connection, name: &str) -> Option<String> {
    conn.query_row(
        "SELECT sql FROM sqlite_schema WHERE type = 'index' AND name = ?1",
        [name],
        |row| row.get(0),
    )
    .ok()
}

fn index_columns(conn: &Connection, index: &str) -> Vec<String> {
    let escaped = index.replace('"', "\"\"");
    let mut stmt = conn
        .prepare(&format!("PRAGMA index_info(\"{escaped}\")"))
        .unwrap();
    stmt.query_map([], |row| row.get(2))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn query_plan(conn: &Connection, sql: &str) -> Vec<String> {
    let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
    stmt.query_map([], |row| row.get(3))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn assert_uses_index(conn: &Connection, index: &str, sql: &str) {
    let plan = query_plan(conn, sql);
    assert!(
        plan.iter().any(|step| step.contains(index)),
        "expected {index} for {sql}; plan: {plan:?}"
    );
    assert!(
        plan.iter().all(|step| !step.contains("USE TEMP B-TREE")),
        "index must preserve requested order for {sql}; plan: {plan:?}"
    );
}

fn assert_partial_indexes(conn: &Connection) {
    for (name, expected_columns) in PARTIAL_INDEXES {
        let sql = index_sql(conn, name).unwrap_or_else(|| panic!("missing {name}"));
        assert!(sql.contains(PARTIAL_PREDICATE), "{name}: {sql}");
        assert_eq!(index_columns(conn, name), expected_columns, "{name}");
    }
    for name in LEGACY_INDEXES {
        assert!(index_sql(conn, name).is_none(), "legacy index {name}");
    }
}

fn install_legacy_full_indexes(conn: &Connection) {
    for (name, _) in PARTIAL_INDEXES {
        conn.execute_batch(&format!("DROP INDEX {name}")).unwrap();
    }
    conn.execute_batch(
        "CREATE INDEX idx_events_role_occurred_seq
           ON events(event_type, role, occurred_at_ms DESC, seq DESC, id DESC);
         CREATE INDEX idx_events_run_role_occurred_seq
           ON events(run_id, event_type, role, occurred_at_ms DESC, seq DESC, id DESC);
         CREATE INDEX idx_events_session_run_role_occurred_seq
           ON events(session_id, run_id, event_type, role, occurred_at_ms DESC, seq DESC, id DESC);",
    )
    .unwrap();
    for name in LEGACY_INDEXES {
        assert!(index_sql(conn, name).is_some(), "missing {name}");
    }
}

fn seed_event_rows(conn: &Connection) {
    conn.execute_batch(
        "INSERT INTO events
         (id, seq, event_type, role, occurred_at_ms, payload_json, deleted_at_ms)
         VALUES
         ('018f45d0-0000-7000-8000-000000091001', 1, 'message', 'user', 10,
          '{\"text\":\"live message\"}', NULL),
         ('018f45d0-0000-7000-8000-000000091002', 2, 'message', 'assistant', 20,
          '{\"text\":\"deleted message\"}', 30),
         ('018f45d0-0000-7000-8000-000000091003', 3, 'tool_call', 'assistant', 30,
          '{\"command\":\"cargo test\"}', NULL);",
    )
    .unwrap();
}

fn event_rows(conn: &Connection) -> Vec<EventRow> {
    let mut stmt = conn
        .prepare(
            "SELECT id, seq, event_type, role, payload_json, deleted_at_ms
             FROM events ORDER BY seq",
        )
        .unwrap();
    stmt.query_map([], |row| {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
        ))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

fn unrelated_event_indexes(conn: &Connection) -> Vec<(String, String)> {
    let mut stmt = conn
        .prepare(
            "SELECT name, sql FROM sqlite_schema
             WHERE type = 'index' AND tbl_name = 'events' AND sql IS NOT NULL
             ORDER BY name",
        )
        .unwrap();
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .filter(|(name, _)| {
            !PARTIAL_INDEXES
                .iter()
                .any(|(partial_name, _)| name == partial_name)
                && !LEGACY_INDEXES.iter().any(|legacy_name| name == legacy_name)
        })
        .collect()
}

fn assert_final_identity(conn: &Connection) {
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    let identity: String = conn
        .query_row(
            "SELECT schema_identity FROM ctx_store_schema_identity WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(identity, FINAL_SCHEMA_IDENTITY);
}

#[test]
fn semantic_role_indexes_are_partial_and_cover_every_ordered_branch() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    assert_partial_indexes(&store.conn);

    assert_uses_index(
        &store.conn,
        PARTIAL_INDEXES[0].0,
        "SELECT id FROM events
         WHERE event_type = 'message' AND role = 'user' AND deleted_at_ms IS NULL
         ORDER BY occurred_at_ms DESC, seq DESC, id DESC LIMIT 1",
    );

    for (role, direction) in [("user", "ASC"), ("assistant", "DESC")] {
        assert_uses_index(
            &store.conn,
            PARTIAL_INDEXES[1].0,
            &format!(
                "SELECT id FROM events
                 WHERE run_id = 'run-1' AND event_type = 'message' AND role = '{role}'
                   AND deleted_at_ms IS NULL
                 ORDER BY occurred_at_ms {direction}, seq {direction}, id {direction} LIMIT 1"
            ),
        );
        assert_uses_index(
            &store.conn,
            PARTIAL_INDEXES[2].0,
            &format!(
                "SELECT id FROM events
                 WHERE session_id = 'session-1' AND run_id IS NULL
                   AND event_type = 'message' AND role = '{role}'
                   AND deleted_at_ms IS NULL
                 ORDER BY occurred_at_ms {direction}, seq {direction}, id {direction} LIMIT 1"
            ),
        );
    }
}

#[test]
fn reopen_creates_partial_indexes_before_removing_legacy_full_shapes() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    {
        let store = Store::open(&path).unwrap();
        install_legacy_full_indexes(&store.conn);
    }

    let reopened = Store::open(&path).unwrap();
    assert_partial_indexes(&reopened.conn);
    drop(reopened);

    let reopened_again = Store::open(&path).unwrap();
    assert_partial_indexes(&reopened_again.conn);
}

#[test]
fn released_v46_reopen_reconciles_legacy_full_role_indexes_without_data_loss() {
    let temp = tempdir();
    let path = temp.path().join("released-v46.sqlite");
    let (expected_rows, expected_unrelated_indexes);
    {
        let store = Store::open(&path).unwrap();
        seed_event_rows(&store.conn);
        expected_rows = event_rows(&store.conn);
        expected_unrelated_indexes = unrelated_event_indexes(&store.conn);
        install_legacy_full_indexes(&store.conn);
        store
            .conn
            .execute_batch(
                "DROP TABLE projection_journal_entities;
                 DROP TABLE projection_journal_chunks;
                 DROP TABLE projection_journal_state;
                 DROP TABLE ctx_store_schema_identity;
                 PRAGMA user_version = 46;",
            )
            .unwrap();
    }

    let reopened = Store::open(&path).unwrap();
    assert_partial_indexes(&reopened.conn);
    assert_eq!(event_rows(&reopened.conn), expected_rows);
    assert_eq!(
        unrelated_event_indexes(&reopened.conn),
        expected_unrelated_indexes
    );
    assert_final_identity(&reopened.conn);
}

#[test]
fn draft_final_v2_reopen_reconciles_legacy_full_role_indexes_without_data_loss() {
    let temp = tempdir();
    let path = temp.path().join("draft-final-v2.sqlite");
    let (expected_rows, expected_unrelated_indexes);
    {
        let store = Store::open(&path).unwrap();
        seed_event_rows(&store.conn);
        expected_rows = event_rows(&store.conn);
        expected_unrelated_indexes = unrelated_event_indexes(&store.conn);
        install_legacy_full_indexes(&store.conn);
        let changed = store
            .conn
            .execute(
                "UPDATE ctx_store_schema_identity
                 SET schema_identity = 'ctx-store-schema-47-final-v2'
                 WHERE singleton = 1 AND schema_version = 47",
                [],
            )
            .unwrap();
        assert_eq!(changed, 1);
    }

    let reopened = Store::open(&path).unwrap();
    assert_partial_indexes(&reopened.conn);
    assert_eq!(event_rows(&reopened.conn), expected_rows);
    assert_eq!(
        unrelated_event_indexes(&reopened.conn),
        expected_unrelated_indexes
    );
    assert_final_identity(&reopened.conn);
}
