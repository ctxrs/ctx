use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs::{self, File, FileTimes, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::Path,
    sync::{Arc, Barrier},
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::fs::{symlink, PermissionsExt};

use ctx_history_core::ScannedSourceCounts;
use rusqlite::{config::DbConfig, params, Connection};
use sha2::{Digest, Sha256};

use super::{
    open_root_handle_sqlite_source_physical_revision, open_root_handle_sqlite_source_snapshot,
    open_root_handle_sqlite_source_snapshot_after_parent_certification_for_test,
    open_root_handle_sqlite_source_snapshot_for_test, retain_sqlite_source_directory_authority,
    SqliteLogicalSnapshot, SqliteSourceAccessError, SqliteSourceComponent,
    SqliteSourceDirectoryAuthority, SqliteSourceReadSnapshot, SqliteSourceSnapshotStrategy,
    SQLITE_SHM_MAX_BYTES, SQLITE_SNAPSHOT_MAX_COMPONENT_BYTES,
};

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

fn retain_parent(path: &Path) -> SqliteSourceDirectoryAuthority {
    let parent = File::open(path).unwrap();
    retain_sqlite_source_directory_authority(crate::test_provider_sqlite_data_root(), &parent, path)
        .unwrap()
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
fn active_wal_physical_replay_hashes_exact_components_and_opens_no_sqlite_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let _writer = create_persistent_wal(&database);
    let cold_parent = retain_parent(temp.path());
    let cold = open_root_handle_sqlite_source_snapshot(&cold_parent, OsStr::new("provider.sqlite"))
        .unwrap();
    let committed_physical_revision = *cold.evidence().physical_revision();
    cold.finish().unwrap();

    let before = directory_file_bytes(temp.path());
    let database_length = fs::metadata(&database).unwrap().len();
    let wal_length = fs::metadata(database.with_file_name("provider.sqlite-wal"))
        .unwrap()
        .len();
    let replay_parent = retain_parent(temp.path());
    let replay = open_root_handle_sqlite_source_physical_revision(
        &replay_parent,
        OsStr::new("provider.sqlite"),
    )
    .unwrap();
    assert_eq!(replay.revision(), &committed_physical_revision);
    replay.mark_replay_hit().unwrap();
    replay.revalidate().unwrap();
    assert_eq!(directory_file_bytes(temp.path()), before);

    let counters = replay_parent.snapshot_counters();
    assert_eq!(counters.physical_revision_captures(), 1);
    assert_eq!(counters.physical_replay_hits(), 1);
    assert_eq!(counters.physical_database_bytes_read(), database_length * 3);
    assert_eq!(counters.physical_wal_bytes_read(), wal_length * 3);
    assert_eq!(counters.immutable_snapshot_opens(), 0);
    assert_eq!(counters.copied_snapshot_opens(), 0);
    assert_eq!(counters.source_bytes_copied(), 0);
    assert_eq!(counters.logical_rows_scanned(), 0);
    assert_eq!(counters.terminal_fences(), 1);
    assert_eq!(counters.terminal_revalidations(), 1);
    assert_eq!(counters.active_snapshots(), 0);
}

#[test]
fn physical_replay_ignores_shared_memory_churn() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let _writer = create_persistent_wal(&database);
    let shared_memory = database.with_file_name("provider.sqlite-shm");
    let database_length = fs::metadata(&database).unwrap().len();
    let wal_length = fs::metadata(database.with_file_name("provider.sqlite-wal"))
        .unwrap()
        .len();
    let parent = retain_parent(temp.path());
    let before =
        open_root_handle_sqlite_source_physical_revision(&parent, OsStr::new("provider.sqlite"))
            .unwrap();
    let expected_revision = *before.revision();

    let mut file = OpenOptions::new().write(true).open(&shared_memory).unwrap();
    file.seek(SeekFrom::Start(16 * 1024)).unwrap();
    file.write_all(b"changed-shm").unwrap();
    file.sync_all().unwrap();

    before.revalidate().unwrap();
    let after =
        open_root_handle_sqlite_source_physical_revision(&parent, OsStr::new("provider.sqlite"))
            .unwrap();
    assert_eq!(after.revision(), &expected_revision);
    after.mark_replay_hit().unwrap();
    after.revalidate().unwrap();
    let counters = parent.snapshot_counters();
    assert_eq!(counters.physical_revision_captures(), 2);
    assert_eq!(counters.physical_replay_hits(), 1);
    assert_eq!(counters.physical_database_bytes_read(), database_length * 6);
    assert_eq!(counters.physical_wal_bytes_read(), wal_length * 6);
    assert_eq!(counters.copied_snapshot_opens(), 0);
    assert_eq!(counters.logical_rows_scanned(), 0);
}

