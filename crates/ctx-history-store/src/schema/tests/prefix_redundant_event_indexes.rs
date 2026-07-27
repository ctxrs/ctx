//! `events(history_record_id)` and `events(session_id)` are strict left
//! prefixes of the composite indexes that also carry `occurred_at_ms`. These
//! tests pin the empirical claim the prune rests on: every lookup the narrow
//! indexes served is still served by the wider index with the same access
//! shape, and no reopen path can leave a Store without an access path.
//!
//! The prune carries no schema identity of its own. Like `idx_events_seq` and
//! the legacy role indexes before it, it is reconciled by `INDEXES_SQL` on
//! every writable open, which makes it safe in both directions: an older
//! binary recreates the two indexes, a newer one drops them again, and no
//! command ever fails on an identity it does not recognise.

use rusqlite::Connection;

use super::fixtures::tempdir;
use crate::{Store, FINAL_SCHEMA_IDENTITY, SCHEMA_VERSION};

const PRUNED_INDEXES: [&str; 2] = ["idx_events_history_record_id", "idx_events_session_id"];
const COVERING_COMPOSITES: [(&str, &[&str]); 2] = [
    (
        "idx_events_history_record_occurred_at_ms",
        &["history_record_id", "occurred_at_ms"],
    ),
    (
        "idx_events_session_occurred_at_ms",
        &["session_id", "occurred_at_ms"],
    ),
];
/// Every production lookup that reached a pruned index, paired with the
/// composite that must absorb it.
const ABSORBED_LOOKUPS: [(&str, &str); 6] = [
    (
        "idx_events_history_record_occurred_at_ms",
        "SELECT 1 FROM events WHERE history_record_id = 'record-1'",
    ),
    (
        "idx_events_history_record_occurred_at_ms",
        "SELECT COALESCE(MAX(event_count), 0) FROM (
             SELECT COUNT(*) AS event_count FROM events GROUP BY history_record_id
         )",
    ),
    (
        "idx_events_history_record_occurred_at_ms",
        "SELECT id FROM events WHERE history_record_id = 'record-1' ORDER BY occurred_at_ms",
    ),
    (
        "idx_events_session_occurred_at_ms",
        "SELECT COUNT(*) FROM events WHERE session_id = 'session-1'",
    ),
    (
        "idx_events_session_occurred_at_ms",
        "SELECT id FROM events WHERE session_id = 'session-1' ORDER BY occurred_at_ms",
    ),
    (
        "idx_events_session_occurred_at_ms",
        "SELECT ctx_event_id FROM ctx_events WHERE ctx_session_id = 'session-1'",
    ),
];
type EventRow = (String, i64, String, Option<String>, Option<String>);

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

fn assert_pruned_index_set(conn: &Connection) {
    for name in PRUNED_INDEXES {
        assert!(
            index_sql(conn, name).is_none(),
            "prefix-redundant index {name} is still present"
        );
    }
    for (name, expected_columns) in COVERING_COMPOSITES {
        assert!(index_sql(conn, name).is_some(), "missing {name}");
        assert_eq!(index_columns(conn, name), expected_columns, "{name}");
    }
}

fn other_event_indexes(conn: &Connection) -> Vec<(String, String)> {
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
        .filter(|(name, _)| !PRUNED_INDEXES.iter().any(|pruned| name == pruned))
        .collect()
}

fn install_prefix_redundant_indexes(conn: &Connection) {
    conn.execute_batch(
        "CREATE INDEX idx_events_history_record_id ON events(history_record_id);
         CREATE INDEX idx_events_session_id ON events(session_id);",
    )
    .unwrap();
    for name in PRUNED_INDEXES {
        assert!(index_sql(conn, name).is_some(), "missing {name}");
    }
}

fn seed_event_rows(conn: &Connection) {
    conn.execute_batch(
        "INSERT INTO events
         (id, seq, event_type, role, occurred_at_ms, payload_json)
         VALUES
         ('018f45d0-0000-7000-8000-0000000a1001', 1, 'message', 'user', 10,
          '{\"text\":\"first\"}'),
         ('018f45d0-0000-7000-8000-0000000a1002', 2, 'message', 'assistant', 20,
          '{\"text\":\"second\"}'),
         ('018f45d0-0000-7000-8000-0000000a1003', 3, 'tool_call', 'assistant', 30,
          '{\"command\":\"cargo test\"}');",
    )
    .unwrap();
}

fn event_rows(conn: &Connection) -> Vec<EventRow> {
    let mut stmt = conn
        .prepare(
            "SELECT id, seq, event_type, history_record_id, session_id
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
        ))
    })
    .unwrap()
    .map(Result::unwrap)
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

fn set_identity(conn: &Connection, identity: &str) {
    let changed = conn
        .execute(
            "UPDATE ctx_store_schema_identity SET schema_identity = ?1
             WHERE singleton = 1 AND schema_version = 47",
            [identity],
        )
        .unwrap();
    assert_eq!(changed, 1);
}

