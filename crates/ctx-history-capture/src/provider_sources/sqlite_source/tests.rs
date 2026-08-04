use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{self, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Barrier},
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::fs::{symlink, PermissionsExt};

use ctx_history_core::ScannedSourceCounts;
use rusqlite::{config::DbConfig, ffi, params, Connection};
use sha2::{Digest, Sha256};

use crate::provider::source_backed::SourceBackedCurrentSourceProgressStage;

use super::{
    certify_root_handle_sqlite_source_snapshot_copy_budget_for_test, map_revalidation_error,
    map_revalidation_io_error, open_root_handle_sqlite_source_online_backup_after_backup_for_test,
    open_root_handle_sqlite_source_online_backup_after_database_copy_for_test,
    open_root_handle_sqlite_source_online_backup_after_private_source_copy_for_test,
    open_root_handle_sqlite_source_online_backup_before_identity_check_for_test,
    open_root_handle_sqlite_source_online_backup_with_scratch_limit_for_test,
    open_root_handle_sqlite_source_snapshot,
    open_root_handle_sqlite_source_snapshot_after_database_copy_for_test,
    open_root_handle_sqlite_source_snapshot_after_parent_certification_for_test,
    open_root_handle_sqlite_source_snapshot_for_test, retain_sqlite_source_directory_authority,
    run_online_backup_with_deadline_for_test, SqliteArtifactKind, SqliteCleanupStatus,
    SqliteFailurePhase, SqliteLogicalSnapshot, SqliteRetryDecision, SqliteSourceAccessError,
    SqliteSourceComponent, SqliteSourceDirectoryAuthority, SqliteSourceReadSnapshot,
    SqliteSourceSnapshotStrategy, SQLITE_SHM_MAX_BYTES, SQLITE_SNAPSHOT_MAX_TOTAL_BYTES,
};

mod copy_races;
mod corruption_cleanup;
mod diagnostics;
mod path_safety;
mod scratch;

fn create_database(path: &Path, value: &str) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute("CREATE TABLE messages (body TEXT NOT NULL)", [])
        .unwrap();
    connection
        .execute("INSERT INTO messages (body) VALUES (?1)", params![value])
        .unwrap();
}

fn create_persistent_wal(path: &Path) -> Connection {
    let connection = Connection::open(path).unwrap();
    let mode: String = connection
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    connection
        .execute("CREATE TABLE messages (body TEXT NOT NULL)", [])
        .unwrap();
    connection
        .execute("INSERT INTO messages (body) VALUES ('from-wal')", [])
        .unwrap();
    connection
        .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
        .unwrap();
    connection
}

fn create_large_persistent_wal(path: &Path) -> Connection {
    create_database(path, "before-wal");
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE copy_window_padding(payload BLOB NOT NULL);
             INSERT INTO copy_window_padding VALUES (zeroblob(10485760));",
        )
        .unwrap();
    let mode: String = connection
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    connection
        .execute_batch("PRAGMA wal_autocheckpoint=0")
        .unwrap();
    connection
        .execute("INSERT INTO messages (body) VALUES ('from-wal')", [])
        .unwrap();
    connection
}

fn retain_parent(path: &Path) -> SqliteSourceDirectoryAuthority {
    retain_parent_in_data_root(crate::test_provider_sqlite_data_root(), path)
}

fn retain_parent_in_data_root(data_root: &Path, path: &Path) -> SqliteSourceDirectoryAuthority {
    let parent = File::open(path).unwrap();
    retain_sqlite_source_directory_authority(data_root, &parent, path).unwrap()
}

fn read_values(snapshot: &SqliteSourceReadSnapshot) -> Vec<String> {
    snapshot
        .connection()
        .unwrap()
        .prepare("SELECT body FROM messages ORDER BY rowid")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

fn logical_message_snapshot(snapshot: &SqliteSourceReadSnapshot) -> SqliteLogicalSnapshot {
    let values = read_values(snapshot);
    let mut digest = Sha256::new();
    let mut certified_bytes = 0_u64;
    for value in &values {
        digest.update((value.len() as u64).to_le_bytes());
        digest.update(value.as_bytes());
        certified_bytes += value.len() as u64;
    }
    SqliteLogicalSnapshot::new(
        "shared-sqlite-test-v1",
        b"messages(body TEXT NOT NULL)",
        digest.finalize().into(),
        ScannedSourceCounts {
            complete_records: values.len() as u64,
            retained_records: values.len() as u64,
            rejected_records: 0,
            ignored_records: 0,
            indexed_documents: values.len() as u64,
            certified_bytes,
        },
    )
}

fn directory_file_bytes(path: &Path) -> BTreeMap<OsString, Vec<u8>> {
    fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (entry.file_name(), fs::read(entry.path()).unwrap())
        })
        .collect()
}

fn directory_names(path: &Path) -> BTreeSet<OsString> {
    fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect()
}

#[test]
fn stock_sqlite_initial_snapshot_succeeds_with_idle_wal_writer() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "before-wal");
    let writer = Connection::open(&database).unwrap();
    let mode: String = writer
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    writer.execute_batch("PRAGMA wal_autocheckpoint=0").unwrap();
    let wal = database.with_file_name("provider.sqlite-wal");
    assert!(
        !wal.exists(),
        "the idle writer must not have materialized a WAL pathname"
    );
    let before = directory_file_bytes(temp.path());
    let parent = retain_parent(temp.path());

    let snapshot =
        open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite")).unwrap();
    assert_eq!(read_values(&snapshot), ["before-wal"]);
    #[cfg(target_os = "linux")]
    {
        assert_eq!(
            snapshot.strategy(),
            SqliteSourceSnapshotStrategy::ImmutableMain
        );
        assert_eq!(snapshot.copied_bytes(), 0);
        let counters = parent.snapshot_counters();
        assert_eq!(counters.immutable_snapshot_opens(), 1);
        assert_eq!(counters.copied_snapshot_opens(), 0);
        assert_eq!(counters.source_bytes_copied(), 0);
        assert_eq!(counters.active_snapshots(), 1);
        assert_eq!(counters.active_snapshot_bytes(), 0);
        assert_eq!(counters.max_active_snapshots(), 1);
        assert_eq!(counters.max_active_snapshot_bytes(), 0);
    }
    #[cfg(not(target_os = "linux"))]
    {
        assert_eq!(
            snapshot.strategy(),
            SqliteSourceSnapshotStrategy::CopiedFamily
        );
        assert!(snapshot.copied_bytes() > 0);
    }
    assert_eq!(snapshot.evidence().wal_length(), None);
    let fence = snapshot.seal().unwrap();
    assert_eq!(fence.evidence().wal_length(), None);
    fence.revalidate().unwrap();
    assert!(!wal.exists());
    assert!(!database.with_file_name("provider.sqlite-shm").exists());
    assert_eq!(directory_file_bytes(temp.path()), before);

    drop(writer);
}

