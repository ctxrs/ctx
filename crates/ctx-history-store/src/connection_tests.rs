use std::{
    fs,
    process::{Child, Command},
    thread,
    time::{Duration, Instant},
};

use rusqlite::{params, Connection, OptionalExtension};

use crate::{Store, StoreError};

const STAGE_A_PROCESS_ROLE: &str = "CTX_TEST_STAGE_A_PROCESS_ROLE";

fn wait_for_path(path: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn stage_a_helper_command(role: &str, db_path: &std::path::Path) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .arg("--exact")
        .arg("connection_tests::stage_a_process_helper")
        .arg("--nocapture")
        .env(STAGE_A_PROCESS_ROLE, role)
        .env("CTX_TEST_STAGE_A_DB", db_path);
    command
}

fn wait_for_child(mut child: Child) {
    let status = child.wait().unwrap();
    assert!(status.success(), "stage A helper failed: {status}");
}

fn downgrade_to_schema_46(path: &std::path::Path) {
    let store = Store::open(path).unwrap();
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

#[test]
fn stage_a_process_helper() {
    let Ok(role) = std::env::var(STAGE_A_PROCESS_ROLE) else {
        return;
    };
    let db_path = std::path::PathBuf::from(std::env::var_os("CTX_TEST_STAGE_A_DB").unwrap());
    match role.as_str() {
        "open_store" => {
            if let Some(started) = std::env::var_os("CTX_TEST_STAGE_A_STARTED") {
                fs::write(started, b"started").unwrap();
            }
            let store = Store::open(&db_path).unwrap();
            assert_eq!(
                store
                    .conn
                    .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                crate::SCHEMA_VERSION
            );
            if let Some(done) = std::env::var_os("CTX_TEST_STAGE_A_DONE") {
                fs::write(done, b"done").unwrap();
            }
        }
        "legacy_writer" => {
            let event_id = std::env::var("CTX_TEST_STAGE_A_EVENT_ID").unwrap();
            let ready = std::path::PathBuf::from(
                std::env::var_os("CTX_TEST_STAGE_A_WRITER_READY").unwrap(),
            );
            let release = std::path::PathBuf::from(
                std::env::var_os("CTX_TEST_STAGE_A_WRITER_RELEASE").unwrap(),
            );
            let conn = Connection::open(&db_path).unwrap();
            conn.busy_timeout(Duration::from_secs(5)).unwrap();
            conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;")
                .unwrap();
            let mut statement = conn
                .prepare("UPDATE events SET payload_json = ?1 WHERE id = ?2")
                .unwrap();
            fs::write(&ready, b"ready").unwrap();
            wait_for_path(&release);
            let error = statement
                .execute(params![r#"{"text":"stale writer"}"#, event_id])
                .expect_err("a pre-activation legacy writer must be fenced");
            assert!(
                error
                    .to_string()
                    .contains("ctx_projection_writer_authorized_v1")
                    || error
                        .to_string()
                        .contains("ctx projection journal requires a current writer"),
                "unexpected writer fence error: {error}"
            );
        }
        "read_after_writer" => {
            let ready = std::path::PathBuf::from(
                std::env::var_os("CTX_TEST_STAGE_A_READER_READY").unwrap(),
            );
            let release = std::path::PathBuf::from(
                std::env::var_os("CTX_TEST_STAGE_A_READER_RELEASE").unwrap(),
            );
            let store = Store::open_read_only(&db_path).unwrap();
            fs::write(&ready, b"ready").unwrap();
            wait_for_path(&release);
            let event_count = store
                .conn
                .query_row("SELECT COUNT(*) FROM events", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap();
            assert_eq!(
                event_count, 2,
                "live read-only connection omitted the writer's committed WAL row"
            );
        }
        other => panic!("unknown stage A helper role {other}"),
    }
}

#[test]
fn read_only_open_participates_in_wal_when_writer_starts_after_open() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let initial = Store::open(&db_path).unwrap();
    initial
        .conn
        .execute(
            "INSERT INTO events
             (id, seq, event_type, role, occurred_at_ms, payload_json, visibility,
              fidelity, sync_state, sync_version, metadata_json)
             VALUES (?1, 1, 'message', 'user', 1, '{}', 'local_only', 'full',
                     'local_only', 0, '{}')",
            [uuid::Uuid::new_v4().to_string()],
        )
        .unwrap();
    initial.checkpoint_wal_truncate_required().unwrap();
    drop(initial);
    // Copy the checkpointed main database to create a platform-independent
    // no-sidecar starting point without deleting SQLite-owned files.
    let race_path = temp.path().join("race.sqlite");
    fs::copy(&db_path, &race_path).unwrap();
    let db_path = race_path;
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = db_path.as_os_str().to_os_string();
        sidecar.push(suffix);
        assert!(
            !std::path::PathBuf::from(sidecar).exists(),
            "test requires no sidecar before the reader opens"
        );
    }

    let reader_ready = temp.path().join("reader-ready");
    let reader_release = temp.path().join("reader-release");
    let reader = stage_a_helper_command("read_after_writer", &db_path)
        .env("CTX_TEST_STAGE_A_READER_READY", &reader_ready)
        .env("CTX_TEST_STAGE_A_READER_RELEASE", &reader_release)
        .spawn()
        .unwrap();
    wait_for_path(&reader_ready);

    let writer = Connection::open(&db_path).unwrap();
    writer
        .execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
        .unwrap();
    writer
        .execute(
            "INSERT INTO events
             (id, seq, event_type, role, occurred_at_ms, payload_json, visibility,
              fidelity, sync_state, sync_version, metadata_json)
             VALUES (?1, 2, 'message', 'assistant', 2, '{}', 'local_only', 'full',
                     'local_only', 0, '{}')",
            [uuid::Uuid::new_v4().to_string()],
        )
        .unwrap();
    fs::write(&reader_release, b"release").unwrap();
    wait_for_child(reader);
    drop(writer);
}

#[test]
fn migration_dispatch_is_serialized_across_processes_before_version_read() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    downgrade_to_schema_46(&db_path);
    let first_ready = temp.path().join("first-ready");
    let first_release = temp.path().join("first-release");
    let first_done = temp.path().join("first-done");
    let second_started = temp.path().join("second-started");
    let second_done = temp.path().join("second-done");

    let first = stage_a_helper_command("open_store", &db_path)
        .env("CTX_TEST_MIGRATION_READY", &first_ready)
        .env("CTX_TEST_MIGRATION_RELEASE", &first_release)
        .env("CTX_TEST_STAGE_A_DONE", &first_done)
        .spawn()
        .unwrap();
    wait_for_path(&first_ready);
    let second = stage_a_helper_command("open_store", &db_path)
        .env("CTX_TEST_STAGE_A_STARTED", &second_started)
        .env("CTX_TEST_STAGE_A_DONE", &second_done)
        .spawn()
        .unwrap();
    wait_for_path(&second_started);
    thread::sleep(Duration::from_millis(150));
    assert!(!first_done.exists());
    assert!(
        !second_done.exists(),
        "a second process bypassed migration ownership"
    );

    fs::write(&first_release, b"release").unwrap();
    wait_for_child(first);
    wait_for_child(second);
    assert!(first_done.exists());
    assert!(second_done.exists());
    Store::open(&db_path).unwrap();
}

#[test]
fn migration_lock_is_recovered_after_owner_process_is_killed() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    downgrade_to_schema_46(&db_path);
    let ready = temp.path().join("ready");
    let never_release = temp.path().join("never-release");
    let mut owner = stage_a_helper_command("open_store", &db_path)
        .env("CTX_TEST_MIGRATION_READY", &ready)
        .env("CTX_TEST_MIGRATION_RELEASE", &never_release)
        .spawn()
        .unwrap();
    wait_for_path(&ready);
    owner.kill().unwrap();
    owner.wait().unwrap();

    let reopened = Store::open(&db_path).unwrap();
    assert_eq!(
        reopened
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        crate::SCHEMA_VERSION
    );
}

