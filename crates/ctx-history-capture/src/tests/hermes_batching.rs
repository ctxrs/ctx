use crate::provider::providers::hermes::import_hermes_sqlite_batched;
use crate::tests::support::paths::tempdir;
use crate::{
    import_hermes_sqlite, HermesSqliteImportOptions, NormalizedProviderImportOptions,
    ProviderAdapterContext, ProviderImportSummary, MAX_PROVIDER_SQLITE_VALUE_BYTES,
};
use chrono::{DateTime, Utc};
use ctx_history_store::Store;
use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Barrier,
    },
    thread,
    time::Duration,
};
use tempfile::TempDir;

fn test_imported_at() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-07-18T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn import_hermes_captured(path: &Path, store: &mut Store) -> ProviderImportSummary {
    import_hermes_sqlite_batched(
        path,
        store,
        ProviderAdapterContext {
            machine_id: "hermes-captured-test-machine".to_owned(),
            source_path: Some(path.to_path_buf()),
            source_root: None,
            imported_at: test_imported_at(),
        },
        NormalizedProviderImportOptions {
            fast_event_inserts: true,
            capture_work_limit: crate::CaptureWorkLimit::Drain,
            inventory_observation_token: None,
            ..NormalizedProviderImportOptions::default()
        },
    )
    .unwrap()
}

#[test]
fn captured_hermes_import_crosses_message_batches_and_replays_from_cursor() {
    let temp = tempdir();
    let fixture = write_hermes_batched_db(&temp, 130);
    let db_path = temp.path().join("captured-work.sqlite");
    let mut store = Store::open(&db_path).unwrap();

    let first = import_hermes_captured(&fixture, &mut store);
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 130);
    assert_eq!(
        store
            .search_event_hits("hermes-batched-message-129", 10)
            .unwrap()
            .len(),
        1
    );

    let replay = import_hermes_captured(&fixture, &mut store);
    assert_eq!(replay.failed, 0, "{:?}", replay.failures);
    assert_eq!(replay.imported, 0);
    assert_eq!(replay.skipped, 0);
    assert_eq!(store.list_sessions().unwrap().len(), 1);
}

#[test]
fn captured_hermes_sessions_phase_preserves_eventless_sessions() {
    let temp = tempdir();
    let fixture = write_hermes_batched_db(&temp, 1);
    Connection::open(&fixture)
        .unwrap()
        .execute(
            "INSERT INTO sessions VALUES ('eventless', 'acp', 1782259300.0)",
            [],
        )
        .unwrap();
    let mut store = Store::open(temp.path().join("eventless-work.sqlite")).unwrap();

    let summary = import_hermes_captured(&fixture, &mut store);
    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 2);
    assert_eq!(summary.imported_events, 1);
    assert_eq!(store.list_sessions().unwrap().len(), 2);
}

