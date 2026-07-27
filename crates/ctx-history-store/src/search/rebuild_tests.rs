use ctx_history_core::{HistoryRecord, SessionHistoryArchive};
use rusqlite::Connection;

use super::projections::v47_projection_event_scan_query;
use super::tests::{local_preview_event, tempdir};
use crate::Store;

fn downgrade_store_to_v46(store: &Store) {
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

fn projection_rows_for_event(conn: &Connection, table: &str, event_id: &str) -> i64 {
    conn.query_row(
        &format!("SELECT COUNT(*) FROM {table} WHERE event_id = ?1"),
        [event_id],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn failed_full_rebuild_atomically_preserves_the_previous_search_projection() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open(&db_path).unwrap();
    let event = local_preview_event(1, "atomic-rebuild-old-needle");
    store.upsert_event(&event).unwrap();
    assert_eq!(
        store
            .search_event_hits("atomic-rebuild-old-needle", 10)
            .unwrap()
            .len(),
        1
    );

    // Fail after the rebuild has deleted the prior lookup rows and begun
    // repopulating projections. The transaction must restore every old table.
    store
        .conn
        .execute_batch(
            "CREATE TRIGGER ctx_test_fail_search_rebuild
             BEFORE INSERT ON event_search_lookup
             BEGIN SELECT RAISE(ABORT, 'injected search rebuild failure'); END;",
        )
        .unwrap();
    let error = store.refresh_search_index().unwrap_err();
    assert!(
        error
            .to_string()
            .contains("injected search rebuild failure"),
        "unexpected rebuild error: {error}"
    );
    assert_eq!(
        store
            .search_event_hits("atomic-rebuild-old-needle", 10)
            .unwrap()
            .len(),
        1
    );
    let lookup_rows: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM event_search_lookup", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(lookup_rows, 1);

    drop(store);
    let reopened = Store::open(&db_path).unwrap();
    assert_eq!(
        reopened
            .search_event_hits("atomic-rebuild-old-needle", 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn rebuild_refreshes_cached_capabilities_after_dropping_malformed_event_lookup() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open(&db_path).unwrap();
    store
        .conn
        .execute_batch(
            "DROP TABLE event_search_lookup;
             CREATE TABLE event_search_lookup (event_id TEXT PRIMARY KEY NOT NULL);",
        )
        .unwrap();

    store.refresh_search_index().unwrap();
    let event = local_preview_event(1, "post-rebuild-upsert-needle");
    store.upsert_event(&event).unwrap();

    assert_eq!(store.get_event(event.id).unwrap(), event);
}

#[test]
fn archive_import_and_its_full_search_rebuild_commit_or_rollback_together() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&db_path).unwrap();
    let existing = local_preview_event(1, "archive-atomic-existing-needle");
    let incoming = local_preview_event(2, "archive-atomic-incoming-needle");
    store.upsert_event(&existing).unwrap();
    store
        .conn
        .execute_batch(
            "CREATE TRIGGER ctx_test_fail_archive_search_rebuild
             BEFORE INSERT ON event_search_lookup
             BEGIN SELECT RAISE(ABORT, 'injected archive rebuild failure'); END;",
        )
        .unwrap();
    let archive = SessionHistoryArchive {
        schema_version: 2,
        version: 2,
        events: vec![incoming.clone()],
        ..SessionHistoryArchive::default()
    };

    let error = store.import_archive(&archive, false).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("injected archive rebuild failure"),
        "unexpected import error: {error}"
    );
    assert!(store.get_event(incoming.id).is_err());
    assert_eq!(
        store
            .search_event_hits("archive-atomic-existing-needle", 10)
            .unwrap()
            .len(),
        1
    );
    assert!(store
        .search_event_hits("archive-atomic-incoming-needle", 10)
        .unwrap()
        .is_empty());
}

#[test]
fn schema_46_upgrade_forces_one_clean_rebuild_of_a_partial_projection() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open(&db_path).unwrap();
    let retained = local_preview_event(1, "upgrade-retained-projection-needle");
    let missing = local_preview_event(2, "upgrade-missing-projection-needle");
    store.upsert_event(&retained).unwrap();
    store.upsert_event(&missing).unwrap();
    assert_eq!(
        store
            .search_event_hits("upgrade-missing-projection-needle", 10)
            .unwrap()
            .len(),
        1
    );

    for table in [
        "event_search",
        "event_search_scriptgram",
        "event_search_lookup",
    ] {
        store
            .conn
            .execute(
                &format!("DELETE FROM {table} WHERE event_id = ?1"),
                [missing.id.to_string()],
            )
            .unwrap();
    }
    // The retained event makes this look initialized to the legacy aggregate
    // nonzero check even though one canonical event is now unsearchable.
    assert_eq!(
        store
            .search_event_hits("upgrade-retained-projection-needle", 10)
            .unwrap()
            .len(),
        1
    );
    assert!(store
        .search_event_hits("upgrade-missing-projection-needle", 10)
        .unwrap()
        .is_empty());
    downgrade_store_to_v46(&store);
    drop(store);

    let upgraded = Store::open(&db_path).unwrap();
    assert_eq!(
        upgraded
            .search_event_hits("upgrade-retained-projection-needle", 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        upgraded
            .search_event_hits("upgrade-missing-projection-needle", 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn schema_46_upgrade_recreates_missing_main_and_scriptgram_fts_tables() {
    for (table, text) in [
        ("event_search", "missing-main-projection-needle"),
        ("event_search_scriptgram", "欠落スクリプトグラム検索"),
    ] {
        let temp = tempdir();
        let db_path = temp.path().join(format!("{table}.sqlite"));
        let store = Store::open(&db_path).unwrap();
        let event = local_preview_event(1, text);
        store.upsert_event(&event).unwrap();
        store
            .conn
            .execute(&format!("DROP TABLE {table}"), [])
            .unwrap();
        downgrade_store_to_v46(&store);
        drop(store);

        let upgraded = Store::open(&db_path).unwrap();
        let create_sql: String = upgraded
            .conn
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = ?1",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            create_sql
                .to_ascii_lowercase()
                .starts_with("create virtual table"),
            "{table} was not recreated as FTS5: {create_sql}"
        );
        assert_eq!(
            upgraded.search_event_hits(text, 10).unwrap().len(),
            1,
            "{table} did not receive its canonical event row"
        );
        if table == "event_search_scriptgram" {
            assert_eq!(
                projection_rows_for_event(
                    &upgraded.conn,
                    "event_search_scriptgram",
                    &event.id.to_string()
                ),
                1
            );
        }
    }
}

#[test]
fn schema_46_upgrade_fails_closed_on_an_ordinary_table_masquerading_as_fts5() {
    let temp = tempdir();
    let db_path = temp.path().join("ordinary-masquerade.sqlite");
    let store = Store::open(&db_path).unwrap();
    store
        .conn
        .execute_batch(
            "DROP TABLE event_search;
         CREATE TABLE event_search (
             event_id TEXT,
             history_record_id TEXT,
             session_id TEXT,
             role TEXT,
             preview_text TEXT,
             rank_bucket TEXT
         );",
        )
        .unwrap();
    downgrade_store_to_v46(&store);
    drop(store);

    let error = match Store::open(&db_path) {
        Ok(_) => panic!("ordinary event_search table was accepted as FTS5"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("malformed v47 search projection table event_search"),
        "unexpected migration error: {error}"
    );
    let conn = Connection::open(&db_path).unwrap();
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        46
    );
    let create_sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name = 'event_search'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(create_sql.to_ascii_lowercase().starts_with("create table"));
}

#[test]
fn schema_46_upgrade_fails_closed_on_a_noncanonical_fts5_shape() {
    let temp = tempdir();
    let db_path = temp.path().join("malformed-fts.sqlite");
    let store = Store::open(&db_path).unwrap();
    store
        .conn
        .execute_batch(
            "DROP TABLE event_search;
             CREATE VIRTUAL TABLE event_search USING fts5(
                 event_id,
                 history_record_id,
                 session_id,
                 role,
                 preview_text,
                 rank_bucket
             );",
        )
        .unwrap();
    downgrade_store_to_v46(&store);
    drop(store);

    let error = match Store::open(&db_path) {
        Ok(_) => panic!("noncanonical event_search FTS5 shape was accepted"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("malformed v47 search projection table event_search"),
        "unexpected migration error: {error}"
    );
    let conn = Connection::open(&db_path).unwrap();
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        46
    );
}

#[test]
fn schema_46_upgrade_rebuilds_stale_metadata_content_extra_and_ineligible_rows() {
    let temp = tempdir();
    let db_path = temp.path().join("noncanonical-projection.sqlite");
    let store = Store::open(&db_path).unwrap();
    let event = local_preview_event(1, "正規イベント本文");
    let ineligible = local_preview_event(2, "削除済みイベント本文");
    store.upsert_event(&event).unwrap();
    store.upsert_event(&ineligible).unwrap();
    let record = HistoryRecord::new(
        "正規レコード題名",
        "canonical record body",
        vec!["canonical-tag".to_owned()],
        "task",
        None,
    );
    store.upsert_record(&record).unwrap();

    store
        .conn
        .execute(
            "DELETE FROM ctx_history_search WHERE record_id = ?1",
            [record.id.to_string()],
        )
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO ctx_history_search
                 (record_id, title, summary, primary_user_text,
                  decision_text, context_text, tag_text)
             VALUES (?1, 'stale title', 'stale summary', 'stale primary',
                     'stale decision', 'stale context', 'stale tags')",
            [record.id.to_string()],
        )
        .unwrap();
    store
        .conn
        .execute(
            "DELETE FROM ctx_history_search_scriptgram WHERE record_id = ?1",
            [record.id.to_string()],
        )
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO ctx_history_search_scriptgram (record_id, token_text)
             VALUES (?1, 'stale-scriptgram')",
            [record.id.to_string()],
        )
        .unwrap();

    store
        .conn
        .execute(
            "DELETE FROM event_search WHERE event_id = ?1",
            [event.id.to_string()],
        )
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO event_search
                 (event_id, history_record_id, session_id, role, preview_text, rank_bucket)
             VALUES (?1, 'stale-history', 'stale-session', 'assistant',
                     'stale event text', 'summary')",
            [event.id.to_string()],
        )
        .unwrap();
    store
        .conn
        .execute(
            "UPDATE event_search_lookup
             SET role = 'assistant', preview_text = 'stale lookup text',
                 rank_bucket = 'summary'
             WHERE event_id = ?1",
            [event.id.to_string()],
        )
        .unwrap();
    store
        .conn
        .execute(
            "DELETE FROM event_search_scriptgram WHERE event_id = ?1",
            [event.id.to_string()],
        )
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO event_search_scriptgram
                 (event_id, history_record_id, session_id, role, token_text, rank_bucket)
             VALUES (?1, NULL, NULL, 'assistant', 'stale-scriptgram', 'summary')",
            [event.id.to_string()],
        )
        .unwrap();

    store
        .conn
        .execute(
            "UPDATE events SET deleted_at_ms = 1 WHERE id = ?1",
            [ineligible.id.to_string()],
        )
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO event_search
                 (event_id, role, preview_text, rank_bucket)
             VALUES ('extra-event-id', 'user', 'extra projection', 'message')",
            [],
        )
        .unwrap();
    downgrade_store_to_v46(&store);
    drop(store);

    let upgraded = Store::open(&db_path).unwrap();
    let actual_record: (String, String, String, String, String, String) = upgraded
        .conn
        .query_row(
            "SELECT title, summary, primary_user_text,
                    decision_text, context_text, tag_text
             FROM ctx_history_search WHERE record_id = ?1",
            [record.id.to_string()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        actual_record,
        (
            "正規レコード題名".to_owned(),
            "canonical record body".to_owned(),
            "canonical record body".to_owned(),
            String::new(),
            String::new(),
            "canonical-tag".to_owned(),
        )
    );
    let actual_event: (
        Option<String>,
        Option<String>,
        Option<String>,
        String,
        String,
    ) = upgraded
        .conn
        .query_row(
            "SELECT history_record_id, session_id, role, preview_text, rank_bucket
             FROM event_search WHERE event_id = ?1",
            [event.id.to_string()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        actual_event,
        (
            None,
            None,
            Some("user".to_owned()),
            "正規イベント本文".to_owned(),
            "message".to_owned(),
        )
    );
    let event_scriptgram: (String, String, String) = upgraded
        .conn
        .query_row(
            "SELECT token_text, role, rank_bucket
             FROM event_search_scriptgram WHERE event_id = ?1",
            [event.id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_ne!(event_scriptgram.0, "stale-scriptgram");
    assert_eq!(event_scriptgram.1, "user");
    assert_eq!(event_scriptgram.2, "message");
    for table in [
        "event_search",
        "event_search_scriptgram",
        "event_search_lookup",
    ] {
        assert_eq!(
            projection_rows_for_event(&upgraded.conn, table, &ineligible.id.to_string()),
            0,
            "ineligible row remained in {table}"
        );
    }
    assert_eq!(
        projection_rows_for_event(&upgraded.conn, "event_search", "extra-event-id"),
        0
    );
}

#[test]
fn failed_v47_projection_repair_rolls_back_schema_and_all_projection_tables() {
    let temp = tempdir();
    let db_path = temp.path().join("migration-rollback.sqlite");
    let store = Store::open(&db_path).unwrap();
    let event = local_preview_event(1, "rollback-canonical-text");
    store.upsert_event(&event).unwrap();
    store
        .conn
        .execute(
            "UPDATE event_search SET preview_text = 'rollback-stale-text'
             WHERE event_id = ?1",
            [event.id.to_string()],
        )
        .unwrap();
    store
        .conn
        .execute_batch(
            "CREATE TRIGGER ctx_test_fail_v47_projection_repair
             BEFORE INSERT ON event_search_lookup
             BEGIN SELECT RAISE(ABORT, 'injected v47 projection repair failure'); END;",
        )
        .unwrap();
    downgrade_store_to_v46(&store);
    drop(store);

    let error = match Store::open(&db_path) {
        Ok(_) => panic!("injected migration failure did not abort Store::open"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("injected v47 projection repair failure"),
        "unexpected migration error: {error}"
    );

    let conn = Connection::open(&db_path).unwrap();
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        46
    );
    assert_eq!(
        conn.query_row(
            "SELECT preview_text FROM event_search WHERE event_id = ?1",
            [event.id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .unwrap(),
        "rollback-stale-text"
    );
    assert_eq!(
        projection_rows_for_event(&conn, "event_search_lookup", &event.id.to_string()),
        1
    );
    for table in [
        "ctx_store_schema_identity",
        "ctx_v47_projection_equivalence_scratch",
    ] {
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name = ?1",
                [table],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            0,
            "rolled-back migration left {table}"
        );
    }
}

#[test]
fn exact_large_v46_projection_uses_an_unsorted_bounded_scan_and_avoids_rebuild() {
    const EVENT_COUNT: i64 = 4096;

    let temp = tempdir();
    let db_path = temp.path().join("large-exact-projection.sqlite");
    let store = Store::open(&db_path).unwrap();
    store
        .conn
        .execute_batch(
            "WITH RECURSIVE n(i) AS (
                 VALUES(1)
                 UNION ALL
                 SELECT i + 1 FROM n WHERE i < 4096
             )
             INSERT INTO events
                 (id, seq, event_type, role, occurred_at_ms, payload_json)
             SELECT printf('00000000-0000-4000-8000-%012d', i),
                    i, 'message', 'user', i,
                    json_object('text', 'large-store-' || i)
             FROM n;

             INSERT INTO event_search
                 (event_id, history_record_id, session_id, role, preview_text, rank_bucket)
             SELECT id, NULL, NULL, role, json_extract(payload_json, '$.text'), event_type
             FROM events;

             INSERT INTO event_search_lookup
                 (event_id, history_record_id, session_id, role, preview_text, rank_bucket)
             SELECT id, NULL, NULL, role, json_extract(payload_json, '$.text'), event_type
             FROM events;",
        )
        .unwrap();
    let explain_sql = format!("EXPLAIN QUERY PLAN {}", v47_projection_event_scan_query());
    let mut explain = store.conn.prepare(&explain_sql).unwrap();
    let details = explain
        .query_map([], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(
        details
            .iter()
            .all(|detail| !detail.to_ascii_uppercase().contains("TEMP B-TREE")),
        "v47 canonical scan unexpectedly sorts globally: {details:?}"
    );
    drop(explain);

    store
        .conn
        .execute_batch(
            "CREATE TRIGGER ctx_test_reject_large_projection_rebuild
             BEFORE DELETE ON event_search_lookup
             BEGIN SELECT RAISE(ABORT, 'large exact projection was rebuilt'); END;",
        )
        .unwrap();
    downgrade_store_to_v46(&store);
    drop(store);

    let upgraded = Store::open(&db_path).unwrap();
    assert_eq!(
        upgraded
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        EVENT_COUNT
    );
    assert_eq!(
        upgraded
            .conn
            .query_row("SELECT COUNT(*) FROM event_search_lookup", [], |row| row
                .get::<_, i64>(0),)
            .unwrap(),
        EVENT_COUNT
    );
    assert_eq!(
        upgraded
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_temp_schema
                 WHERE name = 'ctx_v47_projection_equivalence_scratch'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    assert_eq!(
        upgraded
            .conn
            .query_row("PRAGMA temp_store", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        2,
        "the disk-backed migration scratch mode was not restored"
    );
}