#[test]
fn journal_activation_fences_an_already_open_legacy_writer_process() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let initial = Store::open(&db_path).unwrap();
    let event_id = uuid::Uuid::new_v4();
    initial
        .conn
        .execute(
            "INSERT INTO events
             (id, seq, event_type, role, occurred_at_ms, payload_json, visibility,
              fidelity, sync_state, sync_version, metadata_json)
             VALUES (?1, 1, 'message', 'user', 1, ?2, 'local_only', 'full',
                     'local_only', 0, '{}')",
            params![event_id.to_string(), r#"{"text":"original"}"#],
        )
        .unwrap();
    initial
        .conn
        .execute_batch(
            "DROP TABLE projection_journal_entities;
             DROP TABLE projection_journal_chunks;
             DROP TABLE projection_journal_state;
             DROP TABLE ctx_store_schema_identity;
             PRAGMA user_version = 46;",
        )
        .unwrap();
    drop(initial);

    // This process opens and prepares its mutation while the database really
    // is schema 46, then remains alive across migration and activation.
    let writer_ready = temp.path().join("writer-ready");
    let writer_release = temp.path().join("writer-release");
    let writer = stage_a_helper_command("legacy_writer", &db_path)
        .env("CTX_TEST_STAGE_A_EVENT_ID", event_id.to_string())
        .env("CTX_TEST_STAGE_A_WRITER_READY", &writer_ready)
        .env("CTX_TEST_STAGE_A_WRITER_RELEASE", &writer_release)
        .spawn()
        .unwrap();
    wait_for_path(&writer_ready);

    let store = Store::open(&db_path).unwrap();
    let activated = store.activate_projection_journal(&"a".repeat(64)).unwrap();
    assert_eq!(activated.position.sequence, 1);
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'trigger' AND name LIKE 'ctx_projection_writer_fence_%'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        21
    );
    fs::write(&writer_release, b"release").unwrap();
    wait_for_child(writer);

    let payload: String = store
        .conn
        .query_row(
            "SELECT payload_json FROM events WHERE id = ?1",
            [event_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(payload, r#"{"text":"original"}"#);
    assert_eq!(
        store
            .projection_journal_snapshot(None)
            .unwrap()
            .frozen_through
            .position
            .sequence,
        1
    );

    store.disable_projection_journal().unwrap();
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'trigger' AND name LIKE 'ctx_projection_writer_fence_%'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    let legacy_after_disable = Connection::open(&db_path).unwrap();
    legacy_after_disable
        .execute(
            "UPDATE events SET payload_json = ?1 WHERE id = ?2",
            params![r#"{"text":"allowed after disable"}"#, event_id.to_string()],
        )
        .unwrap();
}

fn tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("ctx-history-store-connection-")
        .tempdir()
        .unwrap()
}

#[test]
fn writable_store_does_not_create_obsolete_spool_directory() {
    let temp = tempdir();
    let database = temp.path().join("work.sqlite");

    let _store = Store::open(&database).unwrap();

    assert!(temp.path().join(crate::object_store::OBJECTS_DIR).is_dir());
    assert!(!temp.path().join("spool").exists());
}

#[test]
fn released_spool_and_legacy_inbox_are_preserved_but_not_store_authority() {
    let temp = tempdir();
    let legacy_root = temp.path().join("work-record");
    let legacy_inbox = legacy_root.join("inbox");
    fs::create_dir_all(&legacy_inbox).unwrap();
    let legacy_data = legacy_inbox.join("legacy-fragment");
    fs::write(&legacy_data, b"legacy non-authoritative data").unwrap();
    let released_spool = temp.path().join("spool");
    fs::create_dir_all(&released_spool).unwrap();
    let released_data = released_spool.join("released-fragment");
    fs::write(&released_data, b"released non-authoritative data").unwrap();
    drop(Connection::open(legacy_root.join("work.sqlite")).unwrap());

    let _store = Store::open(temp.path().join("work.sqlite")).unwrap();

    assert!(temp.path().join("work.sqlite").is_file());
    assert_eq!(
        fs::read(&released_data).unwrap(),
        b"released non-authoritative data"
    );
    assert_eq!(
        fs::read(&legacy_data).unwrap(),
        b"legacy non-authoritative data"
    );
}

#[test]
fn nested_write_batches_use_savepoints_and_preserve_outer_atomicity() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    store
        .conn
        .execute_batch("CREATE TABLE nested_batch_probe(value INTEGER NOT NULL)")
        .unwrap();

    store.begin_immediate_batch().unwrap();
    store
        .conn
        .execute("INSERT INTO nested_batch_probe VALUES (1)", [])
        .unwrap();
    store.begin_immediate_batch().unwrap();
    store
        .conn
        .execute("INSERT INTO nested_batch_probe VALUES (2)", [])
        .unwrap();
    store.rollback_batch().unwrap();
    store.begin_immediate_batch().unwrap();
    store
        .conn
        .execute("INSERT INTO nested_batch_probe VALUES (3)", [])
        .unwrap();
    store.commit_batch().unwrap();
    store.rollback_batch().unwrap();

    let count: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM nested_batch_probe", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);

    store.begin_immediate_batch().unwrap();
    store.begin_immediate_batch().unwrap();
    store
        .conn
        .execute("INSERT INTO nested_batch_probe VALUES (4)", [])
        .unwrap();
    store.commit_batch().unwrap();
    store.commit_batch().unwrap();
    let values: Vec<i64> = store
        .conn
        .prepare("SELECT value FROM nested_batch_probe ORDER BY value")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(values, vec![4]);
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

fn event_search_maintenance_marker(store: &Store) -> Option<i64> {
    store
        .conn
        .query_row(
            "SELECT value FROM search_projection_stats WHERE key = 'event_search_maintenance_v1'",
            [],
            |row| row.get(0),
        )
        .optional()
        .unwrap()
}

fn event_search_maintenance_groups(store: &Store) -> Option<i64> {
    store
        .conn
        .query_row(
            "SELECT value FROM search_projection_stats WHERE key = 'event_search_maintenance_v1:groups'",
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
fn bulk_search_mode_recovers_on_reopen_and_restores_saved_config() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open(&db_path).unwrap();
    for table in ["event_search", "event_search_scriptgram"] {
        set_fts_config(&store, table, "automerge", 8);
        set_fts_config(&store, table, "crisismerge", 32);
    }

    let guard = store.begin_event_search_bulk_mode_eager().unwrap();
    assert_eq!(bulk_mode_marker(&store), Some(1));
    for table in ["event_search", "event_search_scriptgram"] {
        assert_eq!(fts_config(&store, table, "automerge", 4), 0);
        assert_eq!(fts_config(&store, table, "crisismerge", 16), 16);
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
fn unused_lazy_bulk_mode_is_a_physical_store_noop() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open(&db_path).unwrap();
    let observer = Connection::open(&db_path).unwrap();
    let data_version = observer
        .query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))
        .unwrap();
    let total_changes = store.conn.total_changes();
    let database_bytes = fs::read(&db_path).unwrap();
    let wal_path = db_path.with_extension("sqlite-wal");
    let wal_bytes = fs::read(&wal_path).ok();

    let guard = store.begin_event_search_bulk_mode().unwrap();
    assert_eq!(bulk_mode_marker(&store), None);
    store.finish_event_search_bulk_mode(&guard).unwrap();
    drop(guard);

    assert_eq!(store.conn.total_changes(), total_changes);
    assert_eq!(
        observer
            .query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        data_version
    );
    assert_eq!(fs::read(&db_path).unwrap(), database_bytes);
    assert_eq!(fs::read(&wal_path).ok(), wal_bytes);
}

#[test]
fn overlapping_bulk_search_mode_is_rejected_until_guard_releases() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let first = Store::open(&db_path).unwrap();
    let guard = first.begin_event_search_bulk_mode_eager().unwrap();
    let second = Store::open_with_busy_timeout(&db_path, Duration::from_millis(10)).unwrap();

    let error = second.begin_event_search_bulk_mode().err().unwrap();
    assert!(matches!(error, StoreError::BulkSearchImportBusy));
    assert_eq!(bulk_mode_marker(&second), Some(1));
    for table in ["event_search", "event_search_scriptgram"] {
        assert_eq!(fts_config(&second, table, "automerge", 4), 0);
        assert_eq!(fts_config(&second, table, "crisismerge", 16), 16);
    }

    first.finish_event_search_bulk_mode(&guard).unwrap();
    drop(guard);
    let next_guard = second.begin_event_search_bulk_mode().unwrap();
    second.finish_event_search_bulk_mode(&next_guard).unwrap();
}

#[test]
fn overlapping_source_inventory_is_rejected_until_guard_releases() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let first = Store::open(&db_path).unwrap();
    let guard = first.acquire_source_inventory_lock().unwrap();
    let second = Store::open_with_busy_timeout(&db_path, Duration::from_millis(10)).unwrap();

    let error = second.acquire_source_inventory_lock().err().unwrap();
    assert!(matches!(error, StoreError::SourceInventoryBusy));

    drop(guard);
    let next_guard = second.acquire_source_inventory_lock().unwrap();
    drop(next_guard);
}

