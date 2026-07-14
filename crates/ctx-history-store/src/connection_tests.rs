use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};

use crate::{connection::is_recoverable_bulk_maintenance_error, Store, StoreError};

fn tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("ctx-history-store-connection-")
        .tempdir()
        .unwrap()
}

fn fts_config(store: &Store, table: &str, key: &str, default: i64) -> i64 {
    let sql = format!("SELECT v FROM {table}_config WHERE k = ?1");
    store
        .conn
        .query_row(&sql, params![key], |row| row.get(0))
        .optional()
        .unwrap()
        .unwrap_or(default)
}

fn set_fts_config(store: &Store, table: &str, key: &str, value: i64) {
    let sql = format!("INSERT INTO {table}({table}, rank) VALUES (?1, ?2)");
    store.conn.execute(&sql, params![key, value]).unwrap();
}

fn bulk_mode_marker(store: &Store) -> Option<i64> {
    store
        .conn
        .query_row(
            "SELECT value FROM search_projection_stats WHERE key = 'event_search_bulk_mode_v1'",
            [],
            |row| row.get(0),
        )
        .optional()
        .unwrap()
}

#[test]
fn strict_truncating_checkpoint_reports_pinned_reader() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open_with_busy_timeout(&db_path, Duration::from_millis(10)).unwrap();
    store
        .conn
        .execute_batch("CREATE TABLE checkpoint_probe(value INTEGER); INSERT INTO checkpoint_probe VALUES (1);")
        .unwrap();

    let reader = Connection::open(&db_path).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    let count = reader
        .query_row("SELECT COUNT(*) FROM checkpoint_probe", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(count, 1);

    store
        .conn
        .execute("INSERT INTO checkpoint_probe VALUES (2)", [])
        .unwrap();
    let error = store.checkpoint_wal_truncate_required().unwrap_err();
    assert!(matches!(
        error,
        StoreError::WalCheckpointBusy {
            log_frames,
            checkpointed_frames,
        } if log_frames > checkpointed_frames
    ));

    reader.execute_batch("ROLLBACK").unwrap();
    store.checkpoint_wal_truncate_required().unwrap();
}

#[test]
fn bulk_recovery_only_suppresses_temporary_pressure() {
    assert!(is_recoverable_bulk_maintenance_error(
        &StoreError::WalCheckpointBusy {
            log_frames: 2,
            checkpointed_frames: 1,
        }
    ));
    assert!(is_recoverable_bulk_maintenance_error(&StoreError::Sql(
        rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL), None,)
    )));
    assert!(!is_recoverable_bulk_maintenance_error(&StoreError::Sql(
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
            None,
        )
    )));
}

#[test]
fn bulk_search_mode_recovers_on_reopen_and_restores_saved_config() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open(&db_path).unwrap();
    for table in ["event_search", "event_search_scriptgram"] {
        set_fts_config(&store, table, "automerge", 8);
        set_fts_config(&store, table, "crisismerge", 32);
    }

    let guard = store.begin_event_search_bulk_mode().unwrap();
    assert_eq!(bulk_mode_marker(&store), Some(1));
    for table in ["event_search", "event_search_scriptgram"] {
        assert_eq!(fts_config(&store, table, "automerge", 4), 0);
        assert_eq!(fts_config(&store, table, "crisismerge", 16), 1_000_000);
    }
    store.defer_event_search_bulk_mode(&guard).unwrap();
    assert_eq!(bulk_mode_marker(&store), None);
    for table in ["event_search", "event_search_scriptgram"] {
        assert_eq!(fts_config(&store, table, "automerge", 4), 8);
        assert_eq!(fts_config(&store, table, "crisismerge", 16), 32);
    }
    drop(store);
    drop(guard);

    let reopened = Store::open(&db_path).unwrap();
    assert_eq!(bulk_mode_marker(&reopened), None);
    for table in ["event_search", "event_search_scriptgram"] {
        assert_eq!(fts_config(&reopened, table, "automerge", 4), 8);
        assert_eq!(fts_config(&reopened, table, "crisismerge", 16), 32);
    }
}

#[test]
fn bulk_search_recovery_without_marker_preserves_custom_config() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open(&db_path).unwrap();
    for table in ["event_search", "event_search_scriptgram"] {
        set_fts_config(&store, table, "automerge", 8);
        set_fts_config(&store, table, "crisismerge", 32);
    }

    store.recover_event_search_bulk_mode().unwrap();

    assert_eq!(bulk_mode_marker(&store), None);
    for table in ["event_search", "event_search_scriptgram"] {
        assert_eq!(fts_config(&store, table, "automerge", 4), 8);
        assert_eq!(fts_config(&store, table, "crisismerge", 16), 32);
    }
}