#[test]
fn stock_sqlite_reads_active_wal_read_only_and_query_only() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let writer = create_persistent_wal(&database);
    let wal = database.with_file_name("provider.sqlite-wal");
    let shared_memory = database.with_file_name("provider.sqlite-shm");
    let before_database = fs::read(&database).unwrap();
    let before_wal = fs::read(&wal).unwrap();
    let before_shared_memory = fs::read(&shared_memory).unwrap();
    let before_directory = directory_file_bytes(temp.path());
    let parent = retain_parent(temp.path());

    let snapshot =
        open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite")).unwrap();
    assert_eq!(read_values(&snapshot), ["from-wal"]);
    assert_eq!(
        snapshot.strategy(),
        SqliteSourceSnapshotStrategy::CopiedFamily
    );
    assert_eq!(
        snapshot.copied_bytes(),
        u64::try_from(before_database.len() + before_wal.len()).unwrap()
    );
    assert!(snapshot.snapshot_directory().unwrap().starts_with(
        crate::test_provider_sqlite_data_root()
            .join("tmp")
            .join("provider-sqlite")
    ));
    let snapshot_directory = snapshot.snapshot_directory().unwrap().to_path_buf();
    assert_eq!(
        snapshot
            .connection()
            .unwrap()
            .pragma_query_value(None, "query_only", |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        snapshot
            .connection()
            .unwrap()
            .pragma_query_value(None, "temp_store", |row| row.get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert!(snapshot
        .connection()
        .unwrap()
        .execute("INSERT INTO messages (body) VALUES ('forbidden')", [])
        .is_err());
    assert!(
        snapshot
            .connection()
            .unwrap()
            .execute_batch("COMMIT")
            .is_err(),
        "provider consumers may not end the guard-owned transaction"
    );
    assert!(snapshot.evidence().wal_length().is_some());
    assert!(snapshot.evidence().shared_memory_length().is_some());
    let counters = parent.snapshot_counters();
    assert_eq!(counters.immutable_snapshot_opens(), 0);
    assert_eq!(counters.copied_snapshot_opens(), 1);
    assert_eq!(
        counters.source_bytes_copied(),
        u64::try_from(before_database.len() + before_wal.len()).unwrap()
    );
    assert_eq!(counters.active_snapshots(), 1);
    assert_eq!(counters.active_snapshot_bytes(), snapshot.copied_bytes());
    assert_eq!(counters.max_active_snapshots(), 1);
    assert_eq!(
        counters.max_active_snapshot_bytes(),
        snapshot.copied_bytes()
    );
    let fence = snapshot.seal().unwrap();

    assert!(!snapshot_directory.exists());
    let counters = parent.snapshot_counters();
    assert_eq!(counters.terminal_fences(), 1);
    assert_eq!(counters.terminal_revalidations(), 1);
    assert_eq!(counters.active_snapshots(), 0);
    assert_eq!(counters.active_snapshot_bytes(), 0);
    fence.revalidate().unwrap();
    assert_eq!(parent.snapshot_counters().terminal_revalidations(), 2);
    assert_eq!(fs::read(&database).unwrap(), before_database);
    assert_eq!(fs::read(&wal).unwrap(), before_wal);
    assert_eq!(fs::read(&shared_memory).unwrap(), before_shared_memory);
    assert_eq!(directory_file_bytes(temp.path()), before_directory);
    assert_eq!(
        writer
            .query_row("SELECT count(*) FROM messages", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn shared_snapshot_policies_keep_cross_provider_temp_work_off_the_filesystem() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("ctx-data");
    for (name, logical) in [("strict-provider", false), ("logical-provider", true)] {
        let provider = temp.path().join(name);
        fs::create_dir_all(&provider).unwrap();
        let database = provider.join("provider.sqlite");
        create_database(&database, name);
        let before = directory_file_bytes(&provider);
        let authority = retain_parent_in_data_root(&data_root, &provider);
        let snapshot = if logical {
            authority
                .open_logical_online_backup_snapshot(OsStr::new("provider.sqlite"))
                .unwrap()
        } else {
            open_root_handle_sqlite_source_snapshot(&authority, OsStr::new("provider.sqlite"))
                .unwrap()
        };
        let connection = snapshot.connection().unwrap();
        assert_eq!(
            connection
                .pragma_query_value(None, "temp_store", |row| row.get::<_, i64>(0))
                .unwrap(),
            2
        );
        let values = connection
            .prepare(
                "with recursive generated(value) as (
                     values(4096) union all select value - 1 from generated where value > 1
                 )
                 select value from generated order by printf('%08d', value)",
            )
            .unwrap()
            .query_map([], |row| row.get::<_, i64>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(values.len(), 4096);
        snapshot.finish().unwrap();
        assert_eq!(directory_file_bytes(&provider), before);
    }
}

#[test]
fn finish_publishes_retained_terminal_revalidator() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "retained");
    let parent = retain_parent(temp.path());

    let snapshot =
        open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite")).unwrap();
    let revalidate = snapshot.terminal_revalidator();
    assert!(matches!(
        revalidate(),
        Err(SqliteSourceAccessError::SnapshotNotActive)
    ));

    snapshot.finish().unwrap();
    revalidate().unwrap();
    let counters = parent.snapshot_counters();
    assert_eq!(
        counters.immutable_snapshot_opens(),
        u64::from(cfg!(target_os = "linux"))
    );
    assert_eq!(
        counters.copied_snapshot_opens(),
        u64::from(!cfg!(target_os = "linux"))
    );
    assert_eq!(counters.terminal_fences(), 1);
    assert_eq!(counters.terminal_revalidations(), 2);
}

#[test]
fn copied_wal_snapshot_keeps_missing_provider_shm_missing() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let writer = create_persistent_wal(&database);
    drop(writer);
    let wal = database.with_file_name("provider.sqlite-wal");
    let shared_memory = database.with_file_name("provider.sqlite-shm");
    fs::remove_file(&shared_memory).unwrap();
    let before_database = fs::read(&database).unwrap();
    let before_wal = fs::read(&wal).unwrap();
    let parent = retain_parent(temp.path());

    let snapshot =
        open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite")).unwrap();
    assert_eq!(read_values(&snapshot), ["from-wal"]);
    assert_eq!(
        snapshot.strategy(),
        SqliteSourceSnapshotStrategy::CopiedFamily
    );
    assert_eq!(
        snapshot.copied_bytes(),
        u64::try_from(before_database.len() + before_wal.len()).unwrap()
    );
    let snapshot_directory = snapshot.snapshot_directory().unwrap().to_path_buf();
    let evidence = snapshot.finish().unwrap();
    assert!(evidence.wal_length().is_some());

    assert!(!snapshot_directory.exists());
    assert!(!shared_memory.exists());
    assert_eq!(fs::read(&database).unwrap(), before_database);
    assert_eq!(fs::read(&wal).unwrap(), before_wal);
    let counters = parent.snapshot_counters();
    assert_eq!(counters.copied_snapshot_opens(), 1);
    assert_eq!(counters.terminal_fences(), 1);
    assert_eq!(counters.terminal_revalidations(), 1);
    assert_eq!(counters.active_snapshots(), 0);
    assert_eq!(counters.active_snapshot_bytes(), 0);
}

#[cfg(unix)]
#[test]
fn active_wal_snapshot_reads_a_read_only_provider_tree() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let writer = create_persistent_wal(&database);
    drop(writer);
    let wal = database.with_file_name("provider.sqlite-wal");
    let shared_memory = database.with_file_name("provider.sqlite-shm");
    for path in [&database, &wal, &shared_memory] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o444)).unwrap();
    }
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o555)).unwrap();
    let before = directory_file_bytes(temp.path());
    let parent = retain_parent(temp.path());

    let snapshot =
        open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite")).unwrap();
    assert_eq!(read_values(&snapshot), ["from-wal"]);
    assert_eq!(
        snapshot.strategy(),
        SqliteSourceSnapshotStrategy::CopiedFamily
    );
    let fence = snapshot.seal().unwrap();
    fence.revalidate().unwrap();
    assert_eq!(directory_file_bytes(temp.path()), before);

    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o755)).unwrap();
    for path in [&database, &wal, &shared_memory] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
    }
}