#[test]
fn nested_bulk_search_mode_finishes_only_at_outer_scope() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let first = Store::open(&db_path).unwrap();
    let outer = first.begin_event_search_bulk_mode_eager().unwrap();
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
fn nested_bulk_search_mode_enforces_the_outer_wal_bound() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open(&db_path).unwrap();
    let outer = store.begin_event_search_bulk_mode_eager().unwrap();
    insert_bulk_search_events(&store, "nested-wal-bound", 8, 32);
    let wal_path = format!("{}-wal", db_path.display());
    assert!(
        std::fs::metadata(&wal_path).unwrap().len() > 1,
        "test setup must grow the outer operation's WAL"
    );
    let _limits = Store::event_search_bulk_test_limits(Some(1), None);

    let nested = store.begin_event_search_bulk_mode().unwrap();

    assert_eq!(std::fs::metadata(&wal_path).unwrap().len(), 0);
    drop(nested);
    store.finish_event_search_bulk_mode(&outer).unwrap();
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
fn bulk_search_finish_defers_compaction_without_delaying_search_visibility() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open(&db_path).unwrap();
    let guard = store.begin_event_search_bulk_mode_eager().unwrap();

    let mut peak_wal_bytes = 0;
    for index in 0..8 {
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
    assert_eq!(segments, 8, "bulk writes should remain unmerged");
    assert!(
        peak_wal_bytes <= 4 * 1024 * 1024,
        "bulk FTS writes grew WAL to {peak_wal_bytes} bytes"
    );

    store.finish_event_search_bulk_mode(&guard).unwrap();
    assert_eq!(bulk_mode_marker(&store), None);
    assert_eq!(event_search_maintenance_marker(&store), Some(1));
    assert_eq!(event_search_maintenance_groups(&store), Some(1));
    let deferred_segments = store
        .conn
        .query_row(
            "SELECT COUNT(DISTINCT segid) FROM event_search_idx",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(deferred_segments, segments);
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM event_search WHERE event_search MATCH 'bulk'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        8
    );

    drop(guard);
    store.optimize_search_index().unwrap();
    assert_eq!(event_search_segment_count(&store), 1);
    assert_eq!(event_search_maintenance_marker(&store), None);
    assert_eq!(event_search_maintenance_groups(&store), None);
}