#[test]
fn overlapping_bulk_search_mode_is_rejected_until_guard_releases() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let first = Store::open(&db_path).unwrap();
    let guard = first.begin_event_search_bulk_mode().unwrap();
    let second = Store::open_with_busy_timeout(&db_path, Duration::from_millis(10)).unwrap();

    let error = second.begin_event_search_bulk_mode().err().unwrap();
    assert!(matches!(error, StoreError::BulkSearchImportBusy));
    assert_eq!(bulk_mode_marker(&second), Some(1));
    for table in ["event_search", "event_search_scriptgram"] {
        assert_eq!(fts_config(&second, table, "automerge", 4), 0);
        assert_eq!(fts_config(&second, table, "crisismerge", 16), 1_000_000);
    }

    first.finish_event_search_bulk_mode(&guard).unwrap();
    drop(guard);
    let next_guard = second.begin_event_search_bulk_mode().unwrap();
    second.finish_event_search_bulk_mode(&next_guard).unwrap();
}

#[test]
fn nested_bulk_search_mode_finishes_only_at_outer_scope() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let first = Store::open(&db_path).unwrap();
    let outer = first.begin_event_search_bulk_mode().unwrap();
    let nested = first.begin_event_search_bulk_mode().unwrap();
    let second = Store::open_with_busy_timeout(&db_path, Duration::from_millis(10)).unwrap();

    first.finish_event_search_bulk_mode(&nested).unwrap();
    assert_eq!(bulk_mode_marker(&first), Some(1));
    let error = first.finish_event_search_bulk_mode(&outer).unwrap_err();
    assert!(matches!(error, StoreError::InvalidBulkSearchGuard));
    assert!(matches!(
        second.begin_event_search_bulk_mode().err().unwrap(),
        StoreError::BulkSearchImportBusy
    ));

    drop(nested);
    first.finish_event_search_bulk_mode(&outer).unwrap();
    assert_eq!(bulk_mode_marker(&first), None);
    for table in ["event_search", "event_search_scriptgram"] {
        assert_eq!(fts_config(&first, table, "automerge", 4), 4);
        assert_eq!(fts_config(&first, table, "crisismerge", 16), 16);
    }
    drop(outer);

    let fresh = second.begin_event_search_bulk_mode().unwrap();
    second.finish_event_search_bulk_mode(&fresh).unwrap();
}

#[test]
fn optimize_serializes_with_bulk_guard_even_without_visible_marker() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let first = Store::open(&db_path).unwrap();
    let guard = first.begin_event_search_bulk_mode().unwrap();
    first
        .conn
        .execute(
            "DELETE FROM search_projection_stats WHERE key = ?1 OR key LIKE ?2",
            params!["event_search_bulk_mode_v1", "event_search_bulk_mode_v1:%"],
        )
        .unwrap();
    for table in ["event_search", "event_search_scriptgram"] {
        set_fts_config(&first, table, "automerge", 4);
        set_fts_config(&first, table, "crisismerge", 16);
    }
    let second = Store::open_with_busy_timeout(&db_path, Duration::from_millis(10)).unwrap();

    let error = second.optimize_search_index().unwrap_err();
    assert!(matches!(error, StoreError::BulkSearchImportBusy));

    drop(guard);
    second.optimize_search_index().unwrap();
}

#[test]
fn bulk_search_mode_crosses_crisis_threshold_without_automatic_merge() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open(&db_path).unwrap();
    let guard = store.begin_event_search_bulk_mode().unwrap();

    let mut peak_wal_bytes = 0;
    for index in 0..20 {
        store
            .conn
            .execute(
                r#"
                INSERT INTO event_search
                (event_id, history_record_id, session_id, role, preview_text, rank_bucket)
                VALUES (?1, NULL, NULL, 'user', ?2, 'message')
                "#,
                params![
                    format!("bulk-event-{index}"),
                    format!("bulk token {index} {}", "payload ".repeat(2_048))
                ],
            )
            .unwrap();
        let wal_path = format!("{}-wal", db_path.display());
        peak_wal_bytes = peak_wal_bytes.max(
            std::fs::metadata(wal_path)
                .map(|metadata| metadata.len())
                .unwrap_or(0),
        );
    }

    let segments = store
        .conn
        .query_row(
            "SELECT COUNT(DISTINCT segid) FROM event_search_idx",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert!(segments >= 20, "expected unmerged segments, got {segments}");
    assert!(
        peak_wal_bytes <= 4 * 1024 * 1024,
        "bulk FTS writes grew WAL to {peak_wal_bytes} bytes"
    );

    store.finish_event_search_bulk_mode(&guard).unwrap();
    assert_eq!(bulk_mode_marker(&store), None);
    let compacted_segments = store
        .conn
        .query_row(
            "SELECT COUNT(DISTINCT segid) FROM event_search_idx",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(compacted_segments, 1);
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM event_search WHERE event_search MATCH 'bulk'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        20
    );
}

#[test]
fn bulk_search_finish_preserves_preexisting_optimized_segment() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open(&db_path).unwrap();

    let first_guard = store.begin_event_search_bulk_mode().unwrap();
    insert_bulk_search_events(&store, "historic", 80, 512);
    store.finish_event_search_bulk_mode(&first_guard).unwrap();
    drop(first_guard);
    assert_eq!(event_search_segment_count(&store), 1);

    let second_guard = store.begin_event_search_bulk_mode().unwrap();
    insert_bulk_search_events(&store, "new", 20, 128);
    assert_eq!(event_search_segment_count(&store), 21);
    store.finish_event_search_bulk_mode(&second_guard).unwrap();

    assert_eq!(
        event_search_segment_count(&store),
        2,
        "finishing one provider import must not re-optimize the historical index"
    );
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM event_search WHERE event_search MATCH 'historic OR new'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        100
    );
}