#[test]
fn sidecar_creation_during_immutable_open_is_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let wal = database.with_file_name("provider.sqlite-wal");
    create_database(&database, "expected");
    let parent = retain_parent(temp.path());

    let result = open_root_handle_sqlite_source_snapshot_for_test(
        &parent,
        OsStr::new("provider.sqlite"),
        || fs::write(&wal, b"appeared during acquisition").unwrap(),
    );

    assert!(matches!(
        &result,
        Err(SqliteSourceAccessError::SourceChanged)
    ));
}

#[cfg(unix)]
#[test]
fn wal_deletion_during_copied_acquisition_is_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let _writer = create_persistent_wal(&database);
    let wal = database.with_file_name("provider.sqlite-wal");
    let parent = retain_parent(temp.path());

    let result = open_root_handle_sqlite_source_snapshot_for_test(
        &parent,
        OsStr::new("provider.sqlite"),
        || fs::remove_file(&wal).unwrap(),
    );

    assert!(result.unwrap_err().is_source_changed());
}

#[test]
fn bounded_active_wal_copy_has_one_retained_snapshot_lifecycle() {
    const PAYLOAD_BYTES: usize = 8 * 1024 * 1024;

    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let writer = Connection::open(&database).unwrap();
    let mode: String = writer
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    writer.execute_batch("PRAGMA wal_autocheckpoint=0").unwrap();
    writer
        .execute("CREATE TABLE payloads (body BLOB NOT NULL)", [])
        .unwrap();
    writer
        .execute(
            "INSERT INTO payloads (body) VALUES (zeroblob(?1))",
            [PAYLOAD_BYTES],
        )
        .unwrap();
    writer
        .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
        .unwrap();
    let wal = database.with_file_name("provider.sqlite-wal");
    let expected_copied =
        fs::metadata(&database).unwrap().len() + fs::metadata(&wal).unwrap().len();
    let parent = retain_parent(temp.path());

    let started = Instant::now();
    let snapshot =
        open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite")).unwrap();
    for _ in 0..2 {
        let length: i64 = snapshot
            .connection()
            .unwrap()
            .query_row("SELECT length(body) FROM payloads", [], |row| row.get(0))
            .unwrap();
        assert_eq!(length, PAYLOAD_BYTES as i64);
    }
    assert_eq!(snapshot.copied_bytes(), expected_copied);
    assert_eq!(
        snapshot.family_revalidation_count(),
        4,
        "copied acquisition fences the DB/WAL family before and after the copy, then post-pin and final evidence"
    );
    let counters = parent.snapshot_counters();
    assert_eq!(counters.copied_snapshot_opens(), 1);
    assert_eq!(counters.source_bytes_copied(), expected_copied);
    assert_eq!(counters.active_snapshots(), 1);
    assert_eq!(counters.max_active_snapshots(), 1);
    assert_eq!(counters.max_active_snapshot_bytes(), expected_copied);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "an 8 MiB active-WAL snapshot exceeded the focused sanity bound"
    );
    let fence = snapshot.seal().unwrap();
    fence.revalidate().unwrap();
    let counters = parent.snapshot_counters();
    assert_eq!(counters.copied_snapshot_opens(), 1);
    assert_eq!(counters.source_bytes_copied(), expected_copied);
    assert_eq!(counters.terminal_fences(), 1);
    assert_eq!(counters.terminal_revalidations(), 2);
    assert_eq!(counters.active_snapshots(), 0);
    assert_eq!(counters.active_snapshot_bytes(), 0);
}