#[test]
fn repeated_bulk_groups_accumulate_durable_debt_without_strict_finalization() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open(&db_path).unwrap();

    for index in 0..7 {
        let guard = store.begin_event_search_bulk_mode_eager().unwrap();
        insert_bulk_search_events(&store, &format!("deferred-{index}"), 1, 8);
        store.finish_event_search_bulk_mode(&guard).unwrap();
        drop(guard);
    }

    assert_eq!(event_search_segment_count(&store), 7);
    assert_eq!(event_search_maintenance_marker(&store), Some(1));
    assert_eq!(event_search_maintenance_groups(&store), Some(7));
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM event_search WHERE event_search MATCH 'deferred'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        7
    );
}

#[test]
fn bulk_admission_rechecks_wal_after_due_maintenance_with_pinned_reader() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open_with_busy_timeout(&db_path, Duration::from_millis(10)).unwrap();
    let preparation = store.begin_event_search_bulk_mode_eager().unwrap();
    insert_bulk_search_events(&store, "wal-maintenance-prep", 8, 32);
    store.finish_event_search_bulk_mode(&preparation).unwrap();
    drop(preparation);
    set_event_search_maintenance_groups(&store, 8);
    store.checkpoint_wal_truncate_required().unwrap();

    let reader = Connection::open(&db_path).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    assert_eq!(
        reader
            .query_row("SELECT COUNT(*) FROM event_search", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        8
    );
    let _limits = Store::event_search_bulk_test_limits(Some(1), None);

    let error = store
        .begin_event_search_bulk_mode_eager()
        .err()
        .expect("post-maintenance WAL admission must fail");
    assert!(matches!(
        error,
        StoreError::WalCheckpointBusy {
            log_frames,
            checkpointed_frames,
        } if log_frames > checkpointed_frames
    ));
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM event_search WHERE event_id = 'post-maintenance-admission'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .conn
            .query_row("SELECT COUNT(*) FROM sync_cursors", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );

    reader.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn bulk_admission_audits_legacy_segments_without_maintenance_debt() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open(&db_path).unwrap();
    insert_bulk_search_events(&store, "legacy-segment", 1, 8);
    assert_eq!(event_search_segment_count(&store), 1);
    assert_eq!(event_search_maintenance_marker(&store), None);
    let _limits = Store::event_search_bulk_test_limits(None, Some(1));

    let error = store
        .begin_event_search_bulk_mode_eager()
        .err()
        .expect("legacy segment guard admission must fail");
    assert!(matches!(
        error,
        StoreError::EventSearchSegmentLimit {
            table: "event_search",
            segments: 1,
            guard: 1,
            hard_limit: 2_000,
        }
    ));
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM event_search WHERE event_id = 'post-segment-admission'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .conn
            .query_row("SELECT COUNT(*) FROM sync_cursors", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
}

#[test]
fn writable_reopen_runs_only_due_or_stale_bulk_maintenance_slice() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open(&db_path).unwrap();
    let guard = store.begin_event_search_bulk_mode_eager().unwrap();
    insert_bulk_search_events(&store, "reopen-debt", 8, 8);
    store.finish_event_search_bulk_mode(&guard).unwrap();
    drop(guard);
    let deferred_segments = event_search_segment_count(&store);
    assert!(deferred_segments > 1);
    assert_eq!(event_search_maintenance_groups(&store), Some(1));

    Store::reset_event_search_maintenance_slice_calls_for_test();
    drop(store);
    let reopened = Store::open(&db_path).unwrap();
    assert_eq!(Store::event_search_maintenance_slice_calls_for_test(), 0);
    assert_eq!(event_search_maintenance_groups(&reopened), Some(1));
    assert_eq!(event_search_segment_count(&reopened), deferred_segments);

    set_event_search_maintenance_groups(&reopened, 8);
    Store::reset_event_search_maintenance_slice_calls_for_test();
    drop(reopened);
    let due_reopen = Store::open(&db_path).unwrap();
    assert_eq!(Store::event_search_maintenance_slice_calls_for_test(), 1);
    assert_ne!(event_search_maintenance_groups(&due_reopen), Some(8));
    assert!(event_search_segment_count(&due_reopen) < deferred_segments);
}

