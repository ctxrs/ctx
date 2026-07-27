use ctx_history_core::new_id;
use rusqlite::{params, Connection, ErrorCode};

use super::fixtures::tempdir;
use crate::{Store, FINAL_SCHEMA_IDENTITY};

const EXPLICIT_INDEX: &str = "idx_events_seq";

fn explicit_index_exists(conn: &Connection) -> bool {
    conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM sqlite_schema
             WHERE type = 'index' AND name = ?1
         )",
        [EXPLICIT_INDEX],
        |row| row.get(0),
    )
    .unwrap()
}

fn event_indexes(conn: &Connection) -> Vec<(String, bool, String)> {
    let mut stmt = conn.prepare("PRAGMA index_list('events')").unwrap();
    let mut indexes = stmt
        .query_map([], |row| {
            Ok((row.get(1)?, row.get::<_, i64>(2)? != 0, row.get(3)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    indexes.sort();
    indexes
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
    let mut stmt = conn.prepare(sql).unwrap();
    stmt.query_map([], |row| row.get(3))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn assert_only_unique_autoindex_owns_seq(conn: &Connection) {
    assert!(!explicit_index_exists(conn));
    let seq_indexes = event_indexes(conn)
        .into_iter()
        .filter(|(name, unique, origin)| {
            *unique && origin == "u" && index_columns(conn, name) == ["seq"]
        })
        .collect::<Vec<_>>();
    assert_eq!(seq_indexes.len(), 1, "events.seq UNIQUE autoindex");
    assert!(seq_indexes[0].0.starts_with("sqlite_autoindex_events_"));
}

#[test]
fn events_seq_unique_autoindex_preserves_equality_order_and_uniqueness() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    assert_only_unique_autoindex_owns_seq(&store.conn);

    for seq in [30_i64, 10, 20] {
        store
            .conn
            .execute(
                "INSERT INTO events (id, seq, event_type, occurred_at_ms)
                 VALUES (?1, ?2, 'notice', ?2)",
                params![new_id().to_string(), seq],
            )
            .unwrap();
    }

    let equality_plan = query_plan(
        &store.conn,
        "EXPLAIN QUERY PLAN SELECT id FROM events WHERE seq = 20",
    );
    assert!(
        equality_plan
            .iter()
            .any(|step| step.contains("sqlite_autoindex_events_") && step.contains("seq=?")),
        "equality plan: {equality_plan:?}"
    );

    for direction in ["ASC", "DESC"] {
        let plan = query_plan(
            &store.conn,
            &format!("EXPLAIN QUERY PLAN SELECT seq FROM events ORDER BY seq {direction}"),
        );
        assert!(
            plan.iter()
                .any(|step| step.contains("sqlite_autoindex_events_")),
            "{direction} plan: {plan:?}"
        );
        assert!(
            plan.iter().all(|step| !step.contains("USE TEMP B-TREE")),
            "{direction} plan: {plan:?}"
        );
    }

    let ordered = |direction: &str| {
        let mut stmt = store
            .conn
            .prepare(&format!("SELECT seq FROM events ORDER BY seq {direction}"))
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<i64>>()
    };
    assert_eq!(ordered("ASC"), [10, 20, 30]);
    assert_eq!(ordered("DESC"), [30, 20, 10]);

    let duplicate = store
        .conn
        .execute(
            "INSERT INTO events (id, seq, event_type, occurred_at_ms)
             VALUES (?1, 20, 'notice', 40)",
            [new_id().to_string()],
        )
        .unwrap_err();
    assert!(matches!(
        duplicate,
        rusqlite::Error::SqliteFailure(ref failure, _)
            if failure.code == ErrorCode::ConstraintViolation
    ));
}

#[test]
fn draft_final_v2_v47_reopen_drops_only_the_redundant_events_seq_index() {
    let temp = tempdir();
    let path = temp.path().join("v47-final-v2.sqlite");
    let expected_indexes;
    {
        let store = Store::open(&path).unwrap();
        expected_indexes = event_indexes(&store.conn);
        store
            .conn
            .execute_batch("CREATE INDEX idx_events_seq ON events(seq)")
            .unwrap();
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
        assert!(explicit_index_exists(&store.conn));
    }

    let store = Store::open(&path).unwrap();
    assert_only_unique_autoindex_owns_seq(&store.conn);
    assert_eq!(event_indexes(&store.conn), expected_indexes);
    let identity: String = store
        .conn
        .query_row(
            "SELECT schema_identity FROM ctx_store_schema_identity WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(identity, FINAL_SCHEMA_IDENTITY);
}

#[test]
fn released_v46_and_final_v47_drop_only_the_redundant_events_seq_index() {
    for released_version in [46_i64, 47] {
        let temp = tempdir();
        let path = temp.path().join(format!("v{released_version}.sqlite"));
        let expected_indexes;
        {
            let store = Store::open(&path).unwrap();
            expected_indexes = event_indexes(&store.conn);
            store
                .conn
                .execute_batch("CREATE INDEX idx_events_seq ON events(seq)")
                .unwrap();
            assert!(explicit_index_exists(&store.conn));
            if released_version == 46 {
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
        }

        let store = Store::open(&path).unwrap();
        assert_only_unique_autoindex_owns_seq(&store.conn);
        assert_eq!(event_indexes(&store.conn), expected_indexes);
        let identity: String = store
            .conn
            .query_row(
                "SELECT schema_identity FROM ctx_store_schema_identity WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(identity, FINAL_SCHEMA_IDENTITY);
    }
}
