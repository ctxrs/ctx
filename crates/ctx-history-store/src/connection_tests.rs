use std::{
    env, fs,
    process::Command,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use rusqlite::{params, Connection, OptionalExtension};

use crate::{CatalogSession, IndexingAdmission, IndexingWorkClass, Store, StoreError};
use ctx_history_core::{AgentType, CaptureProvider};

const ADMISSION_CRASH_DB_ENV: &str = "CTX_TEST_ADMISSION_CRASH_DB";
const FTS_ROTATION_DB_ENV: &str = "CTX_TEST_FTS_ROTATION_DB";
const FTS_ROTATION_READY_ENV: &str = "CTX_TEST_FTS_ROTATION_READY";
const CATALOG_RESULT_DB_ENV: &str = "CTX_TEST_CATALOG_RESULT_DB";
const CATALOG_RESULT_READY_ENV: &str = "CTX_TEST_CATALOG_RESULT_READY";
const CATALOG_RESULT_GO_ENV: &str = "CTX_TEST_CATALOG_RESULT_GO";
const CATALOG_RESULT_DONE_ENV: &str = "CTX_TEST_CATALOG_RESULT_DONE";

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
fn pressure_checkpoint_returns_bounded_with_pinned_reader_and_preserves_snapshot() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open_with_busy_timeout(&db_path, Duration::from_millis(10)).unwrap();
    store
        .conn
        .execute_batch(
            "CREATE TABLE pressure_probe(value BLOB);\
             INSERT INTO pressure_probe VALUES (zeroblob(1));\
             PRAGMA wal_checkpoint(TRUNCATE);",
        )
        .unwrap();

    let reader = Connection::open(&db_path).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    assert_eq!(
        reader
            .query_row("SELECT COUNT(*) FROM pressure_probe", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );

    store
        .conn
        .execute(
            "INSERT INTO pressure_probe VALUES (zeroblob(?1))",
            params![9 * 1024 * 1024_i64],
        )
        .unwrap();
    let started = Instant::now();
    let status = store.checkpoint_wal_for_pressure().unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(status.attempted);
    assert!(status.pinned(), "{status:?}");
    assert_eq!(
        reader
            .query_row("SELECT COUNT(*) FROM pressure_probe", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1,
        "checkpoint must preserve the pinned snapshot"
    );

    reader.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        store
            .conn
            .query_row("SELECT COUNT(*) FROM pressure_probe", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(store.validate().unwrap(), Vec::<String>::new());
}

#[test]
fn failed_main_commit_rolls_back_sidecar_before_releasing_admission() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let admission = IndexingAdmission::acquire(&db_path, IndexingWorkClass::Background).unwrap();
    let store = Store::open_admitted(&db_path, &admission).unwrap();
    store.begin_immediate_batch().unwrap();
    store
        .conn
        .execute_batch(
            "CREATE TABLE commit_parent(id INTEGER PRIMARY KEY);\
             CREATE TABLE commit_child(\
                 parent_id INTEGER REFERENCES commit_parent(id)\
                 DEFERRABLE INITIALLY DEFERRED\
             );",
        )
        .unwrap();
    store.commit_batch().unwrap();

    let guard = store.begin_event_search_bulk_mode().unwrap();
    store.begin_immediate_batch().unwrap();
    store
        .conn
        .execute("INSERT INTO commit_child(parent_id) VALUES (7)", [])
        .unwrap();
    assert!(store.commit_batch().is_err());
    assert!(store.conn.is_autocommit());
    assert!(store.event_search_transaction_lock.borrow().is_none());
    assert!(store.indexing_writer_lease.borrow().is_none());

    let foreground_admission =
        IndexingAdmission::acquire(&db_path, IndexingWorkClass::Foreground).unwrap();
    let foreground = Store::open_admitted_with_busy_timeout(
        &db_path,
        Duration::from_millis(100),
        &foreground_admission,
    )
    .unwrap();
    assert_eq!(
        foreground
            .conn
            .query_row("SELECT COUNT(*) FROM commit_child", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

#[test]
fn catalog_result_contention_child_helper() {
    let Some(db_path) = env::var_os(CATALOG_RESULT_DB_ENV) else {
        return;
    };
    let db_path = std::path::PathBuf::from(db_path);
    let ready = std::path::PathBuf::from(env::var_os(CATALOG_RESULT_READY_ENV).unwrap());
    let go = std::path::PathBuf::from(env::var_os(CATALOG_RESULT_GO_ENV).unwrap());
    let done = std::path::PathBuf::from(env::var_os(CATALOG_RESULT_DONE_ENV).unwrap());
    let admission = IndexingAdmission::acquire(&db_path, IndexingWorkClass::Background).unwrap();
    let store = Store::open_admitted(&db_path, &admission).unwrap();
    fs::write(&ready, b"ready").unwrap();
    while !go.exists() {
        thread::sleep(Duration::from_millis(2));
    }
    store
        .mark_catalog_source_failed(
            CaptureProvider::Codex,
            "catalog-root",
            "catalog-path",
            "expected test failure",
            2,
        )
        .unwrap();
    fs::write(done, b"done").unwrap();
}

#[test]
fn standalone_catalog_result_write_waits_for_scoped_writer_lane() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let ready = temp.path().join("ready");
    let go = temp.path().join("go");
    let done = temp.path().join("done");
    let admission = IndexingAdmission::acquire(&db_path, IndexingWorkClass::Background).unwrap();
    let store = Store::open_admitted(&db_path, &admission).unwrap();
    store
        .upsert_catalog_sessions(&[CatalogSession {
            provider: CaptureProvider::Codex,
            source_format: "codex_session_jsonl_tree".to_owned(),
            source_root: "catalog-root".to_owned(),
            source_path: "catalog-path".to_owned(),
            external_session_id: Some("catalog-session".to_owned()),
            parent_external_session_id: None,
            agent_type: AgentType::Primary,
            role_hint: None,
            external_agent_id: None,
            cwd: None,
            session_started_at_ms: Some(1),
            file_size_bytes: 1,
            file_modified_at_ms: 1,
            cataloged_at_ms: 1,
            metadata: serde_json::json!({}),
        }])
        .unwrap();

    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "connection_tests::catalog_result_contention_child_helper",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CATALOG_RESULT_DB_ENV, &db_path)
        .env(CATALOG_RESULT_READY_ENV, &ready)
        .env(CATALOG_RESULT_GO_ENV, &go)
        .env(CATALOG_RESULT_DONE_ENV, &done)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !ready.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(2));
    }
    assert!(ready.exists());

    let held_writer = admission.lease().unwrap();
    fs::write(&go, b"go").unwrap();
    thread::sleep(Duration::from_millis(100));
    assert!(
        !done.exists(),
        "standalone catalog result write bypassed global writer admission"
    );
    drop(held_writer);
    assert!(child.wait().unwrap().success());
    assert!(done.exists());
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT indexed_status FROM catalog_sessions WHERE source_path = 'catalog-path'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "failed"
    );
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
    drop(store);
    drop(guard);

    let reopened = Store::open(&db_path).unwrap();
    assert_eq!(bulk_mode_marker(&reopened), Some(1));
    while reopened.run_event_search_maintenance_slice().unwrap() {}
    assert_eq!(bulk_mode_marker(&reopened), None);
    for table in ["event_search", "event_search_scriptgram"] {
        assert_eq!(fts_config(&reopened, table, "automerge", 4), 8);
        assert_eq!(fts_config(&reopened, table, "crisismerge", 16), 32);
    }
}