#[test]
fn bulk_search_crisis_guard_prevents_github_181_segment_exhaustion() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open(&db_path).unwrap();
    let guard = store.begin_event_search_bulk_mode_eager().unwrap();

    assert_eq!(fts_config(&store, "event_search", "automerge", 4), 0);
    assert_eq!(fts_config(&store, "event_search", "crisismerge", 16), 16);
    insert_bulk_search_events(&store, "crisis-safe", 80, 8);

    let segments = event_search_segment_count(&store);
    assert!(
        segments < 80,
        "the safe crisis threshold should merge before every insert becomes a segment: {segments}"
    );
    assert!(segments < 2_000);
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM event_search WHERE event_search MATCH 'crisis'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        80
    );

    store.finish_event_search_bulk_mode(&guard).unwrap();
}

#[test]
fn segment_guard_reports_fts_limit_instead_of_disk_full() {
    let error =
        Store::event_search_segment_guard_diagnostic_for_test("event_search", 1_024).unwrap_err();
    assert!(matches!(
        &error,
        StoreError::EventSearchSegmentLimit {
            table: "event_search",
            segments: 1_024,
            guard: 1_024,
            hard_limit: 2_000,
        }
    ));
    let diagnostic = error.to_string();
    assert!(diagnostic.contains("FTS5 segment guard"));
    assert!(diagnostic.contains("not evidence that the disk is full"));
    Store::event_search_segment_guard_diagnostic_for_test("event_search", 1_023).unwrap();
}