#[cfg(unix)]
#[test]
fn snapshot_budget_admits_reported_large_main_plus_one_main_sized_wal() {
    use std::os::unix::fs::MetadataExt;

    const MIB: u64 = 1024 * 1024;
    const REPORTED_MAIN_BYTES: u64 = 943 * MIB;
    const BOUNDED_WAL_BYTES: u64 = REPORTED_MAIN_BYTES;
    const FORMER_TOTAL_LIMIT: u64 = 1024 * MIB;
    const SELECTED_TOTAL_LIMIT: u64 = 2 * 1024 * MIB;

    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let wal = database.with_file_name("provider.sqlite-wal");
    create_database(&database, "large-valid-main");
    OpenOptions::new()
        .write(true)
        .open(&database)
        .unwrap()
        .set_len(REPORTED_MAIN_BYTES)
        .unwrap();
    File::create(&wal)
        .unwrap()
        .set_len(BOUNDED_WAL_BYTES)
        .unwrap();
    for (path, expected_length) in [(&database, REPORTED_MAIN_BYTES), (&wal, BOUNDED_WAL_BYTES)] {
        let metadata = fs::metadata(path).unwrap();
        assert_eq!(metadata.len(), expected_length);
        assert!(
            metadata.blocks().saturating_mul(512) < MIB,
            "the GiB-scale regression fixture must remain physically sparse"
        );
    }
    let expected_total = REPORTED_MAIN_BYTES + BOUNDED_WAL_BYTES;
    assert_eq!(SQLITE_SNAPSHOT_MAX_TOTAL_BYTES, SELECTED_TOTAL_LIMIT);
    assert!(expected_total > FORMER_TOTAL_LIMIT);
    assert!(expected_total <= SQLITE_SNAPSHOT_MAX_TOTAL_BYTES);
    let parent = retain_parent(temp.path());

    let admitted = certify_root_handle_sqlite_source_snapshot_copy_budget_for_test(
        &parent,
        OsStr::new("provider.sqlite"),
    )
    .unwrap();
    assert_eq!(admitted, expected_total);
}

#[test]
fn snapshot_budget_rejects_cumulative_main_plus_wal_over_total() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "over-total");
    OpenOptions::new()
        .write(true)
        .open(&database)
        .unwrap()
        .set_len(SQLITE_SNAPSHOT_MAX_TOTAL_BYTES)
        .unwrap();
    fs::write(database.with_file_name("provider.sqlite-wal"), b"x").unwrap();
    let parent = retain_parent(temp.path());

    let result = open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite"));
    assert!(matches!(
        result,
        Err(SqliteSourceAccessError::SnapshotTooLarge {
            length,
            maximum: SQLITE_SNAPSHOT_MAX_TOTAL_BYTES,
            ..
        }) if length == SQLITE_SNAPSHOT_MAX_TOTAL_BYTES + 1
    ));
    assert_eq!(parent.snapshot_counters().source_bytes_copied(), 0);
}

#[test]
fn snapshot_copy_fails_closed_on_database_mutation_during_family_copy() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let _writer = create_persistent_wal(&database);
    assert!(
        fs::metadata(database.with_file_name("provider.sqlite-wal"))
            .unwrap()
            .len()
            > 0
    );
    let original_length = fs::metadata(&database).unwrap().len();
    let parent = retain_parent(temp.path());

    let result = open_root_handle_sqlite_source_snapshot_after_database_copy_for_test(
        &parent,
        OsStr::new("provider.sqlite"),
        || {
            let mut file = OpenOptions::new().write(true).open(&database).unwrap();
            file.seek(SeekFrom::End(-8)).unwrap();
            file.write_all(b"mutation").unwrap();
            file.sync_all().unwrap();
            assert_eq!(file.metadata().unwrap().len(), original_length);
        },
    );

    assert!(matches!(
        result,
        Err(SqliteSourceAccessError::SourceChanged)
    ));
}

#[test]
fn shared_memory_rewrite_during_copied_acquisition_is_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let shared_memory = database.with_file_name("provider.sqlite-shm");
    create_database(&database, "expected");
    fs::write(&shared_memory, vec![0_u8; 32 * 1024]).unwrap();
    let parent = retain_parent(temp.path());

    let result = open_root_handle_sqlite_source_snapshot_for_test(
        &parent,
        OsStr::new("provider.sqlite"),
        || {
            let mut file = OpenOptions::new().write(true).open(&shared_memory).unwrap();
            file.seek(SeekFrom::Start(16 * 1024)).unwrap();
            file.write_all(b"changed-shm").unwrap();
            file.sync_all().unwrap();
        },
    );

    assert!(matches!(
        result,
        Err(SqliteSourceAccessError::SourceChanged)
    ));
}

#[test]
fn oversized_shared_memory_is_typed_unavailable_before_hashing() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let shared_memory = database.with_file_name("provider.sqlite-shm");
    create_database(&database, "expected");
    let file = File::create(&shared_memory).unwrap();
    file.set_len(SQLITE_SHM_MAX_BYTES + 1).unwrap();
    let parent = retain_parent(temp.path());

    let result = open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite"));

    assert!(matches!(
        result,
        Err(SqliteSourceAccessError::SnapshotTooLarge { .. })
    ));
}

#[test]
fn logical_online_backup_enforces_the_shared_memory_bound_before_opening_sqlite() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let shared_memory = database.with_file_name("provider.sqlite-shm");
    create_database(&database, "expected");
    let file = File::create(&shared_memory).unwrap();
    file.set_len(SQLITE_SHM_MAX_BYTES + 1).unwrap();
    let parent = retain_parent(temp.path());

    let result = parent.open_logical_online_backup_snapshot(OsStr::new("provider.sqlite"));

    assert!(matches!(
        result,
        Err(SqliteSourceAccessError::SnapshotTooLarge {
            length,
            maximum: SQLITE_SHM_MAX_BYTES,
            ..
        }) if length == SQLITE_SHM_MAX_BYTES + 1
    ));
    assert_eq!(parent.snapshot_counters().logical_online_backup_opens(), 0);
}