#[test]
fn captured_hermes_changed_source_resets_and_imports_new_tail() {
    let temp = tempdir();
    let fixture = write_hermes_batched_db(&temp, 2);
    let mut store = Store::open(temp.path().join("changed-work.sqlite")).unwrap();
    let first = import_hermes_captured(&fixture, &mut store);
    assert_eq!(first.imported_events, 2);

    Connection::open(&fixture)
        .unwrap()
        .execute(
            "INSERT INTO messages (session_id, role, content, timestamp) \
             VALUES ('hermes-batched', 'assistant', 'hermes-changed-tail', 1782259300.0)",
            [],
        )
        .unwrap();

    let changed = import_hermes_captured(&fixture, &mut store);
    assert_eq!(changed.failed, 0, "{:?}", changed.failures);
    assert_eq!(changed.imported_events, 1);
    assert_eq!(
        store
            .search_event_hits("hermes-changed-tail", 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn captured_hermes_rejects_oversized_rows_before_hydration() {
    let temp = tempdir();
    let fixture = write_hermes_batched_db(&temp, 0);
    let oversized_bytes = i64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES)
        .unwrap()
        .saturating_add(1);
    Connection::open(&fixture)
        .unwrap()
        .execute(
            "INSERT INTO messages (session_id, role, content, timestamp) \
             VALUES ('hermes-batched', 'assistant', zeroblob(?1), 1782259300.0)",
            [oversized_bytes],
        )
        .unwrap();
    let mut store = Store::open(temp.path().join("oversized-work.sqlite")).unwrap();

    let summary = import_hermes_captured(&fixture, &mut store);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 0);
    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert!(summary.failures[0].error.contains("exceeds the"));
}

#[test]
fn native_hermes_import_crosses_batches_and_replays_idempotently() {
    let temp = tempdir();
    let fixture = write_hermes_batched_db(&temp, 130);
    let db_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&db_path).unwrap();
    let options = HermesSqliteImportOptions {
        source_path: Some(fixture.clone()),
        ..HermesSqliteImportOptions::default()
    };

    let first = import_hermes_sqlite(&fixture, &mut store, options.clone()).unwrap();
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 130);
    assert_eq!(
        store
            .search_event_hits("hermes-batched-message-129", 10)
            .unwrap()
            .len(),
        1
    );
    assert_bounded_wal(&db_path);

    let replay = import_hermes_sqlite(&fixture, &mut store, options).unwrap();
    assert_eq!(replay.failed, 0, "{:?}", replay.failures);
    assert_eq!(replay.imported_events, 0);
    assert_eq!(replay.skipped_events, 0);
    assert_eq!(
        store
            .search_event_hits("hermes-batched-message-129", 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn native_hermes_import_preserves_search_projection() {
    let temp = tempdir();
    let fixture = write_hermes_batched_db(&temp, 70);
    let db_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&db_path).unwrap();

    let summary = import_hermes_sqlite(
        &fixture,
        &mut store,
        HermesSqliteImportOptions {
            source_path: Some(fixture.clone()),
            ..HermesSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_events, 70);
    assert_eq!(
        store
            .search_event_hits("hermes-batched-message-69", 10)
            .unwrap()
            .len(),
        1
    );
    assert_bounded_wal(&db_path);
}

#[test]
fn native_hermes_import_preserves_preexisting_search_segment_and_bounds_peak_wal() {
    let temp = tempdir();
    let historic = write_hermes_batched_db_named(&temp, "historic", 256, 8 * 1024);
    let current = write_hermes_batched_db_named(&temp, "current", 130, 8 * 1024);
    let db_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&db_path).unwrap();

    let historic_summary = import_hermes_sqlite(
        &historic,
        &mut store,
        HermesSqliteImportOptions {
            source_path: Some(historic.clone()),
            ..HermesSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(
        historic_summary.failed, 0,
        "{:?}",
        historic_summary.failures
    );
    assert_eq!(historic_summary.imported_events, 256);
    assert!(
        event_search_segment_count(&db_path) > 1,
        "batched Hermes import must preserve its per-batch search segments"
    );

    let running = Arc::new(AtomicBool::new(true));
    let peak_wal_bytes = Arc::new(AtomicU64::new(0));
    let sampler_ready = Arc::new(Barrier::new(2));
    let sampler = {
        let running = Arc::clone(&running);
        let peak_wal_bytes = Arc::clone(&peak_wal_bytes);
        let sampler_ready = Arc::clone(&sampler_ready);
        let wal_path = PathBuf::from(format!("{}-wal", db_path.display()));
        thread::spawn(move || {
            sampler_ready.wait();
            loop {
                if let Ok(metadata) = fs::metadata(&wal_path) {
                    peak_wal_bytes.fetch_max(metadata.len(), Ordering::AcqRel);
                }
                if !running.load(Ordering::Acquire) {
                    break;
                }
                thread::sleep(Duration::from_millis(1));
            }
        })
    };
    sampler_ready.wait();
    let current_options = HermesSqliteImportOptions {
        source_path: Some(current.clone()),
        ..HermesSqliteImportOptions::default()
    };
    let current_summary = import_hermes_sqlite(&current, &mut store, current_options.clone());
    running.store(false, Ordering::Release);
    sampler.join().unwrap();
    let current_summary = current_summary.unwrap();

    assert_eq!(current_summary.failed, 0, "{:?}", current_summary.failures);
    assert_eq!(current_summary.imported_events, 130);
    assert!(
        peak_wal_bytes.load(Ordering::Acquire) <= 32 * 1024 * 1024,
        "pre-populated Hermes import grew WAL to {} bytes",
        peak_wal_bytes.load(Ordering::Acquire)
    );
    assert!(
        event_search_segment_count(&db_path) > 1,
        "finishing Hermes must not re-optimize the pre-existing search segment"
    );
    assert_eq!(bulk_search_marker_count(&db_path), 0);
    assert_eq!(integrity_check(&db_path), "ok");
    assert_eq!(
        store
            .search_event_hits("current-message-129", 10)
            .unwrap()
            .len(),
        1
    );

    let replay = import_hermes_sqlite(&current, &mut store, current_options).unwrap();
    assert_eq!(replay.failed, 0, "{:?}", replay.failures);
    assert_eq!(replay.imported_events, 0);
    assert_eq!(replay.skipped_events, 0);
}

fn assert_bounded_wal(db_path: &Path) {
    let wal_path = PathBuf::from(format!("{}-wal", db_path.display()));
    let wal_bytes = fs::metadata(&wal_path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    assert!(
        wal_bytes <= 4 * 1024 * 1024,
        "WAL remained at {wal_bytes} bytes"
    );
}

fn write_hermes_batched_db(temp: &TempDir, messages: usize) -> PathBuf {
    write_hermes_batched_db_named(temp, "hermes-batched", messages, 0)
}

fn write_hermes_batched_db_named(
    temp: &TempDir,
    name: &str,
    messages: usize,
    payload_bytes: usize,
) -> PathBuf {
    let path = temp.path().join(format!("{name}.db"));
    let mut conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (
            id TEXT PRIMARY KEY,
            source TEXT NOT NULL,
            started_at REAL NOT NULL
        );
        CREATE TABLE messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            role TEXT NOT NULL,
            content TEXT,
            timestamp REAL NOT NULL,
            active INTEGER NOT NULL DEFAULT 1,
            compacted INTEGER NOT NULL DEFAULT 0
        );",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sessions VALUES (?1, 'acp', 1782259200.0)",
        [name],
    )
    .unwrap();
    let transaction = conn.transaction().unwrap();
    for index in 0..messages {
        let role = if index % 2 == 0 { "user" } else { "assistant" };
        transaction
            .execute(
                "INSERT INTO messages (session_id, role, content, timestamp)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    name,
                    role,
                    format!("{name}-message-{index} {}", "x".repeat(payload_bytes)),
                    1782259201.0 + index as f64,
                ],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
    path
}

fn event_search_segment_count(db_path: &Path) -> i64 {
    Connection::open(db_path)
        .unwrap()
        .query_row(
            "SELECT COUNT(DISTINCT segid) FROM event_search_idx",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn bulk_search_marker_count(db_path: &Path) -> i64 {
    Connection::open(db_path)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM search_projection_stats WHERE key LIKE 'event_search_bulk_mode_v1%'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn integrity_check(db_path: &Path) -> String {
    Connection::open(db_path)
        .unwrap()
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap()
}