#[test]
fn bulk_search_finish_preserves_preexisting_optimized_segment() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open(&db_path).unwrap();

    let first_guard = store.begin_event_search_bulk_mode_eager().unwrap();
    insert_bulk_search_events(&store, "historic", 80, 512);
    store.finish_event_search_bulk_mode(&first_guard).unwrap();
    drop(first_guard);
    store.optimize_search_index().unwrap();
    assert_eq!(event_search_segment_count(&store), 1);

    let second_guard = store.begin_event_search_bulk_mode_eager().unwrap();
    insert_bulk_search_events(&store, "new", 8, 128);
    let before_finish = event_search_segment_count(&store);
    assert_eq!(before_finish, 9);
    store.finish_event_search_bulk_mode(&second_guard).unwrap();

    assert_eq!(
        event_search_segment_count(&store),
        before_finish,
        "finishing one provider group must not synchronously rewrite the historical index"
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
        88
    );
}

#[test]
fn bulk_search_recovery_resumes_legacy_in_progress_full_merge() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open(&db_path).unwrap();
    let guard = store.begin_event_search_bulk_mode_eager().unwrap();
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

fn set_event_search_maintenance_groups(store: &Store, groups: i64) {
    store
        .conn
        .execute(
            "UPDATE search_projection_stats SET value = ?1 WHERE key = 'event_search_maintenance_v1:groups'",
            params![groups],
        )
        .unwrap();
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
fn pinned_reader_does_not_block_publication_or_bounded_recovery() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let store = Store::open_with_busy_timeout(&db_path, Duration::from_millis(10)).unwrap();
    let guard = store.begin_event_search_bulk_mode_eager().unwrap();
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
    assert_eq!(bulk_mode_marker(&store), None);
    assert_eq!(event_search_maintenance_marker(&store), Some(1));
    drop(store);
    drop(guard);

    let reopened = Store::open(&db_path).unwrap();
    assert_eq!(bulk_mode_marker(&reopened), None);
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
    reader.execute_batch("ROLLBACK").unwrap();
    drop(reader);
    reopened.optimize_search_index().unwrap();
    assert_eq!(event_search_segment_count(&reopened), 1);
}