#[test]
fn logical_online_backup_checks_an_expired_deadline_before_a_bounded_step() {
    let temp = tempfile::tempdir().unwrap();
    let source_path = temp.path().join("source.sqlite");
    let destination_path = temp.path().join("destination.sqlite");
    create_database(&source_path, "expected");
    let source =
        Connection::open_with_flags(&source_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let destination = Connection::open(&destination_path).unwrap();

    let result = run_online_backup_with_deadline_for_test(&source, &destination, Instant::now());

    assert!(matches!(
        result,
        Err(SqliteSourceAccessError::SnapshotUnavailable { reason })
            if reason.contains("five-minute deadline")
    ));
}

#[test]
fn active_source_family_contract_sqlite_keeps_a_pinned_view_and_fails_changed_writer_generation() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let writer = create_persistent_wal(&database);
    let parent = retain_parent(temp.path());
    let snapshot =
        open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite")).unwrap();
    assert_eq!(read_values(&snapshot), ["from-wal"]);
    let snapshot_directory = snapshot.snapshot_directory().unwrap().to_path_buf();

    writer
        .execute("INSERT INTO messages (body) VALUES ('later')", [])
        .unwrap();
    assert_eq!(read_values(&snapshot), ["from-wal"]);
    assert!(matches!(
        snapshot.seal(),
        Err(SqliteSourceAccessError::SourceChanged)
    ));
    assert!(!snapshot_directory.exists());
    let counters = parent.snapshot_counters();
    assert_eq!(counters.copied_snapshot_opens(), 1);
    assert_eq!(counters.terminal_fences(), 0);
    assert_eq!(counters.active_snapshots(), 0);
    assert_eq!(counters.active_snapshot_bytes(), 0);

    let replacement =
        open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite")).unwrap();
    assert_eq!(read_values(&replacement), ["from-wal", "later"]);
    replacement.finish().unwrap();
}

#[test]
fn committed_wal_write_during_stock_open_is_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let writer = create_persistent_wal(&database);
    let parent = retain_parent(temp.path());

    let result = open_root_handle_sqlite_source_snapshot_for_test(
        &parent,
        OsStr::new("provider.sqlite"),
        || {
            writer
                .execute("INSERT INTO messages (body) VALUES ('during-open')", [])
                .unwrap();
        },
    );

    assert!(matches!(
        result,
        Err(SqliteSourceAccessError::SourceChanged)
    ));
}

#[test]
fn direct_wal_truncate_is_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let _writer = create_persistent_wal(&database);
    let wal = database.with_file_name("provider.sqlite-wal");
    let parent = retain_parent(temp.path());
    let snapshot =
        open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite")).unwrap();
    assert_eq!(read_values(&snapshot), ["from-wal"]);

    let file = OpenOptions::new().write(true).open(&wal).unwrap();
    file.set_len(0).unwrap();
    file.sync_all().unwrap();

    assert!(snapshot.finish().is_err());
}

#[test]
fn source_revision_changes_after_a_committed_wal_generation() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let writer = create_persistent_wal(&database);
    let parent = retain_parent(temp.path());
    let first =
        open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite")).unwrap();
    let first_revision = *first.evidence().revision();
    first.finish().unwrap();

    writer
        .execute("INSERT INTO messages (body) VALUES ('next')", [])
        .unwrap();
    let second =
        open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite")).unwrap();
    assert_ne!(second.evidence().revision(), &first_revision);
    second.finish().unwrap();
}

#[test]
fn direct_database_rewrite_is_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "expected");
    let parent = retain_parent(temp.path());
    let snapshot =
        open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite")).unwrap();
    assert_eq!(read_values(&snapshot), ["expected"]);

    let mut file = OpenOptions::new().append(true).open(&database).unwrap();
    file.write_all(b"rewrite evidence").unwrap();
    file.sync_all().unwrap();
    assert!(matches!(
        snapshot.finish(),
        Err(SqliteSourceAccessError::SourceChanged)
    ));
}

#[test]
fn mutation_after_seal_fails_retained_terminal_revalidation() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let writer = create_persistent_wal(&database);
    let before = directory_file_bytes(temp.path());
    let parent = retain_parent(temp.path());
    let snapshot =
        open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite")).unwrap();
    assert_eq!(read_values(&snapshot), ["from-wal"]);
    let snapshot_directory = snapshot.snapshot_directory().unwrap().to_path_buf();
    let fence = snapshot.seal().unwrap();
    assert!(!snapshot_directory.exists());
    assert_eq!(directory_file_bytes(temp.path()), before);

    writer
        .execute("INSERT INTO messages (body) VALUES ('after-seal')", [])
        .unwrap();
    assert!(matches!(
        fence.revalidate(),
        Err(SqliteSourceAccessError::SourceChanged)
    ));
    let counters = parent.snapshot_counters();
    assert_eq!(counters.copied_snapshot_opens(), 1);
    assert_eq!(counters.terminal_fences(), 1);
    assert_eq!(
        counters.terminal_revalidations(),
        1,
        "failed terminal checks do not count as successful fences"
    );
    assert_eq!(counters.active_snapshots(), 0);
    assert_eq!(counters.active_snapshot_bytes(), 0);
}