#[test]
fn composite_indexes_absorb_every_prefix_lookup_with_the_same_access_shape() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    assert_pruned_index_set(&store.conn);

    for (index, sql) in ABSORBED_LOOKUPS {
        let plan = query_plan(&store.conn, sql);
        assert!(
            plan.iter().any(|step| step.contains(index)),
            "expected {index} for {sql}; plan: {plan:?}"
        );
        assert!(
            plan.iter()
                .all(|step| !step.starts_with("SCAN events") || step.contains("USING")),
            "prefix lookup fell back to a table scan for {sql}; plan: {plan:?}"
        );
    }
}

#[test]
fn released_store_reopen_drops_prefix_indexes_without_data_loss() {
    let temp = tempdir();
    let path = temp.path().join("released.sqlite");
    let (expected_rows, expected_other_indexes);
    {
        let store = Store::open(&path).unwrap();
        seed_event_rows(&store.conn);
        expected_rows = event_rows(&store.conn);
        expected_other_indexes = other_event_indexes(&store.conn);
        install_prefix_redundant_indexes(&store.conn);
    }

    let reopened = Store::open(&path).unwrap();
    assert_pruned_index_set(&reopened.conn);
    assert_eq!(event_rows(&reopened.conn), expected_rows);
    assert_eq!(other_event_indexes(&reopened.conn), expected_other_indexes);
    assert_final_identity(&reopened.conn);
}

#[test]
fn an_older_binary_recreating_the_indexes_is_reconciled_on_the_next_open() {
    // The no-identity design has to survive a mixed-version window: an older
    // binary's INDEXES_SQL recreates both indexes on its own writable open,
    // and the next open by this binary must drop them again, every time,
    // without touching anything else.
    let temp = tempdir();
    let path = temp.path().join("mixed-version.sqlite");
    let expected_rows;
    {
        let store = Store::open(&path).unwrap();
        seed_event_rows(&store.conn);
        expected_rows = event_rows(&store.conn);
    }

    for _ in 0..3 {
        {
            let store = Store::open(&path).unwrap();
            install_prefix_redundant_indexes(&store.conn);
        }
        let reopened = Store::open(&path).unwrap();
        assert_pruned_index_set(&reopened.conn);
        assert_eq!(event_rows(&reopened.conn), expected_rows);
        assert_final_identity(&reopened.conn);
    }
}

#[test]
fn released_v46_upgrade_lands_on_the_pruned_index_set() {
    let temp = tempdir();
    let path = temp.path().join("released-v46.sqlite");
    let expected_rows;
    {
        let store = Store::open(&path).unwrap();
        seed_event_rows(&store.conn);
        expected_rows = event_rows(&store.conn);
        install_prefix_redundant_indexes(&store.conn);
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
    assert_pruned_index_set(&reopened.conn);
    assert_eq!(event_rows(&reopened.conn), expected_rows);
    assert_final_identity(&reopened.conn);
}

#[test]
fn every_same_version_identity_upgrade_lands_on_the_pruned_index_set() {
    // The prune is not gated on any identity, so every entry point has to
    // converge through the reopen-time INDEXES_SQL reconciliation alone. This
    // walks the whole same-version identity chain to pin that it does.
    for identity in [
        "ctx-store-schema-47-final-v2",
        "ctx-store-schema-47-final-v3",
        "ctx-store-schema-47-final-v4",
        "ctx-store-schema-47-final-v5",
        "ctx-store-schema-47-final-v6",
        "ctx-store-schema-47-final-v7",
    ] {
        let temp = tempdir();
        let path = temp.path().join("identity.sqlite");
        let expected_rows;
        {
            let store = Store::open(&path).unwrap();
            seed_event_rows(&store.conn);
            expected_rows = event_rows(&store.conn);
            install_prefix_redundant_indexes(&store.conn);
            set_identity(&store.conn, identity);
        }

        let reopened = Store::open(&path).unwrap();
        assert_pruned_index_set(&reopened.conn);
        assert_eq!(event_rows(&reopened.conn), expected_rows, "{identity}");
        assert_final_identity(&reopened.conn);
    }
}

#[test]
fn prefix_prune_is_reversible_by_recreating_the_narrow_indexes() {
    let temp = tempdir();
    let path = temp.path().join("rollback.sqlite");
    let store = Store::open(&path).unwrap();
    seed_event_rows(&store.conn);
    let expected_rows = event_rows(&store.conn);
    assert_pruned_index_set(&store.conn);

    // Rolling a Store back is two CREATE INDEX statements and nothing else:
    // the prune removes no row and no column, and it carries no identity, so
    // an older binary opens the rolled-back store without complaint.
    install_prefix_redundant_indexes(&store.conn);
    assert_eq!(event_rows(&store.conn), expected_rows);
    assert_final_identity(&store.conn);
    for name in PRUNED_INDEXES {
        assert!(index_sql(&store.conn, name).is_some(), "missing {name}");
    }
}