#[cfg(windows)]
#[test]
fn windows_store_replaces_permissive_inheritance_on_all_state_roots() {
    use std::{ffi::OsString, fs, path::PathBuf, process::Command};

    use ctx_history_core::platform_security::{verify_private_directory, verify_private_file};

    let parent = tempdir();
    let status = Command::new("icacls.exe")
        .arg(parent.path())
        .args(["/grant", "*S-1-1-0:(OI)(CI)F"])
        .status()
        .unwrap();
    assert!(status.success());
    let root = parent.path().join("custom-ctx-root");
    let database = root.join("ctx.db");
    let store = Store::open(&database).unwrap();
    store
        .conn
        .execute_batch("CREATE TABLE acl_probe (value INTEGER); INSERT INTO acl_probe VALUES (1)")
        .unwrap();

    verify_private_directory(&root).unwrap();
    verify_private_directory(&root.join(crate::object_store::OBJECTS_DIR)).unwrap();
    assert!(!root.join("spool").exists());
    verify_private_file(&database).unwrap();
    for suffix in ["-wal", "-shm"] {
        let mut value = OsString::from(database.as_os_str());
        value.push(suffix);
        let path = PathBuf::from(value);
        if fs::symlink_metadata(&path).is_ok() {
            verify_private_file(&path).unwrap();
        }
    }
}