#[test]
fn authority_local_counters_track_two_overlapping_copied_snapshots() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<super::SqliteSourceTerminalFence>();

    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let _writer = create_persistent_wal(&database);
    let wal = database.with_file_name("provider.sqlite-wal");
    let expected_bytes = fs::metadata(&database).unwrap().len() + fs::metadata(&wal).unwrap().len();
    let parent = Arc::new(retain_parent(temp.path()));
    let independent = retain_parent(temp.path());
    let opened = Arc::new(Barrier::new(3));
    let release = Arc::new(Barrier::new(3));
    let mut workers = Vec::new();

    for _ in 0..2 {
        let parent = Arc::clone(&parent);
        let opened = Arc::clone(&opened);
        let release = Arc::clone(&release);
        workers.push(thread::spawn(move || {
            let snapshot =
                open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite"))
                    .unwrap();
            assert_eq!(read_values(&snapshot), ["from-wal"]);
            opened.wait();
            release.wait();
            snapshot.seal().unwrap()
        }));
    }

    opened.wait();
    let counters = parent.snapshot_counters();
    assert_eq!(counters.copied_snapshot_opens(), 2);
    assert_eq!(counters.source_bytes_copied(), expected_bytes * 2);
    assert_eq!(counters.active_snapshots(), 2);
    assert_eq!(counters.active_snapshot_bytes(), expected_bytes * 2);
    assert_eq!(counters.max_active_snapshots(), 2);
    assert_eq!(counters.max_active_snapshot_bytes(), expected_bytes * 2);
    assert_eq!(
        independent.snapshot_counters(),
        super::SqliteSourceSnapshotCounters::default(),
        "separately retained authorities do not share process-global counters"
    );
    release.wait();

    let fences = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    let counters = parent.snapshot_counters();
    assert_eq!(counters.terminal_fences(), 2);
    assert_eq!(counters.terminal_revalidations(), 2);
    assert_eq!(counters.active_snapshots(), 0);
    assert_eq!(counters.active_snapshot_bytes(), 0);
    for fence in fences {
        fence.revalidate().unwrap();
    }
    assert_eq!(parent.snapshot_counters().terminal_revalidations(), 4);
}

#[test]
fn logical_snapshot_ignores_wal_growth_when_rows_are_unchanged() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let writer = create_persistent_wal(&database);
    let parent = retain_parent(temp.path());

    let first =
        open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite")).unwrap();
    let first_logical = logical_message_snapshot(&first);
    let first_source_revision = *first.evidence().revision();
    first.finish().unwrap();

    writer
        .execute("UPDATE messages SET body = 'transient'", [])
        .unwrap();
    writer
        .execute("UPDATE messages SET body = 'from-wal'", [])
        .unwrap();

    let second =
        open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite")).unwrap();
    let second_logical = logical_message_snapshot(&second);
    assert_ne!(second.evidence().revision(), &first_source_revision);
    assert_eq!(second_logical, first_logical);
    second.finish().unwrap();
}

#[test]
fn logical_online_backup_is_one_private_snapshot_without_provider_content_or_name_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let _writer = create_persistent_wal(&database);
    fs::write(temp.path().join("unrelated-provider-state"), b"unchanged").unwrap();
    let before_directory = directory_file_bytes(temp.path());
    for expected in [
        "provider.sqlite",
        "provider.sqlite-wal",
        "provider.sqlite-shm",
        "unrelated-provider-state",
    ] {
        assert!(before_directory.contains_key(OsStr::new(expected)));
    }
    let parent = retain_parent(temp.path());

    let snapshot = parent
        .open_logical_online_backup_snapshot(OsStr::new("provider.sqlite"))
        .unwrap();
    assert_eq!(
        snapshot.strategy(),
        SqliteSourceSnapshotStrategy::LogicalOnlineBackup
    );
    assert_eq!(read_values(&snapshot), vec!["from-wal"]);
    let certification = snapshot.certification().unwrap();
    assert_eq!(certification.source.pages, certification.backup.pages);
    assert_eq!(certification.source.bytes, certification.backup.bytes);
    assert!(certification.source.elapsed_ms < 5 * 60 * 1_000);
    assert!(certification.backup.elapsed_ms < 5 * 60 * 1_000);
    let counters = parent.snapshot_counters();
    assert_eq!(counters.immutable_snapshot_opens(), 0);
    assert_eq!(counters.copied_snapshot_opens(), 0);
    assert_eq!(counters.logical_online_backup_opens(), 1);
    assert!(counters.source_bytes_copied() > 0);
    assert!(counters.logical_online_backup_bytes() > 0);
    assert_eq!(counters.active_snapshots(), 1);
    assert_eq!(counters.max_active_snapshots(), 1);

    let fence = snapshot.seal().unwrap();
    fence.revalidate().unwrap();
    assert_eq!(parent.snapshot_counters().active_snapshots(), 0);
    assert_eq!(directory_file_bytes(temp.path()), before_directory);
}

#[test]
fn logical_online_backup_progress_is_monotonic_and_terminal() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let _writer = create_persistent_wal(&database);
    let parent = retain_parent(temp.path());
    let mut progress = Vec::new();

    let snapshot = parent
        .open_logical_online_backup_snapshot_with_progress(
            OsStr::new("provider.sqlite"),
            |update| {
                progress.push(update);
                Ok::<(), std::convert::Infallible>(())
            },
        )
        .unwrap();
    let copied_bytes = snapshot.copied_bytes();
    snapshot.finish().unwrap();

    let backup = progress
        .iter()
        .filter(|update| update.stage == SourceBackedCurrentSourceProgressStage::OnlineBackup)
        .collect::<Vec<_>>();
    assert!(backup.len() >= 2);
    assert_eq!(backup[0].snapshot_pages_completed, Some(0));
    assert_eq!(backup[0].snapshot_bytes_completed, Some(0));
    assert!(backup.windows(2).all(|pair| {
        pair[0].snapshot_pages_completed <= pair[1].snapshot_pages_completed
            && pair[0].snapshot_bytes_completed <= pair[1].snapshot_bytes_completed
            && pair[0].snapshot_pages_total == pair[1].snapshot_pages_total
            && pair[0].snapshot_bytes_total == pair[1].snapshot_bytes_total
    }));
    let terminal = backup.last().unwrap();
    assert_eq!(
        terminal.snapshot_pages_completed,
        terminal.snapshot_pages_total
    );
    assert_eq!(
        terminal.snapshot_bytes_completed,
        terminal.snapshot_bytes_total
    );
    assert_eq!(terminal.snapshot_bytes_total, Some(copied_bytes));
}