#[test]
fn admitted_crash_child_helper() {
    let Some(db_path) = env::var_os(ADMISSION_CRASH_DB_ENV) else {
        return;
    };
    let db_path = std::path::PathBuf::from(db_path);
    let admission = IndexingAdmission::acquire(&db_path, IndexingWorkClass::Background).unwrap();
    let store = Store::open_admitted(&db_path, &admission).unwrap();
    let _guard = store.begin_event_search_bulk_mode().unwrap();
    for index in 0..20 {
        store.begin_immediate_batch().unwrap();
        insert_bulk_search_events(&store, &format!("admission-crash-{index}"), 1, 2_048);
        store.commit_batch().unwrap();
    }
    assert!(event_search_segment_count(&store) >= 20);
    assert_eq!(bulk_mode_marker(&store), Some(1));

    // Exit without destructors so the parent exercises OS lock cleanup and
    // persisted FTS recovery state rather than an orderly guard drop.
    std::process::exit(86);
}

#[test]
fn admitted_reopen_recovers_crashed_owner_one_slice_at_a_time() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "connection_tests::admitted_crash_child_helper",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ADMISSION_CRASH_DB_ENV, &db_path)
        .status()
        .unwrap();
    assert_eq!(child.code(), Some(86));

    let plain = Store::open(&db_path).unwrap();
    assert_eq!(bulk_mode_marker(&plain), Some(1));
    drop(plain);

    let admission = IndexingAdmission::acquire(&db_path, IndexingWorkClass::Foreground).unwrap();
    let mut reopened = Store::open_admitted(&db_path, &admission).unwrap();
    assert_eq!(
        bulk_mode_marker(&reopened),
        Some(1),
        "one admitted open must not drain all pending FTS maintenance"
    );
    for _ in 0..256 {
        if bulk_mode_marker(&reopened).is_none() {
            break;
        }
        drop(reopened);
        reopened = Store::open_admitted(&db_path, &admission).unwrap();
    }

    assert_eq!(bulk_mode_marker(&reopened), None);
    assert_eq!(
        reopened
            .conn
            .query_row(
                "SELECT COUNT(*) FROM event_search WHERE event_search MATCH 'admission'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        20
    );
    assert_eq!(reopened.validate().unwrap(), Vec::<String>::new());
}

