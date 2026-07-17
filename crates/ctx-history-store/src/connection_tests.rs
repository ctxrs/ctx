use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use rusqlite::{params, Connection, OptionalExtension};

use crate::{
    bulk_search::{
        event_search_final_checkpoint_debt_path, set_checkpoint_debt_persisted_hook,
        set_final_checkpoint_post_checkpoint_hook, set_restore_post_commit_hook,
        FTS_BULK_CRISISMERGE, FTS_BULK_MAINTENANCE_BATCHES,
    },
    EventSearchBulkMaintenanceOutcome, Store, StoreError,
};

const BULK_SEARCH_WAL_HIGH_WATER_BYTES: u64 = 64 * 1024 * 1024;

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
fn bulk_search_mode_waits_for_paced_recovery_and_restores_saved_config() {
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
        assert_eq!(
            fts_config(&store, table, "crisismerge", 16),
            FTS_BULK_CRISISMERGE
        );
    }
    drop(store);
    drop(guard);

    let reopened = Store::open(&db_path).unwrap();
    assert_eq!(bulk_mode_marker(&reopened), Some(1));
    for table in ["event_search", "event_search_scriptgram"] {
        assert_eq!(fts_config(&reopened, table, "automerge", 4), 0);
        assert_eq!(
            fts_config(&reopened, table, "crisismerge", 16),
            FTS_BULK_CRISISMERGE
        );
    }

    let _pacing = crate::install_event_search_maintenance_pacer(|_| {});
    assert!(reopened
        .advance_event_search_bulk_maintenance()
        .unwrap()
        .is_complete());
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
        assert_eq!(
            fts_config(&second, table, "crisismerge", 16),
            FTS_BULK_CRISISMERGE
        );
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

    assert_eq!(
        first.finish_event_search_bulk_mode(&nested).unwrap(),
        EventSearchBulkMaintenanceOutcome::Complete
    );
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
fn paced_bulk_search_recovery_resumes_legacy_in_progress_full_merge() {
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

    let _pacing = crate::install_event_search_maintenance_pacer(|_| {});
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
fn interrupted_bounded_merge_resumes_after_paced_reopen() {
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

    let _pacing = crate::install_event_search_maintenance_pacer(|_| {});
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

fn insert_committed_bulk_search_event(store: &Store, prefix: &str, index: usize) {
    store.begin_immediate_batch().unwrap();
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
                format!("{prefix} needle {index}")
            ],
        )
        .unwrap();
    store.commit_batch().unwrap();
}