#[cfg(target_os = "linux")]
#[test]
fn sidecar_free_logical_snapshot_has_one_near_limit_scratch_allocation() {
    const PAYLOAD_BYTES: usize = 4 * 1024 * 1024;

    let provider = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = provider.path().join("provider.sqlite");
    let connection = Connection::open(&database).unwrap();
    connection
        .execute("CREATE TABLE payloads (body BLOB NOT NULL)", [])
        .unwrap();
    connection
        .execute(
            "INSERT INTO payloads (body) VALUES (zeroblob(?1))",
            [PAYLOAD_BYTES],
        )
        .unwrap();
    drop(connection);
    let scratch_limit = fs::metadata(&database).unwrap().len();
    let parent = retain_parent_in_data_root(data_root.path(), provider.path());

    let snapshot = open_root_handle_sqlite_source_online_backup_with_scratch_limit_for_test(
        &parent,
        OsStr::new("provider.sqlite"),
        scratch_limit,
    )
    .unwrap();

    assert_eq!(
        snapshot
            .connection()
            .unwrap()
            .query_row("SELECT length(body) FROM payloads", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        PAYLOAD_BYTES as i64
    );
    assert_eq!(snapshot.copied_bytes(), scratch_limit);
    let snapshot_directory = snapshot.snapshot_directory().unwrap();
    let retained_scratch_bytes = fs::read_dir(snapshot_directory)
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .sum::<u64>();
    assert_eq!(retained_scratch_bytes, scratch_limit);
    assert_eq!(
        directory_names(snapshot_directory),
        [OsString::from("source.sqlite")].into()
    );

    let counters = parent.snapshot_counters();
    assert_eq!(counters.source_bytes_copied(), 0);
    assert_eq!(counters.logical_online_backup_bytes(), scratch_limit);
    assert_eq!(counters.active_snapshot_bytes(), scratch_limit);
    assert_eq!(counters.max_active_snapshot_bytes(), scratch_limit);
    let observed_peak_scratch = counters
        .source_bytes_copied()
        .checked_add(counters.max_active_snapshot_bytes())
        .unwrap();
    assert_eq!(observed_peak_scratch, scratch_limit);
    assert!(observed_peak_scratch <= scratch_limit);

    let staging_root = data_root.path().join("tmp").join("provider-sqlite");
    assert_eq!(fs::read_dir(&staging_root).unwrap().count(), 1);
    let fence = snapshot.seal().unwrap();
    drop(fence);
    assert_eq!(fs::read_dir(staging_root).unwrap().count(), 0);
}

#[test]
fn copied_wal_family_is_backed_up_and_source_staging_is_cleaned() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let writer = create_persistent_wal(&database);
    drop(writer);
    let wal = database.with_file_name("provider.sqlite-wal");
    let shared_memory = database.with_file_name("provider.sqlite-shm");
    fs::remove_file(shared_memory).unwrap();
    let expected_copy_bytes =
        fs::metadata(&database).unwrap().len() + fs::metadata(&wal).unwrap().len();
    let parent = retain_parent_in_data_root(data_root.path(), temp.path());

    let snapshot = parent
        .open_logical_online_backup_snapshot(OsStr::new("provider.sqlite"))
        .unwrap();

    assert_eq!(read_values(&snapshot), ["from-wal"]);
    assert!(snapshot.copied_bytes() > 0);
    let counters = parent.snapshot_counters();
    assert_eq!(counters.source_bytes_copied(), expected_copy_bytes);
    assert_eq!(
        counters.logical_online_backup_bytes(),
        snapshot.copied_bytes()
    );
    assert_eq!(counters.active_snapshot_bytes(), snapshot.copied_bytes());
    assert_eq!(
        counters.max_active_snapshot_bytes(),
        snapshot.copied_bytes()
    );
    let measured_peak = counters
        .source_bytes_copied()
        .checked_add(counters.max_active_snapshot_bytes())
        .unwrap();
    assert!(measured_peak <= SQLITE_SNAPSHOT_MAX_TOTAL_BYTES * 2);
    let staging_root = data_root.path().join("tmp").join("provider-sqlite");
    assert_eq!(fs::read_dir(&staging_root).unwrap().count(), 1);
    snapshot.finish().unwrap();
    assert_eq!(fs::read_dir(staging_root).unwrap().count(), 0);
}

#[test]
fn logical_online_backup_keeps_admitted_content_during_commit_and_next_refresh_advances() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let writer = create_persistent_wal(&database);
    let parent = retain_parent(temp.path());

    let admitted = open_root_handle_sqlite_source_online_backup_before_identity_check_for_test(
        &parent,
        OsStr::new("provider.sqlite"),
        || {
            writer
                .execute("INSERT INTO messages (body) VALUES ('later-commit')", [])
                .unwrap();
        },
    )
    .unwrap();
    assert!(!admitted.admitted_revision_is_replay_safe());
    assert_eq!(read_values(&admitted), vec!["from-wal"]);
    let first_revision = *admitted.evidence().revision();
    let fence = admitted.seal().unwrap();
    fence.revalidate().unwrap();

    let refreshed = parent
        .open_logical_online_backup_snapshot(OsStr::new("provider.sqlite"))
        .unwrap();
    assert!(refreshed.admitted_revision_is_replay_safe());
    assert_eq!(read_values(&refreshed), vec!["from-wal", "later-commit"]);
    assert_ne!(refreshed.evidence().revision(), &first_revision);
    refreshed.finish().unwrap();
}

#[test]
fn logical_online_backup_terminal_fence_allows_same_object_wal_growth() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let writer = create_persistent_wal(&database);
    #[cfg(unix)]
    let wal = database.with_file_name("provider.sqlite-wal");
    #[cfg(unix)]
    let wal_identity = fs::metadata(&wal).unwrap();
    let parent = retain_parent(temp.path());
    let snapshot = parent
        .open_logical_online_backup_snapshot(OsStr::new("provider.sqlite"))
        .unwrap();
    assert_eq!(read_values(&snapshot), ["from-wal"]);
    let fence = snapshot.seal().unwrap();

    writer
        .execute("INSERT INTO messages (body) VALUES ('after-seal')", [])
        .unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(fs::metadata(&wal).unwrap().ino(), wal_identity.ino());
    }
    fence.revalidate().unwrap();
    let refreshed = parent
        .open_logical_online_backup_snapshot(OsStr::new("provider.sqlite"))
        .unwrap();
    assert_eq!(read_values(&refreshed), ["from-wal", "after-seal"]);
    refreshed.finish().unwrap();
}