#[test]
fn fts_rotation_child_helper() {
    let Some(db_path) = env::var_os(FTS_ROTATION_DB_ENV) else {
        return;
    };
    let ready = std::path::PathBuf::from(env::var_os(FTS_ROTATION_READY_ENV).unwrap());
    let db_path = std::path::PathBuf::from(db_path);
    let admission = IndexingAdmission::acquire(&db_path, IndexingWorkClass::Background).unwrap();
    let store = Store::open_admitted(&db_path, &admission).unwrap();
    let guard = store.begin_event_search_bulk_mode().unwrap();
    for index in 0..12 {
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
                    format!("rotation-{index}"),
                    format!("rotation token {index}")
                ],
            )
            .unwrap();
        store.commit_batch().unwrap();
        if index == 0 {
            fs::write(&ready, b"ready").unwrap();
        }
        thread::sleep(Duration::from_millis(20));
    }
    store.finish_event_search_bulk_mode(&guard).unwrap();
}

#[test]
fn multiprocess_fts_rotation_hands_off_to_foreground_and_recovers() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let ready = temp.path().join("ready");
    let initial = Store::open(&db_path).unwrap();
    initial
        .conn
        .execute("CREATE TABLE foreground_probe(value INTEGER)", [])
        .unwrap();
    drop(initial);

    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "connection_tests::fts_rotation_child_helper",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(FTS_ROTATION_DB_ENV, &db_path)
        .env(FTS_ROTATION_READY_ENV, &ready)
        .spawn()
        .unwrap();
    let ready_deadline = Instant::now() + Duration::from_secs(2);
    while !ready.exists() && Instant::now() < ready_deadline {
        thread::sleep(Duration::from_millis(2));
    }
    assert!(ready.exists());

    let started = Instant::now();
    let admission = IndexingAdmission::acquire(&db_path, IndexingWorkClass::Foreground).unwrap();
    let foreground = Store::open_admitted(&db_path, &admission).unwrap();
    foreground.begin_immediate_batch().unwrap();
    foreground
        .conn
        .execute("INSERT INTO foreground_probe VALUES (1)", [])
        .unwrap();
    foreground.commit_batch().unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(child.wait().unwrap().success());
    drop(foreground);

    let mut reopened = Store::open_admitted(&db_path, &admission).unwrap();
    for _ in 0..256 {
        if bulk_mode_marker(&reopened).is_none() {
            break;
        }
        drop(reopened);
        reopened = Store::open_admitted(&db_path, &admission).unwrap();
    }
    assert_eq!(bulk_mode_marker(&reopened), None);
    assert_eq!(
        reopened
            .conn
            .query_row("SELECT COUNT(*) FROM foreground_probe", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        reopened
            .conn
            .query_row(
                "SELECT COUNT(*) FROM event_search WHERE event_search MATCH 'rotation'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        12
    );
    assert_eq!(reopened.validate().unwrap(), Vec::<String>::new());
}

#[test]
fn fts_admission_wait_is_not_counted_as_active_slice_time() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let background_admission =
        IndexingAdmission::acquire(&db_path, IndexingWorkClass::Background).unwrap();
    let foreground_admission =
        IndexingAdmission::acquire(&db_path, IndexingWorkClass::Foreground).unwrap();
    let background = Store::open_admitted(&db_path, &background_admission).unwrap();
    let foreground = Store::open_admitted(&db_path, &foreground_admission).unwrap();
    let guard = background.begin_event_search_bulk_mode().unwrap();

    let (ready_tx, ready_rx) = mpsc::channel();
    let foreground = thread::spawn(move || {
        foreground.begin_immediate_batch().unwrap();
        ready_tx.send(()).unwrap();
        thread::sleep(Duration::from_secs(1));
        foreground.commit_batch().unwrap();
    });
    ready_rx.recv().unwrap();

    let started = Instant::now();
    background.finish_event_search_bulk_mode(&guard).unwrap();
    let elapsed = started.elapsed();
    foreground.join().unwrap();

    assert!(elapsed >= Duration::from_millis(900), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
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

    assert_eq!(bulk_mode_marker(&store), None);
    for table in ["event_search", "event_search_scriptgram"] {
        assert_eq!(fts_config(&store, table, "automerge", 4), 8);
        assert_eq!(fts_config(&store, table, "crisismerge", 16), 32);
    }
}

#[test]
fn overlapping_bulk_search_scopes_release_ownership_between_transactions() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let first = Store::open(&db_path).unwrap();
    let guard = first.begin_event_search_bulk_mode().unwrap();
    let second = Store::open_with_busy_timeout(&db_path, Duration::from_millis(10)).unwrap();

    let second_guard = second.begin_event_search_bulk_mode().unwrap();
    assert_eq!(bulk_mode_marker(&second), Some(1));
    for table in ["event_search", "event_search_scriptgram"] {
        assert_eq!(fts_config(&second, table, "automerge", 4), 0);
        assert_eq!(fts_config(&second, table, "crisismerge", 16), 1_000_000);
    }

    second.finish_event_search_bulk_mode(&second_guard).unwrap();
    drop(second_guard);
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
    let overlapping = second.begin_event_search_bulk_mode().unwrap();
    drop(overlapping);

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
fn optimize_and_bulk_import_reacquire_fts_ownership_in_canonical_order() {
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

    second.optimize_search_index().unwrap();

    first.begin_immediate_batch().unwrap();
    first
        .conn
        .execute(
            "INSERT INTO event_search (event_id, role, preview_text, rank_bucket) VALUES ('reacquired', 'user', 'reacquired', 'message')",
            [],
        )
        .unwrap();
    first.commit_batch().unwrap();
    assert_eq!(bulk_mode_marker(&first), Some(1));

    first.finish_event_search_bulk_mode(&guard).unwrap();
    drop(guard);
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
    assert_eq!(
        bulk_mode_marker(&store),
        Some(1),
        "one finish call must perform only one bounded maintenance slice"
    );
    while store.run_event_search_maintenance_slice().unwrap() {}
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
    assert_eq!(bulk_mode_marker(&reopened), Some(1));
    while reopened.run_event_search_maintenance_slice().unwrap() {}
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

    store.finish_event_search_bulk_mode(&guard).unwrap();
    assert_eq!(bulk_mode_marker(&store), Some(1));
    reader.execute_batch("ROLLBACK").unwrap();
    drop(reader);
    drop(store);
    drop(guard);

    let reopened = Store::open(&db_path).unwrap();
    assert_eq!(bulk_mode_marker(&reopened), Some(1));
    while reopened.run_event_search_maintenance_slice().unwrap() {}
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