#[test]
fn bulk_search_recovery_resumes_legacy_in_progress_full_merge() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open(&db_path).unwrap();
    let guard = store.begin_event_search_bulk_mode().unwrap();
    insert_bulk_search_events(&store, "legacy-recovery", 40, 512);
    store
        .conn
        .execute(
            "INSERT INTO event_search(event_search, rank) VALUES ('merge', -1)",
            [],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO search_projection_stats (key, value, updated_at_ms)
            VALUES ('event_search_bulk_mode_v1:merge_started:event_search', 1, 0)
            "#,
            [],
        )
        .unwrap();
    drop(store);
    drop(guard);

    let reopened = Store::open(&db_path).unwrap();
    assert_eq!(bulk_mode_marker(&reopened), None);
    assert_eq!(
        reopened
            .conn
            .query_row(
                "SELECT COUNT(*) FROM search_projection_stats WHERE key LIKE 'event_search_bulk_mode_v1:%'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    assert_eq!(
        reopened
            .conn
            .query_row(
                "SELECT COUNT(*) FROM event_search WHERE event_search MATCH 'legacy'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        40
    );
}

fn insert_bulk_search_events(store: &Store, prefix: &str, count: usize, payload_words: usize) {
    for index in 0..count {
        store
            .conn
            .execute(
                r#"
                INSERT INTO event_search
                (event_id, history_record_id, session_id, role, preview_text, rank_bucket)
                VALUES (?1, NULL, NULL, 'user', ?2, 'message')
                "#,
                params![
                    format!("{prefix}-event-{index}"),
                    format!(
                        "{prefix} token {index} {}",
                        "payload ".repeat(payload_words)
                    )
                ],
            )
            .unwrap();
    }
}

fn event_search_segment_count(store: &Store) -> i64 {
    store
        .conn
        .query_row(
            "SELECT COUNT(DISTINCT segid) FROM event_search_idx",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn interrupted_bounded_merge_resumes_after_reopen() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open_with_busy_timeout(&db_path, Duration::from_millis(10)).unwrap();
    let guard = store.begin_event_search_bulk_mode().unwrap();
    for index in 0..20 {
        store
            .conn
            .execute(
                r#"
                INSERT INTO event_search
                (event_id, history_record_id, session_id, role, preview_text, rank_bucket)
                VALUES (?1, NULL, NULL, 'user', ?2, 'message')
                "#,
                params![
                    format!("resume-event-{index}"),
                    format!("resume token {index}")
                ],
            )
            .unwrap();
    }

    let reader = Connection::open(&db_path).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    let visible = reader
        .query_row("SELECT COUNT(*) FROM event_search", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(visible, 20);

    let error = store.finish_event_search_bulk_mode(&guard).unwrap_err();
    assert!(matches!(error, StoreError::WalCheckpointBusy { .. }));
    assert_eq!(bulk_mode_marker(&store), Some(1));
    reader.execute_batch("ROLLBACK").unwrap();
    drop(reader);
    drop(store);
    drop(guard);

    let reopened = Store::open(&db_path).unwrap();
    assert_eq!(bulk_mode_marker(&reopened), None);
    let segments = reopened
        .conn
        .query_row(
            "SELECT COUNT(DISTINCT segid) FROM event_search_idx",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(segments, 1);
    assert_eq!(
        reopened
            .conn
            .query_row(
                "SELECT COUNT(*) FROM event_search WHERE event_search MATCH 'resume'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        20
    );
}

#[test]
fn deferred_bulk_maintenance_keeps_the_marker_and_does_not_block_reopen() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open_with_busy_timeout(&db_path, Duration::from_millis(10)).unwrap();
    let guard = store.begin_event_search_bulk_mode().unwrap();
    insert_bulk_search_events(&store, "deferred-recovery", 20, 512);

    let reader = Connection::open(&db_path).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    assert_eq!(
        reader
            .query_row("SELECT COUNT(*) FROM event_search", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        20
    );

    // The checkpoint can be pinned, but deferred maintenance must leave the
    // successful import usable and preserve the resumable marker.
    store.defer_event_search_bulk_mode(&guard).unwrap();
    assert_eq!(bulk_mode_marker(&store), Some(1));
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM event_search WHERE event_search MATCH 'deferred'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        20
    );
    drop(guard);
    drop(store);

    // One bounded recovery pass can finish quiescent work, but its checkpoint
    // must not make opening fail while a reader still pins the WAL.
    let reopened = Store::open_with_busy_timeout(&db_path, Duration::from_millis(10)).unwrap();
    assert_eq!(bulk_mode_marker(&reopened), None);
    assert_eq!(
        reopened
            .conn
            .query_row("SELECT COUNT(*) FROM event_search", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        20
    );
    reader.execute_batch("ROLLBACK").unwrap();
}