#[test]
fn physical_replay_fails_if_wal_commits_before_publication() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let writer = create_persistent_wal(&database);
    let parent = retain_parent(temp.path());
    let replay =
        open_root_handle_sqlite_source_physical_revision(&parent, OsStr::new("provider.sqlite"))
            .unwrap();

    writer
        .execute(
            "INSERT INTO messages (body) VALUES ('before-publication')",
            [],
        )
        .unwrap();

    assert!(matches!(
        replay.revalidate(),
        Err(SqliteSourceAccessError::SourceChanged)
    ));
    let counters = parent.snapshot_counters();
    assert_eq!(counters.physical_revision_captures(), 1);
    assert_eq!(counters.physical_replay_hits(), 0);
    assert_eq!(counters.terminal_revalidations(), 0);
    assert_eq!(counters.copied_snapshot_opens(), 0);
}

#[cfg(unix)]
#[test]
fn physical_revision_detects_wal_create_checkpoint_reset_and_delete() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "before-wal");
    let parent = retain_parent(temp.path());
    let without_wal =
        open_root_handle_sqlite_source_physical_revision(&parent, OsStr::new("provider.sqlite"))
            .unwrap();

    let writer = Connection::open(&database).unwrap();
    writer.pragma_update(None, "journal_mode", "wal").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    writer
        .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
        .unwrap();
    writer
        .execute("INSERT INTO messages (body) VALUES ('in-wal')", [])
        .unwrap();
    assert!(without_wal.revalidate().is_err());
    let with_wal =
        open_root_handle_sqlite_source_physical_revision(&parent, OsStr::new("provider.sqlite"))
            .unwrap();
    assert_ne!(with_wal.revision(), without_wal.revision());

    writer
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    assert!(with_wal.revalidate().is_err());
    let after_reset =
        open_root_handle_sqlite_source_physical_revision(&parent, OsStr::new("provider.sqlite"))
            .unwrap();
    assert_ne!(after_reset.revision(), with_wal.revision());

    drop(writer);
    let wal = database.with_file_name("provider.sqlite-wal");
    if wal.exists() {
        fs::remove_file(&wal).unwrap();
    }
    assert!(after_reset.revalidate().is_err());
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
        result,
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

    assert!(matches!(
        result,
        Err(SqliteSourceAccessError::SourceChanged)
    ));
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
        2,
        "acquisition keeps only the post-pin and final evidence fences"
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

#[test]
fn oversized_snapshot_component_is_typed_unavailable_before_copy() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let file = File::create(&database).unwrap();
    file.set_len(SQLITE_SNAPSHOT_MAX_COMPONENT_BYTES + 1)
        .unwrap();
    fs::write(
        database.with_file_name("provider.sqlite-shm"),
        b"force bounded copied-family acquisition",
    )
    .unwrap();
    let parent = retain_parent(temp.path());

    let result = open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite"));
    assert!(matches!(
        result,
        Err(SqliteSourceAccessError::SnapshotTooLarge { .. })
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

#[cfg(any(unix, windows))]
#[test]
fn same_size_rewrite_with_restored_mtime_fails_physical_replay() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "expected");
    let parent = retain_parent(temp.path());
    let replay =
        open_root_handle_sqlite_source_physical_revision(&parent, OsStr::new("provider.sqlite"))
            .unwrap();

    let before = fs::metadata(&database).unwrap();
    let modified = before.modified().unwrap();
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&database)
        .unwrap();
    file.seek(SeekFrom::Start(100)).unwrap();
    file.write_all(b"changed!").unwrap();
    file.sync_all().unwrap();
    file.set_times(FileTimes::new().set_modified(modified))
        .unwrap();
    let after = fs::metadata(&database).unwrap();
    assert_eq!(after.len(), before.len());
    assert_eq!(after.modified().unwrap(), modified);

    assert!(matches!(
        replay.revalidate(),
        Err(SqliteSourceAccessError::SourceChanged)
    ));
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
    let first_physical_revision = *first.evidence().revision();
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
    assert_ne!(second.evidence().revision(), &first_physical_revision);
    assert_eq!(second_logical, first_logical);
    second.finish().unwrap();
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