/// `Store::with_atomic_write` is the choke point that keeps a NativePath
/// group's authorizer enforceable while the group retains the prepared
/// statements its typed mutations use.
///
/// SQLite evaluates an authorizer's write decision once, during
/// `sqlite3_prepare_v2`, and bakes the result into the statement. A statement
/// prepared inside the group's typed write scope therefore carries an "allow"
/// decision that must never be replayed outside it. Repeating the *same*
/// canonical mutation out of route is the exact case where a stale decision
/// could be reused, so it must still be denied and must poison the group.
#[test]
fn unowned_repeat_of_a_cached_group_mutation_is_denied_and_poisons_the_group() {
    use ctx_history_core::{
        CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind, SyncMetadata,
    };

    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let started_at: chrono::DateTime<chrono::Utc> = "2026-07-25T00:00:00Z".parse().unwrap();
    let source = CaptureSource {
        id: uuid::Uuid::from_u128(0x5150),
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Codex,
            machine_id: "machine".to_owned(),
            process_id: Some(42),
            cwd: Some("/repo".to_owned()),
            raw_source_path: Some("/repo/session.jsonl".to_owned()),
            source_format: Some("codex-jsonl".to_owned()),
            source_root: Some("/repo".to_owned()),
            source_identity: Some("source-authorizer".to_owned()),
            external_session_id: Some("session-authorizer".to_owned()),
        },
        started_at,
        ended_at: None,
        sync: SyncMetadata::default(),
    };

    let guard = store.begin_event_search_bulk_mode().unwrap();
    let admission = store.admit_event_search_bulk_group(&guard).unwrap();
    let coordinator = crate::NativePathGroupAccounting::new(1, 1, 64).unwrap();
    let mut group = store
        .begin_native_path_publication_group(admission, coordinator)
        .unwrap();

    // Caches the canonical capture-source upsert under an "allow" decision.
    group.upsert_capture_source(&source).unwrap();

    // Same Store operation, same SQL, now outside the group's typed surface.
    let unowned = CaptureSource {
        started_at: started_at + chrono::TimeDelta::seconds(1),
        ..source.clone()
    };
    assert!(store.upsert_capture_source(&unowned).is_err());

    assert!(matches!(
        group.commit(),
        Err(StoreError::NativePathGroupPoisoned)
    ));
    assert!(matches!(
        store.get_capture_source(source.id),
        Err(StoreError::NotFound(_))
    ));
    store.finish_event_search_bulk_mode(&guard).unwrap();
}