fn wal_bytes(db_path: &std::path::Path) -> u64 {
    let wal_path = PathBuf::from(format!("{}-wal", db_path.display()));
    std::fs::metadata(wal_path)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

fn assert_private_checkpoint_debt(db_path: &std::path::Path) {
    let debt_path = event_search_final_checkpoint_debt_path(db_path);
    assert_eq!(
        std::fs::read(&debt_path).unwrap(),
        b"ctx-event-search-final-checkpoint-debt\nversion=1\n"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(debt_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn subthreshold_changed_slice_coalesces_its_intermediate_checkpoints() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open(&db_path).unwrap();
    let guard = store.begin_event_search_bulk_mode().unwrap();
    insert_bulk_search_events(&store, "coalesced", 20, 8);
    store
        .conn
        .execute(
            r#"
            INSERT INTO search_projection_stats (key, value, updated_at_ms)
            VALUES ('event_search_bulk_mode_v1:test_remaining_merge_passes', 3, 0)
            "#,
            [],
        )
        .unwrap();

    assert_eq!(
        store.finish_event_search_bulk_mode(&guard).unwrap(),
        EventSearchBulkMaintenanceOutcome::Pending
    );
    let pending_wal_bytes = wal_bytes(&db_path);
    assert!(pending_wal_bytes > 0);
    assert!(pending_wal_bytes < BULK_SEARCH_WAL_HIGH_WATER_BYTES);
    assert_eq!(bulk_mode_marker(&store), Some(1));

    for _ in 0..32 {
        if store
            .finish_event_search_bulk_mode(&guard)
            .unwrap()
            .is_complete()
        {
            break;
        }
    }
    assert_eq!(bulk_mode_marker(&store), None);
    assert_eq!(wal_bytes(&db_path), 0);
}

#[test]
fn bulk_search_periodic_maintenance_starts_at_256_committed_groups() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open(&db_path).unwrap();
    let outer = store.begin_event_search_bulk_mode().unwrap();

    for index in 0..FTS_BULK_MAINTENANCE_BATCHES {
        insert_committed_bulk_search_event(&store, "cadence", index);
        assert_eq!(
            store.maintain_event_search_bulk_mode().unwrap(),
            EventSearchBulkMaintenanceOutcome::Complete
        );
        if index + 1 == FTS_BULK_MAINTENANCE_BATCHES - 1 {
            assert_eq!(
                event_search_segment_count(&store),
                (index + 1) as i64,
                "maintenance ran before the 256-group cadence"
            );
        }
    }

    assert!(
        event_search_segment_count(&store) < FTS_BULK_MAINTENANCE_BATCHES as i64,
        "the first bounded cadence slice did not reduce segment debt"
    );
    assert_eq!(bulk_mode_marker(&store), Some(1));

    for _ in 0..32 {
        if store
            .finish_event_search_bulk_mode(&outer)
            .unwrap()
            .is_complete()
        {
            break;
        }
    }
    assert_eq!(bulk_mode_marker(&store), None);
}

#[test]
fn bulk_search_crisis_threshold_bounds_uncounted_connection_segments() {
    const UNCOUNTED_GROUPS: usize = 2_100;
    const _: () = assert!(FTS_BULK_CRISISMERGE > FTS_BULK_MAINTENANCE_BATCHES as i64);
    const _: () = assert!(FTS_BULK_CRISISMERGE * 3 < 2_000);

    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let owner = Store::open(&db_path).unwrap();
    let outer = owner.begin_event_search_bulk_mode().unwrap();
    let other = Store::open(&db_path).unwrap();

    for index in 0..UNCOUNTED_GROUPS {
        other
            .conn
            .execute(
                r#"
                INSERT INTO event_search
                (event_id, history_record_id, session_id, role, preview_text, rank_bucket)
                VALUES (?1, NULL, NULL, 'user', ?2, 'message')
                "#,
                params![
                    format!("uncounted-event-{index}"),
                    format!("uncounted concurrent needle {index}")
                ],
            )
            .unwrap();
    }

    assert_eq!(bulk_mode_marker(&other), Some(1));
    assert!(
        event_search_segment_count(&other) < FTS_BULK_CRISISMERGE,
        "the database-global crisis threshold did not bound uncounted writers"
    );
    assert_eq!(
        other
            .conn
            .query_row(
                "SELECT COUNT(*) FROM event_search WHERE event_search MATCH 'uncounted AND concurrent AND needle'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        UNCOUNTED_GROUPS as i64
    );

    for _ in 0..32 {
        if owner
            .finish_event_search_bulk_mode(&outer)
            .unwrap()
            .is_complete()
        {
            break;
        }
    }
    assert_eq!(bulk_mode_marker(&owner), None);
}

#[test]
fn pinned_high_water_suspends_admission_without_further_wal_growth() {
    const GROUP_BYTES: i64 = 1024 * 1024;
    const MAX_GROUPS: usize = 128;

    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open_with_busy_timeout(&db_path, Duration::from_millis(10)).unwrap();
    store
        .conn
        .execute("CREATE TABLE admission_probe(value BLOB NOT NULL)", [])
        .unwrap();
    let guard = store.begin_event_search_bulk_mode().unwrap();

    let reader = Connection::open(&db_path).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    assert_eq!(
        reader
            .query_row("SELECT COUNT(*) FROM admission_probe", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );

    let mut blocked_wal_bytes = None;
    let mut largest_group_growth = 0;
    for _ in 0..MAX_GROUPS {
        assert_eq!(
            store.event_search_bulk_admission_outcome().unwrap(),
            EventSearchBulkMaintenanceOutcome::Complete
        );
        let before = wal_bytes(&db_path);
        store.begin_immediate_batch().unwrap();
        store
            .conn
            .execute(
                "INSERT INTO admission_probe VALUES (zeroblob(?1))",
                [GROUP_BYTES],
            )
            .unwrap();
        store.commit_batch().unwrap();
        let after = wal_bytes(&db_path);
        largest_group_growth = largest_group_growth.max(after.saturating_sub(before));

        match store
            .checkpoint_wal_truncate_required_if_larger_than(BULK_SEARCH_WAL_HIGH_WATER_BYTES)
        {
            Ok(false) => {}
            Ok(true) => panic!("pinned checkpoint unexpectedly truncated the WAL"),
            Err(StoreError::WalCheckpointBusy { .. }) => {
                blocked_wal_bytes = Some(after);
                break;
            }
            Err(error) => panic!("unexpected checkpoint error: {error}"),
        }
    }

    let blocked_wal_bytes = blocked_wal_bytes.expect("fixture did not reach the WAL high-water");
    assert!(blocked_wal_bytes >= BULK_SEARCH_WAL_HIGH_WATER_BYTES);
    assert!(
        blocked_wal_bytes < BULK_SEARCH_WAL_HIGH_WATER_BYTES.saturating_add(largest_group_growth),
        "WAL overshoot exceeded the one already-admitted bounded group"
    );
    for _ in 0..8 {
        assert_eq!(
            store.event_search_bulk_admission_outcome().unwrap(),
            EventSearchBulkMaintenanceOutcome::Pending
        );
        assert_eq!(wal_bytes(&db_path), blocked_wal_bytes);
    }

    reader.execute_batch("ROLLBACK").unwrap();
    drop(reader);
    assert!(store
        .checkpoint_wal_truncate_required_if_larger_than(BULK_SEARCH_WAL_HIGH_WATER_BYTES)
        .unwrap());
    assert_eq!(
        store.event_search_bulk_admission_outcome().unwrap(),
        EventSearchBulkMaintenanceOutcome::Complete
    );
    assert!(store
        .finish_event_search_bulk_mode(&guard)
        .unwrap()
        .is_complete());
}

#[test]
fn final_checkpoint_phase_survives_reopen_and_refuses_admission() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open_with_busy_timeout(&db_path, Duration::from_millis(10)).unwrap();
    for table in ["event_search", "event_search_scriptgram"] {
        set_fts_config(&store, table, "automerge", 8);
        set_fts_config(&store, table, "crisismerge", 32);
    }
    let guard = store.begin_event_search_bulk_mode().unwrap();
    insert_bulk_search_events(&store, "final-handoff", 20, 8);

    let pinned_reader = Arc::new(Mutex::new(None));
    let hook_reader = Arc::clone(&pinned_reader);
    let hook_db_path = db_path.clone();
    set_restore_post_commit_hook(
        db_path.clone(),
        Box::new(move || {
            let reader = Connection::open(&hook_db_path).unwrap();
            reader.execute_batch("BEGIN").unwrap();
            assert_eq!(
                reader
                    .query_row(
                        "SELECT COUNT(*) FROM search_projection_stats WHERE key = 'event_search_bulk_mode_v1'",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap(),
                0,
                "main recovery marker remained after config restoration"
            );
            assert!(event_search_final_checkpoint_debt_path(&hook_db_path).is_file());
            *hook_reader.lock().unwrap() = Some(reader);
        }),
    );

    assert!(matches!(
        store.finish_event_search_bulk_mode(&guard).unwrap_err(),
        StoreError::WalCheckpointBusy { .. }
    ));
    assert_eq!(bulk_mode_marker(&store), None);
    assert_private_checkpoint_debt(&db_path);
    for table in ["event_search", "event_search_scriptgram"] {
        assert_eq!(fts_config(&store, table, "automerge", 4), 8);
        assert_eq!(fts_config(&store, table, "crisismerge", 16), 32);
    }
    assert!(wal_bytes(&db_path) > 0);
    assert_eq!(
        store.event_search_bulk_maintenance_outcome().unwrap(),
        EventSearchBulkMaintenanceOutcome::Pending
    );
    assert_eq!(
        store.event_search_bulk_admission_outcome().unwrap(),
        EventSearchBulkMaintenanceOutcome::Pending
    );

    assert!(matches!(
        store.finish_event_search_bulk_mode(&guard).unwrap_err(),
        StoreError::WalCheckpointBusy { .. }
    ));
    drop(guard);
    drop(store);

    let reopened = Store::open_with_busy_timeout(&db_path, Duration::from_millis(10)).unwrap();
    assert_eq!(bulk_mode_marker(&reopened), None);
    assert!(event_search_final_checkpoint_debt_path(&db_path).is_file());
    for table in ["event_search", "event_search_scriptgram"] {
        assert_eq!(fts_config(&reopened, table, "automerge", 4), 8);
        assert_eq!(fts_config(&reopened, table, "crisismerge", 16), 32);
    }
    assert_eq!(
        reopened.event_search_bulk_maintenance_outcome().unwrap(),
        EventSearchBulkMaintenanceOutcome::Pending
    );
    assert_eq!(
        reopened.event_search_bulk_admission_outcome().unwrap(),
        EventSearchBulkMaintenanceOutcome::Pending
    );
    let begin_error = match reopened.begin_event_search_bulk_mode() {
        Ok(_) => panic!("final-checkpoint debt admitted a new bulk import"),
        Err(error) => error,
    };
    assert!(matches!(begin_error, StoreError::BulkSearchImportBusy));

    let reader = pinned_reader.lock().unwrap().take().unwrap();
    reader.execute_batch("ROLLBACK").unwrap();
    drop(reader);
    assert!(reopened
        .advance_event_search_bulk_maintenance()
        .unwrap()
        .is_complete());
    assert_eq!(bulk_mode_marker(&reopened), None);
    assert!(!event_search_final_checkpoint_debt_path(&db_path).exists());
    assert_eq!(wal_bytes(&db_path), 0);
}

#[test]
fn checkpoint_debt_handoff_writer_contention_is_retryable_and_converges() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open_with_busy_timeout(&db_path, Duration::from_millis(10)).unwrap();
    for table in ["event_search", "event_search_scriptgram"] {
        set_fts_config(&store, table, "automerge", 8);
        set_fts_config(&store, table, "crisismerge", 32);
    }
    let guard = store.begin_event_search_bulk_mode().unwrap();
    insert_bulk_search_events(&store, "writer-handoff", 20, 8);

    let blocking_writer = Arc::new(Mutex::new(None));
    let hook_writer = Arc::clone(&blocking_writer);
    let hook_db_path = db_path.clone();
    set_checkpoint_debt_persisted_hook(
        db_path.clone(),
        Box::new(move || {
            let writer = Connection::open(&hook_db_path).unwrap();
            writer.execute_batch("BEGIN IMMEDIATE").unwrap();
            *hook_writer.lock().unwrap() = Some(writer);
        }),
    );

    let mut saw_retryable_contention = false;
    for _ in 0..32 {
        match store.finish_event_search_bulk_mode(&guard) {
            Ok(EventSearchBulkMaintenanceOutcome::Pending) => {}
            Ok(EventSearchBulkMaintenanceOutcome::Complete) => {
                panic!("finalization completed before the handoff contention hook ran")
            }
            Err(StoreError::BulkSearchImportBusy) => {
                saw_retryable_contention = true;
                break;
            }
            Err(error) => panic!("unexpected finalization error: {error}"),
        }
    }
    assert!(saw_retryable_contention);
    assert_eq!(bulk_mode_marker(&store), Some(1));
    assert!(event_search_final_checkpoint_debt_path(&db_path).is_file());
    assert_eq!(
        store.event_search_bulk_admission_outcome().unwrap(),
        EventSearchBulkMaintenanceOutcome::Pending
    );
    for table in ["event_search", "event_search_scriptgram"] {
        assert_eq!(fts_config(&store, table, "automerge", 4), 0);
        assert_eq!(
            fts_config(&store, table, "crisismerge", 16),
            FTS_BULK_CRISISMERGE
        );
    }

    drop(guard);
    drop(store);
    let writer = blocking_writer.lock().unwrap().take().unwrap();
    writer.execute_batch("ROLLBACK").unwrap();
    drop(writer);

    let reopened = Store::open_with_busy_timeout(&db_path, Duration::from_millis(10)).unwrap();
    assert_eq!(bulk_mode_marker(&reopened), Some(1));
    assert!(event_search_final_checkpoint_debt_path(&db_path).is_file());
    assert_eq!(
        reopened.event_search_bulk_admission_outcome().unwrap(),
        EventSearchBulkMaintenanceOutcome::Pending
    );
    assert!(matches!(
        reopened.begin_event_search_bulk_mode().err().unwrap(),
        StoreError::BulkSearchImportBusy
    ));

    let mut maintenance_complete = false;
    for _ in 0..32 {
        if reopened
            .advance_event_search_bulk_maintenance()
            .unwrap()
            .is_complete()
        {
            maintenance_complete = true;
            break;
        }
    }
    assert!(maintenance_complete);
    assert_eq!(bulk_mode_marker(&reopened), None);
    assert!(!event_search_final_checkpoint_debt_path(&db_path).exists());
    assert_eq!(wal_bytes(&db_path), 0);
    for table in ["event_search", "event_search_scriptgram"] {
        assert_eq!(fts_config(&reopened, table, "automerge", 4), 8);
        assert_eq!(fts_config(&reopened, table, "crisismerge", 16), 32);
    }
}

#[test]
fn checkpoint_complete_crash_debt_reopens_pending_and_converges_without_main_write() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open(&db_path).unwrap();
    let guard = store.begin_event_search_bulk_mode().unwrap();
    insert_bulk_search_events(&store, "checkpoint-crash", 20, 8);
    set_final_checkpoint_post_checkpoint_hook(db_path.clone(), Box::new(|| true));

    let mut saw_crash_boundary = false;
    for _ in 0..32 {
        match store.finish_event_search_bulk_mode(&guard) {
            Ok(EventSearchBulkMaintenanceOutcome::Pending) => {}
            Ok(EventSearchBulkMaintenanceOutcome::Complete) => {
                panic!("finalization removed debt before the crash hook ran")
            }
            Err(StoreError::BulkSearchImportBusy) => {
                saw_crash_boundary = true;
                break;
            }
            Err(error) => panic!("unexpected finalization error: {error}"),
        }
    }
    assert!(saw_crash_boundary);
    assert_eq!(bulk_mode_marker(&store), None);
    assert!(event_search_final_checkpoint_debt_path(&db_path).is_file());
    assert_eq!(wal_bytes(&db_path), 0);
    drop(guard);
    drop(store);

    let reopened = Store::open(&db_path).unwrap();
    assert!(event_search_final_checkpoint_debt_path(&db_path).is_file());
    assert_eq!(
        reopened.event_search_bulk_admission_outcome().unwrap(),
        EventSearchBulkMaintenanceOutcome::Pending
    );
    assert!(reopened
        .advance_event_search_bulk_maintenance()
        .unwrap()
        .is_complete());
    assert!(!event_search_final_checkpoint_debt_path(&db_path).exists());
    assert_eq!(wal_bytes(&db_path), 0);
}

#[test]
fn unknown_bulk_mode_marker_values_fail_closed() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open(&db_path).unwrap();

    for value in [0, -1, 2, i64::MAX] {
        store
            .conn
            .execute(
                r#"
                INSERT INTO search_projection_stats (key, value, updated_at_ms)
                VALUES ('event_search_bulk_mode_v1', ?1, 0)
                ON CONFLICT(key) DO UPDATE SET value = excluded.value
                "#,
                [value],
            )
            .unwrap();
        assert!(matches!(
            store.event_search_bulk_maintenance_outcome().unwrap_err(),
            StoreError::InvalidBulkSearchPhase(invalid) if invalid == value
        ));
    }
}

#[test]
fn malformed_checkpoint_debt_fails_closed_on_reopen() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open(&db_path).unwrap();
    let debt_path = event_search_final_checkpoint_debt_path(&db_path);
    std::fs::write(&debt_path, b"future-or-tampered-debt\n").unwrap();
    crate::object_store::restrict_private_file(&debt_path).unwrap();

    assert!(matches!(
        store.event_search_bulk_maintenance_outcome().unwrap_err(),
        StoreError::InvalidBulkSearchCheckpointDebt(_)
    ));
    drop(store);

    let error = match Store::open(&db_path) {
        Ok(_) => panic!("malformed checkpoint debt did not fail closed on reopen"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        StoreError::InvalidBulkSearchCheckpointDebt(_)
    ));
}