#[test]
fn logical_online_backup_publishes_private_view_when_wal_appears_after_copy() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "before-wal");
    let writer = Connection::open(&database).unwrap();
    let mode: String = writer
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    writer.execute_batch("PRAGMA wal_autocheckpoint=0").unwrap();
    let wal = database.with_file_name("provider.sqlite-wal");
    assert!(!wal.exists());
    let parent = retain_parent(temp.path());

    let snapshot = open_root_handle_sqlite_source_online_backup_before_identity_check_for_test(
        &parent,
        OsStr::new("provider.sqlite"),
        || {
            writer
                .execute("INSERT INTO messages (body) VALUES ('new-in-wal')", [])
                .unwrap();
        },
    )
    .unwrap();
    assert_eq!(read_values(&snapshot), ["before-wal"]);
    snapshot.finish().unwrap();
    let refreshed = parent
        .open_logical_online_backup_snapshot(OsStr::new("provider.sqlite"))
        .unwrap();
    assert_eq!(read_values(&refreshed), ["before-wal", "new-in-wal"]);
    refreshed.finish().unwrap();
}

#[cfg(unix)]
#[test]
fn logical_online_backup_rejects_wal_leaf_replacement_during_named_open() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let _writer = create_persistent_wal(&database);
    let wal = database.with_file_name("provider.sqlite-wal");
    let retained_wal = database.with_file_name("provider.sqlite-retained-wal");
    let parent = retain_parent(temp.path());

    let result = open_root_handle_sqlite_source_online_backup_before_identity_check_for_test(
        &parent,
        OsStr::new("provider.sqlite"),
        || {
            fs::rename(&wal, &retained_wal).unwrap();
            fs::copy(&retained_wal, &wal).unwrap();
        },
    );

    assert!(matches!(
        result,
        Err(SqliteSourceAccessError::SourceChanged)
    ));
}

#[test]
fn logical_online_backup_rejects_checkpoint_during_copied_family_acquisition() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let writer = create_persistent_wal(&database);
    drop(writer);
    let shared_memory = database.with_file_name("provider.sqlite-shm");
    fs::remove_file(&shared_memory).unwrap();
    let parent = retain_parent(temp.path());

    let result = open_root_handle_sqlite_source_online_backup_after_database_copy_for_test(
        &parent,
        OsStr::new("provider.sqlite"),
        || {
            let checkpointer = Connection::open(&database).unwrap();
            checkpointer
                .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
                .unwrap();
        },
    );

    assert!(result.unwrap_err().is_source_changed());
}

#[cfg(unix)]
#[test]
fn logical_online_backup_terminal_fence_rejects_database_leaf_replacement() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "admitted");
    let parent = retain_parent(temp.path());
    let snapshot = parent
        .open_logical_online_backup_snapshot(OsStr::new("provider.sqlite"))
        .unwrap();
    let fence = snapshot.seal().unwrap();

    fs::rename(&database, temp.path().join("admitted.sqlite")).unwrap();
    create_database(&database, "replacement");
    assert!(matches!(
        fence.revalidate(),
        Err(SqliteSourceAccessError::SourceChanged)
    ));
}

#[cfg(unix)]
#[test]
fn logical_online_backup_terminal_fence_ignores_post_backup_wal_replacement() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let _writer = create_persistent_wal(&database);
    let wal = database.with_file_name("provider.sqlite-wal");
    let retained_wal = database.with_file_name("provider.sqlite-retained-wal");
    let parent = retain_parent(temp.path());
    let snapshot = parent
        .open_logical_online_backup_snapshot(OsStr::new("provider.sqlite"))
        .unwrap();
    let fence = snapshot.seal().unwrap();

    fs::rename(&wal, &retained_wal).unwrap();
    fs::copy(&retained_wal, &wal).unwrap();

    fence.revalidate().unwrap();
}

#[cfg(unix)]
#[test]
fn logical_online_backup_rejects_database_leaf_symlink_replacement_during_open() {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let attacker = outside.path().join("attacker.sqlite");
    create_database(&database, "admitted");
    create_database(&attacker, "attacker");
    let attacker_before = fs::read(&attacker).unwrap();
    let parent = retain_parent(temp.path());

    let result = open_root_handle_sqlite_source_online_backup_before_identity_check_for_test(
        &parent,
        OsStr::new("provider.sqlite"),
        || {
            fs::rename(&database, temp.path().join("admitted.sqlite")).unwrap();
            symlink(&attacker, &database).unwrap();
        },
    );

    assert!(matches!(
        result,
        Err(SqliteSourceAccessError::SourceChanged)
    ));
    assert_eq!(fs::read(attacker).unwrap(), attacker_before);
    assert_eq!(parent.snapshot_counters().logical_online_backup_opens(), 0);
}

#[cfg(unix)]
#[test]
fn parent_swap_after_certification_cannot_open_replacement_members() {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let approved_parent = temp.path().join("provider");
    let retained_parent = temp.path().join("retained-provider");
    let replacement_parent = temp.path().join("replacement-provider");
    fs::create_dir(&approved_parent).unwrap();
    fs::create_dir(&replacement_parent).unwrap();
    create_database(&approved_parent.join("provider.sqlite"), "expected");
    let attacker = outside.path().join("attacker.sqlite");
    fs::write(&attacker, b"attacker-controlled non-SQLite bytes").unwrap();
    symlink(&attacker, replacement_parent.join("provider.sqlite")).unwrap();
    let retained_before = directory_file_bytes(&approved_parent);
    let replacement_before = directory_file_bytes(&replacement_parent);
    let parent = retain_parent(&approved_parent);

    let result = open_root_handle_sqlite_source_snapshot_after_parent_certification_for_test(
        &parent,
        OsStr::new("provider.sqlite"),
        || {
            fs::rename(&approved_parent, &retained_parent).unwrap();
            fs::rename(&replacement_parent, &approved_parent).unwrap();
        },
    );

    assert!(matches!(
        result,
        Err(SqliteSourceAccessError::SourceChanged)
    ));
    assert_eq!(directory_file_bytes(&approved_parent), replacement_before);
    assert_eq!(directory_file_bytes(&retained_parent), retained_before);
}