#[cfg(unix)]
#[test]
fn leaf_swap_between_admission_and_stock_open_is_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let admitted = temp.path().join("admitted.sqlite");
    let attacker = temp.path().join("attacker.sqlite");
    create_database(&database, "expected");
    create_database(&attacker, "attacker");
    let parent = retain_parent(temp.path());

    let result = open_root_handle_sqlite_source_snapshot_for_test(
        &parent,
        OsStr::new("provider.sqlite"),
        || {
            fs::rename(&database, &admitted).unwrap();
            fs::rename(&attacker, &database).unwrap();
        },
    );
    assert!(matches!(
        result,
        Err(SqliteSourceAccessError::SourceChanged)
    ));
}

#[cfg(unix)]
#[test]
fn symlink_database_is_rejected_before_sqlite_open() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("target.sqlite");
    let link = temp.path().join("provider.sqlite");
    create_database(&target, "target");
    symlink(&target, &link).unwrap();
    let parent = retain_parent(temp.path());

    let result = open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite"));
    assert!(matches!(
        result,
        Err(SqliteSourceAccessError::UnsafeFile { .. })
    ));
}

#[cfg(unix)]
#[test]
fn symlink_sidecar_is_rejected_before_sqlite_open() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let target = temp.path().join("outside-wal");
    create_database(&database, "expected");
    fs::write(&target, b"not a WAL").unwrap();
    symlink(&target, database.with_file_name("provider.sqlite-wal")).unwrap();
    let parent = retain_parent(temp.path());

    let result = open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite"));
    assert!(matches!(
        result,
        Err(SqliteSourceAccessError::UnsafeFile { .. })
    ));
}

#[test]
fn nonregular_database_is_rejected_before_sqlite_open() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join("provider.sqlite")).unwrap();
    let parent = retain_parent(temp.path());

    let result = open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite"));
    assert!(matches!(
        result,
        Err(SqliteSourceAccessError::UnsafeFile { .. })
    ));
}

#[test]
fn nonregular_sidecar_is_rejected_before_sqlite_open() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "expected");
    fs::create_dir(database.with_file_name("provider.sqlite-shm")).unwrap();
    let parent = retain_parent(temp.path());

    let result = open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite"));
    assert!(matches!(
        result,
        Err(SqliteSourceAccessError::UnsafeFile { .. })
    ));
}

#[test]
fn rollback_journal_is_typed_unavailable_without_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "expected");
    fs::write(
        database.with_file_name("provider.sqlite-journal"),
        b"not recovered",
    )
    .unwrap();
    let parent = retain_parent(temp.path());

    let physical =
        open_root_handle_sqlite_source_physical_revision(&parent, OsStr::new("provider.sqlite"));
    assert!(matches!(
        physical,
        Err(SqliteSourceAccessError::UnsupportedSidecarIdentity {
            component: SqliteSourceComponent::RollbackJournal,
            ..
        })
    ));
    let result = open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite"));
    assert!(matches!(
        result,
        Err(SqliteSourceAccessError::UnsupportedSidecarIdentity {
            component: SqliteSourceComponent::RollbackJournal,
            ..
        })
    ));
}
