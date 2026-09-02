use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs::{self, File},
    io,
    path::{Path, PathBuf},
};

#[cfg(target_os = "linux")]
use std::{
    io::Read as _,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::fs::symlink;
#[cfg(target_os = "linux")]
use std::os::unix::fs::MetadataExt;

use rusqlite::{ffi, params, Connection};
#[cfg(target_os = "linux")]
use sha2::Digest as _;

use super::snapshot::{
    fail_next_private_directory_cleanup_for_test, fail_next_private_scratch_close_for_test,
    fail_next_private_scratch_open_for_test, fail_next_snapshot_open_for_test,
    fail_next_snapshot_write_enospc_for_test,
    open_root_handle_sqlite_source_snapshot_before_revalidation_for_test,
    open_root_handle_sqlite_source_snapshot_with_limit_for_test,
    open_root_handle_sqlite_source_stable_snapshot_after_database_copy_for_test,
    open_root_handle_sqlite_source_stable_snapshot_before_revalidation_for_test,
    planned_snapshot_copy_bytes_for_test,
};
use super::{
    fail_next_private_sqlite_staging_operation_for_test, map_revalidation_error,
    map_revalidation_io_error, open_private_sqlite_staging_file,
    open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
    SqliteArtifactKind, SqliteCleanupStatus, SqliteFailurePhase, SqliteSourceAccessError,
    SqliteSourceComponent, SqliteSourceDirectoryAuthority, SqliteSourceFamily,
    SqliteSourceProgressError, SqliteSourceProgressStage, SqliteSourceReadSnapshot,
    SqliteSourceSnapshotLimits, SqliteSourceSnapshotStrategy, SqliteSourceStagingOperationForTest,
    SQLITE_SNAPSHOT_FREE_HEADROOM_BYTES,
};

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

#[cfg(target_os = "linux")]
fn create_persistent_wal(path: &Path) -> Connection {
    use rusqlite::config::DbConfig;

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

#[cfg(target_os = "linux")]
struct PersistentWalWriterProcess {
    child: Child,
}

#[cfg(target_os = "linux")]
impl PersistentWalWriterProcess {
    fn start(database: &Path, ready: &Path) -> Self {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "sqlite_source::tests::persistent_wal_writer_process_helper",
                "--nocapture",
            ])
            .env("CTX_TEST_PROVIDER_DATABASE", database)
            .env("CTX_TEST_PROVIDER_READY", ready)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.exists() {
            if let Some(status) = child.try_wait().unwrap() {
                panic!("provider WAL writer exited before readiness: {status}");
            }
            assert!(Instant::now() < deadline, "provider WAL writer timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
        Self { child }
    }
}

#[cfg(target_os = "linux")]
impl Drop for PersistentWalWriterProcess {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        if self.child.wait().is_err() {
            let _ = self.child.kill();
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn persistent_wal_writer_process_helper() {
    let Some(database) = std::env::var_os("CTX_TEST_PROVIDER_DATABASE") else {
        return;
    };
    let ready = PathBuf::from(std::env::var_os("CTX_TEST_PROVIDER_READY").unwrap());
    let _writer = create_persistent_wal(Path::new(&database));
    fs::write(ready, b"ready").unwrap();
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input).unwrap();
}

fn retain_parent(path: &Path) -> SqliteSourceDirectoryAuthority {
    retain_parent_in_data_root(crate::test_provider_sqlite_data_root(), path)
}

fn retain_parent_in_data_root(data_root: &Path, path: &Path) -> SqliteSourceDirectoryAuthority {
    fs::create_dir_all(data_root).unwrap();
    let parent = File::open(path).unwrap();
    retain_sqlite_source_directory_authority(data_root, &parent, path).unwrap()
}

fn read_values(snapshot: &SqliteSourceReadSnapshot) -> Vec<String> {
    read_values_from_connection(snapshot.connection().unwrap())
}

fn read_values_from_connection(connection: &Connection) -> Vec<String> {
    connection
        .prepare("SELECT body FROM messages ORDER BY rowid")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

#[test]
fn physical_replay_fence_unchanged_is_zero_copy() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "first");
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());
    let fence = authority
        .observe_replay_fence(OsStr::new("provider.sqlite"))
        .unwrap();
    let revision = *fence.revision();

    fence.revalidate().unwrap();
    assert_eq!(authority.snapshot_counters(), Default::default());
    assert_eq!(staging_entries(data_root.path()), 0);
    assert_eq!(*fence.revision(), revision);
}

#[test]
fn physical_replay_revision_survives_move_but_old_fence_rejects_it() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = crate::test_support_paths::tempdir().unwrap();
    let original = temp.path().join("original");
    let moved = temp.path().join("moved");
    fs::create_dir(&original).unwrap();
    fs::create_dir(&moved).unwrap();
    create_database(&original.join("provider.sqlite"), "first");
    let original_authority = retain_parent_in_data_root(data_root.path(), &original);
    let original_fence = original_authority
        .observe_replay_fence(OsStr::new("provider.sqlite"))
        .unwrap();
    let revision = *original_fence.revision();

    fs::rename(
        original.join("provider.sqlite"),
        moved.join("provider.sqlite"),
    )
    .unwrap();
    assert!(matches!(
        original_fence.revalidate(),
        Err(SqliteSourceAccessError::SourceChanged)
    ));

    let moved_authority = retain_parent_in_data_root(data_root.path(), &moved);
    let moved_fence = moved_authority
        .observe_replay_fence(OsStr::new("provider.sqlite"))
        .unwrap();
    assert_eq!(*moved_fence.revision(), revision);
    moved_fence.revalidate().unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn physical_replay_fence_rejects_committed_wal_mutation() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let writer = create_persistent_wal(&database);
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());
    let fence = authority
        .observe_replay_fence(OsStr::new("provider.sqlite"))
        .unwrap();

    writer
        .execute("INSERT INTO messages (body) VALUES ('later')", [])
        .unwrap();

    assert!(matches!(
        fence.revalidate(),
        Err(SqliteSourceAccessError::SourceChanged)
    ));
    assert_eq!(authority.snapshot_counters(), Default::default());
    assert_eq!(staging_entries(data_root.path()), 0);
}

#[test]
fn physical_replay_fence_rejects_database_leaf_and_parent_replacement() {
    let data_root = crate::test_support_paths::tempdir().unwrap();
    let leaf_root = crate::test_support_paths::tempdir().unwrap();
    let database = leaf_root.path().join("provider.sqlite");
    create_database(&database, "first");
    let authority = retain_parent_in_data_root(data_root.path(), leaf_root.path());
    let fence = authority
        .observe_replay_fence(OsStr::new("provider.sqlite"))
        .unwrap();
    fs::rename(&database, leaf_root.path().join("retired.sqlite")).unwrap();
    create_database(&database, "first");
    assert!(matches!(
        fence.revalidate(),
        Err(SqliteSourceAccessError::SourceChanged)
    ));

    let parent_root = crate::test_support_paths::tempdir().unwrap();
    let parent = parent_root.path().join("source");
    fs::create_dir(&parent).unwrap();
    create_database(&parent.join("provider.sqlite"), "first");
    let authority = retain_parent_in_data_root(data_root.path(), &parent);
    let fence = authority
        .observe_replay_fence(OsStr::new("provider.sqlite"))
        .unwrap();
    fs::rename(&parent, parent_root.path().join("retired-source")).unwrap();
    fs::create_dir(&parent).unwrap();
    create_database(&parent.join("provider.sqlite"), "first");
    assert!(matches!(
        fence.revalidate(),
        Err(SqliteSourceAccessError::SourceChanged)
    ));
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

#[cfg(target_os = "linux")]
#[derive(Debug, PartialEq, Eq)]
struct DirectoryFileState {
    digest: [u8; 32],
    len: u64,
    mode: u32,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}

#[cfg(target_os = "linux")]
fn directory_file_state(path: &Path) -> BTreeMap<OsString, DirectoryFileState> {
    fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let metadata = entry.metadata().unwrap();
            (
                entry.file_name(),
                DirectoryFileState {
                    digest: sha2::Sha256::digest(fs::read(entry.path()).unwrap()).into(),
                    len: metadata.len(),
                    mode: metadata.mode(),
                    mtime: metadata.mtime(),
                    mtime_nsec: metadata.mtime_nsec(),
                    ctime: metadata.ctime(),
                    ctime_nsec: metadata.ctime_nsec(),
                },
            )
        })
        .collect()
}

fn staging_entries(data_root: &Path) -> usize {
    let staging = data_root.join("tmp/provider-sqlite");
    match fs::read_dir(staging) {
        Ok(entries) => entries.count(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
        Err(error) => panic!("reading SQLite staging root: {error}"),
    }
}

fn physical_error(error: &SqliteSourceAccessError) -> &SqliteSourceAccessError {
    match error {
        SqliteSourceAccessError::Diagnosed { source, .. }
        | SqliteSourceAccessError::ProviderContentCorruption { source } => physical_error(source),
        error => error,
    }
}

#[test]
fn stable_copy_is_one_private_snapshot_and_never_writes_provider_files() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "stable");
    fs::write(temp.path().join("unrelated"), b"unchanged").unwrap();
    let before = directory_file_bytes(temp.path());
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());

    let snapshot = authority
        .open_stable_snapshot(OsStr::new("provider.sqlite"))
        .unwrap();
    assert_eq!(
        snapshot.strategy(),
        SqliteSourceSnapshotStrategy::CopiedFamily
    );
    assert_eq!(read_values(&snapshot), ["stable"]);
    assert_eq!(staging_entries(data_root.path()), 1);
    let copied = snapshot.copied_bytes();
    assert_eq!(authority.snapshot_counters().source_bytes_copied(), copied);
    assert!(authority.snapshot_counters().max_route_scratch_bytes() >= copied);
    snapshot.finish().unwrap();

    assert_eq!(staging_entries(data_root.path()), 0);
    assert_eq!(directory_file_bytes(temp.path()), before);
}

#[cfg(target_os = "linux")]
#[test]
fn active_wal_retains_one_family_copy_under_one_aggregate_limit() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let writer = create_persistent_wal(&database);
    let before = directory_file_bytes(temp.path());
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());

    let snapshot = authority
        .open_stable_snapshot(OsStr::new("provider.sqlite"))
        .unwrap();
    assert_eq!(read_values(&snapshot), ["from-wal"]);
    assert_eq!(staging_entries(data_root.path()), 1);
    assert_eq!(
        snapshot.strategy(),
        SqliteSourceSnapshotStrategy::CopiedFamily
    );
    let counters = authority.snapshot_counters();
    assert_eq!(counters.copied_snapshot_opens(), 1);
    assert_eq!(counters.max_active_snapshots(), 1);
    assert!(counters.max_route_scratch_bytes() >= snapshot.copied_bytes());
    snapshot.finish().unwrap();

    assert_eq!(staging_entries(data_root.path()), 0);
    assert_eq!(directory_file_bytes(temp.path()), before);
    drop(writer);
}

#[cfg(target_os = "linux")]
#[test]
fn pinned_read_only_wal_is_zero_write_and_accepts_no_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let ready = data_root.path().join("provider-ready");
    let writer = PersistentWalWriterProcess::start(&database, &ready);
    let before = directory_file_state(temp.path());
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());

    let opened =
        authority.open_incremental_snapshot_with_progress(OsStr::new("provider.sqlite"), |_| {
            Ok::<_, ()>(())
        });
    if unsafe { libc::geteuid() } == 0 {
        assert!(matches!(
            opened,
            Err(SqliteSourceProgressError::Source(
                SqliteSourceAccessError::SnapshotUnavailable { reason }
            )) if reason.contains("effective UID 0")
        ));
        assert_eq!(directory_file_state(temp.path()), before);
        let counters = authority.snapshot_counters();
        assert_eq!(counters.pinned_read_only_wal_snapshot_opens(), 0);
        assert_eq!(counters.copied_snapshot_opens(), 0);
        assert_eq!(counters.source_bytes_copied(), 0);
        drop(writer);
        return;
    }
    let snapshot = opened.unwrap();
    assert_eq!(
        snapshot.strategy(),
        SqliteSourceSnapshotStrategy::PinnedReadOnlyWal
    );
    assert_eq!(read_values(&snapshot), ["from-wal"]);
    snapshot.finish().unwrap();
    assert_eq!(
        directory_file_state(temp.path()),
        before,
        "successful read changed the provider SQLite family"
    );

    let rejected_write_snapshot = authority
        .open_incremental_snapshot_with_progress(OsStr::new("provider.sqlite"), |_| Ok::<_, ()>(()))
        .unwrap();
    let write = rejected_write_snapshot
        .connection()
        .unwrap()
        .execute("INSERT INTO messages(body) VALUES ('forbidden')", []);
    assert!(write.is_err());
    rejected_write_snapshot.finish().unwrap();

    let counters = authority.snapshot_counters();
    assert_eq!(counters.pinned_read_only_wal_snapshot_opens(), 2);
    assert_eq!(counters.copied_snapshot_opens(), 0);
    assert_eq!(counters.source_bytes_copied(), 0);
    assert_eq!(
        directory_file_state(temp.path()),
        before,
        "rejected write changed the provider SQLite family"
    );
    drop(writer);
}

#[cfg(target_os = "linux")]
#[test]
fn pinned_read_only_wal_parent_swap_reads_retained_authority_and_fails_terminal_path() {
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let selected = temp.path().join("selected");
    let moved = temp.path().join("moved");
    fs::create_dir(&selected).unwrap();
    let original_database = selected.join("provider.sqlite");
    let original_ready = temp.path().join("original-ready");
    let original_writer = PersistentWalWriterProcess::start(&original_database, &original_ready);
    let authority = retain_parent(&selected);
    let family =
        super::family::SqliteSourceFamily::open(&authority, OsStr::new("provider.sqlite"), || {})
            .unwrap();
    let evidence = family.capture_revision_evidence().unwrap();

    fs::rename(&selected, &moved).unwrap();
    fs::create_dir(&selected).unwrap();
    let replacement_database = selected.join("provider.sqlite");
    let replacement = Connection::open(&replacement_database).unwrap();
    replacement
        .execute("CREATE TABLE messages (body TEXT NOT NULL)", [])
        .unwrap();
    replacement
        .execute(
            "INSERT INTO messages(body) VALUES ('outside-authority')",
            [],
        )
        .unwrap();

    let (connection, authority_handle) =
        super::snapshot::acquisition::open_pinned_read_only_wal(&family).unwrap();
    super::verify_connection_read_only(&connection).unwrap();
    super::configure_and_pin_snapshot(&connection).unwrap();
    let values = connection
        .prepare("SELECT body FROM messages ORDER BY rowid")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(values, ["from-wal"]);
    drop(connection);
    drop(authority_handle);
    assert!(family.revalidate_database_identity(&evidence).is_err());
    drop(replacement);
    drop(original_writer);
}

#[cfg(target_os = "linux")]
#[test]
fn pinned_read_only_wal_leaf_swap_fails_terminal_identity_revalidation() {
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let original_ready = temp.path().join("original-ready");
    let original_writer = PersistentWalWriterProcess::start(&database, &original_ready);
    let authority = retain_parent(temp.path());
    let family =
        super::family::SqliteSourceFamily::open(&authority, OsStr::new("provider.sqlite"), || {})
            .unwrap();
    let evidence = family.capture_revision_evidence().unwrap();

    let displaced = temp.path().join("displaced");
    fs::create_dir(&displaced).unwrap();
    for name in [
        "provider.sqlite",
        "provider.sqlite-wal",
        "provider.sqlite-shm",
    ] {
        fs::rename(temp.path().join(name), displaced.join(name)).unwrap();
    }
    let replacement_ready = temp.path().join("replacement-ready");
    let replacement_writer = PersistentWalWriterProcess::start(&database, &replacement_ready);
    Connection::open(&database)
        .unwrap()
        .execute("UPDATE messages SET body = 'replacement'", [])
        .unwrap();

    let (connection, authority_handle) =
        super::snapshot::acquisition::open_pinned_read_only_wal(&family).unwrap();
    super::verify_connection_read_only(&connection).unwrap();
    super::configure_and_pin_snapshot(&connection).unwrap();
    assert_eq!(read_values_from_connection(&connection), ["replacement"]);
    drop(connection);
    drop(authority_handle);
    assert!(family.revalidate_database_identity(&evidence).is_err());

    drop(replacement_writer);
    drop(original_writer);
}

#[cfg(target_os = "linux")]
#[test]
fn pinned_read_only_wal_admission_rejects_root_before_sqlite_open() {
    const MINIMUM: i32 = 3_046_000;
    let root = super::snapshot::acquisition::admit_pinned_read_only_wal(0, MINIMUM, true, MINIMUM)
        .unwrap_err();
    assert!(matches!(
        root,
        SqliteSourceAccessError::SnapshotUnavailable { reason }
            if reason.contains("effective UID 0")
    ));
    super::snapshot::acquisition::admit_pinned_read_only_wal(1_000, MINIMUM, true, MINIMUM)
        .unwrap();
    assert!(super::snapshot::acquisition::admit_pinned_read_only_wal(
        1_000,
        MINIMUM - 1,
        true,
        MINIMUM,
    )
    .is_err());
    assert!(super::snapshot::acquisition::admit_pinned_read_only_wal(
        1_000, MINIMUM, false, MINIMUM,
    )
    .is_err());
}

#[test]
fn sidecar_free_unavailable_incremental_never_copies_the_database_family() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "sidecar-free");
    assert!(!temp.path().join("provider.sqlite-wal").exists());
    assert!(!temp.path().join("provider.sqlite-shm").exists());
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());

    super::snapshot::acquisition::force_next_pinned_wal_unavailable_for_test();
    let opened =
        authority.open_incremental_snapshot_with_progress(OsStr::new("provider.sqlite"), |_| {
            Ok::<_, ()>(())
        });
    assert!(matches!(
        opened,
        Err(SqliteSourceProgressError::Source(
            SqliteSourceAccessError::SnapshotUnavailable { .. }
        ))
    ));
    let counters = authority.snapshot_counters();
    assert_eq!(counters.pinned_read_only_wal_snapshot_opens(), 0);
    assert_eq!(counters.copied_snapshot_opens(), 0);
    assert_eq!(counters.source_bytes_copied(), 0);
    assert_eq!(staging_entries(data_root.path()), 0);
}

#[cfg(target_os = "linux")]
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
fn near_limit_rejection_happens_before_any_scratch_write() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "limit");
    let database_bytes = fs::metadata(&database).unwrap().len();
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());

    let error = open_root_handle_sqlite_source_snapshot_with_limit_for_test(
        &authority,
        OsStr::new("provider.sqlite"),
        database_bytes - 1,
    )
    .unwrap_err();

    assert!(error.is_systemic_resource_failure());
    assert!(error.is_snapshot_capacity_failure());
    assert_eq!(staging_entries(data_root.path()), 0);
    assert_eq!(authority.snapshot_counters().source_bytes_copied(), 0);
}

#[test]
fn production_snapshot_admission_has_no_fixed_source_size_ceiling() {
    const TEN_GIB: u64 = 10 * 1024 * 1024 * 1024;

    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "capacity");
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());
    let limits = SqliteSourceSnapshotLimits::default();
    assert_eq!(limits.maximum_scratch_bytes(), u64::MAX);

    super::override_next_scratch_available_space_for_test(
        TEN_GIB + super::sqlite_snapshot_free_headroom_bytes(TEN_GIB),
    );
    let scratch = super::SqliteRouteScratch::new(&authority.snapshot_context, None);
    scratch.admit_capacity(TEN_GIB).unwrap();

    assert_eq!(authority.snapshot_counters().scratch_admissions(), 1);

    super::override_next_scratch_available_space_for_test(
        TEN_GIB + super::sqlite_snapshot_free_headroom_bytes(TEN_GIB) - 1,
    );
    let error = scratch.admit_capacity(TEN_GIB).unwrap_err();
    assert!(matches!(
        error,
        SqliteSourceAccessError::InsufficientScratchSpace {
            required,
            available,
            ..
        } if required == TEN_GIB + super::sqlite_snapshot_free_headroom_bytes(TEN_GIB)
            && available + 1 == required
    ));
    assert_eq!(staging_entries(data_root.path()), 0);
}

#[cfg(target_os = "linux")]
#[test]
fn multi_gibibyte_sparse_source_produces_an_available_disk_copy_plan() {
    const FIVE_GIB: u64 = 5 * 1024 * 1024 * 1024;

    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    File::create(&database).unwrap().set_len(FIVE_GIB).unwrap();
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());

    assert_eq!(
        planned_snapshot_copy_bytes_for_test(&authority, OsStr::new("provider.sqlite")).unwrap(),
        FIVE_GIB
    );
    assert_eq!(staging_entries(data_root.path()), 0);
}

#[test]
fn free_space_headroom_rejection_happens_before_any_scratch_write() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "headroom");
    let database_bytes = fs::metadata(&database).unwrap().len();
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());
    super::override_next_scratch_available_space_for_test(
        database_bytes + SQLITE_SNAPSHOT_FREE_HEADROOM_BYTES - 1,
    );

    let error = authority
        .open_stable_snapshot(OsStr::new("provider.sqlite"))
        .unwrap_err();

    assert!(matches!(
        error,
        SqliteSourceAccessError::InsufficientScratchSpace { .. }
            | SqliteSourceAccessError::Diagnosed { .. }
    ));
    assert!(error.is_snapshot_capacity_failure());
    assert_eq!(staging_entries(data_root.path()), 0);
}

#[test]
fn injected_enospc_cleans_the_single_private_directory() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "enospc");
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());
    fail_next_snapshot_write_enospc_for_test();

    let error = authority
        .open_stable_snapshot(OsStr::new("provider.sqlite"))
        .unwrap_err();

    assert!(error.is_systemic_resource_failure());
    assert_eq!(staging_entries(data_root.path()), 0);
}

#[test]
fn progress_cancellation_cleans_a_partial_family_copy() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE messages(body TEXT NOT NULL);
             INSERT INTO messages VALUES ('large');
             CREATE TABLE padding(payload BLOB NOT NULL);
             INSERT INTO padding VALUES (zeroblob(10485760));",
        )
        .unwrap();
    drop(connection);
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());
    let mut calls = 0;

    let error = authority
        .open_stable_snapshot_with_progress(OsStr::new("provider.sqlite"), |progress| {
            assert_eq!(progress.stage, SqliteSourceProgressStage::SourceFamilyCopy);
            calls += 1;
            if calls > 1 {
                Err("cancelled")
            } else {
                Ok(())
            }
        })
        .unwrap_err();

    assert!(matches!(
        error,
        SqliteSourceProgressError::Progress("cancelled")
    ));
    assert!(calls > 1);
    assert_eq!(staging_entries(data_root.path()), 0);
}

#[test]
fn progress_cancellation_preserves_simultaneous_cleanup_failure() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE messages(body TEXT NOT NULL);
             INSERT INTO messages VALUES ('large');
             CREATE TABLE padding(payload BLOB NOT NULL);
             INSERT INTO padding VALUES (zeroblob(10485760));",
        )
        .unwrap();
    drop(connection);
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());
    let mut calls = 0;
    fail_next_private_directory_cleanup_for_test();

    let error = authority
        .open_stable_snapshot_with_progress(OsStr::new("provider.sqlite"), |_| {
            calls += 1;
            if calls > 1 {
                Err("cancelled")
            } else {
                Ok(())
            }
        })
        .unwrap_err();

    match error {
        SqliteSourceProgressError::ProgressAndFinalization {
            primary,
            finalization,
        } => {
            assert_eq!(primary, "cancelled");
            assert!(finalization
                .to_string()
                .contains("injected private SQLite directory cleanup failure"));
            assert_eq!(
                finalization.diagnostic().unwrap().cleanup,
                SqliteCleanupStatus::Failed
            );
        }
        other => panic!("expected cancellation plus cleanup failure, got {other:?}"),
    }
    assert!(calls > 1);
    assert_eq!(staging_entries(data_root.path()), 0);
}

#[test]
fn snapshot_open_preserves_simultaneous_cleanup_failure() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "open failure");
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());
    fail_next_snapshot_open_for_test();
    fail_next_private_directory_cleanup_for_test();

    let error = authority
        .open_stable_snapshot(OsStr::new("provider.sqlite"))
        .unwrap_err();

    match error {
        SqliteSourceAccessError::Finalization { primary, cleanup } => {
            assert!(primary
                .to_string()
                .contains("opening the ctx-owned provider snapshot"));
            assert!(cleanup
                .to_string()
                .contains("injected private SQLite directory cleanup failure"));
        }
        other => panic!("expected open plus cleanup failure, got {other:?}"),
    }
    assert_eq!(staging_entries(data_root.path()), 0);
}

#[test]
fn acquisition_revalidation_preserves_simultaneous_cleanup_failure() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "revalidation failure");
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());
    fail_next_private_directory_cleanup_for_test();

    let error = open_root_handle_sqlite_source_stable_snapshot_before_revalidation_for_test(
        &authority,
        OsStr::new("provider.sqlite"),
        || {
            let mut source = fs::OpenOptions::new().append(true).open(&database).unwrap();
            use std::io::Write as _;
            source.write_all(&[0]).unwrap();
            source.sync_all().unwrap();
        },
    )
    .unwrap_err();

    match error {
        SqliteSourceAccessError::Finalization { primary, cleanup } => {
            assert!(primary.is_source_changed());
            assert!(cleanup
                .to_string()
                .contains("injected private SQLite directory cleanup failure"));
        }
        other => panic!("expected revalidation plus cleanup failure, got {other:?}"),
    }
    assert_eq!(staging_entries(data_root.path()), 0);
}

#[test]
fn corrupt_source_copy_fails_closed_and_cleans_scratch() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("provider.sqlite"),
        b"not a sqlite database",
    )
    .unwrap();
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());

    let result = authority.open_stable_snapshot(OsStr::new("provider.sqlite"));

    assert!(result.is_err());
    assert_eq!(staging_entries(data_root.path()), 0);
}

#[test]
fn source_race_after_database_copy_fails_closed_and_cleans_scratch() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "before");
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());
    let mut committed_user_version = None;

    let result = open_root_handle_sqlite_source_stable_snapshot_after_database_copy_for_test(
        &authority,
        OsStr::new("provider.sqlite"),
        || {
            let connection = Connection::open(&database).unwrap();
            connection.pragma_update(None, "user_version", 7).unwrap();
            committed_user_version = Some(
                connection
                    .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
            );
        },
    );

    assert_eq!(committed_user_version, Some(7));
    assert!(
        matches!(&result, Err(error) if error.is_source_changed()),
        "expected source_changed after the committed hook mutation, got {result:?}"
    );
    assert_eq!(staging_entries(data_root.path()), 0);
}

#[test]
fn database_revision_token_fails_closed_when_native_metadata_cannot_distinguish_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "before");
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());
    let family =
        SqliteSourceFamily::open(&authority, OsStr::new("provider.sqlite"), || {}).unwrap();
    let mut evidence = family.capture_evidence().unwrap();

    let connection = Connection::open(&database).unwrap();
    let journal_mode: String = connection
        .query_row("PRAGMA journal_mode=OFF", [], |row| row.get(0))
        .unwrap();
    assert_eq!(journal_mode, "off");
    connection.pragma_update(None, "user_version", 7).unwrap();
    let committed_user_version = connection
        .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
        .unwrap();
    drop(connection);

    // Model timestamp coalescing by retaining the pre-commit content token
    // while making every native database-state field equal to the new state.
    evidence.database = family.database.capture_state().unwrap();
    assert!(family.revalidate_database_identity(&evidence).is_ok());
    let result = family.revalidate(&evidence);

    assert_eq!(committed_user_version, 7);
    assert!(matches!(result, Err(error) if error.is_source_changed()));
}

#[test]
fn finish_is_mandatory_observable_and_revalidates_source_identity() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let admitted = temp.path().join("admitted.sqlite");
    create_database(&database, "expected");
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());
    let snapshot = authority
        .open_stable_snapshot(OsStr::new("provider.sqlite"))
        .unwrap();
    let terminal = snapshot.terminal_revalidator();
    fs::rename(&database, &admitted).unwrap();
    create_database(&database, "replacement");

    assert!(snapshot.finish().is_err());
    assert!(terminal().is_err());
    assert_eq!(authority.snapshot_counters().terminal_fences(), 0);
    assert_eq!(authority.snapshot_counters().unfinished_drops(), 0);
    assert_eq!(staging_entries(data_root.path()), 0);
}

#[test]
fn abort_and_unfinished_drop_are_distinct_observable_paths() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "observable");
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());

    authority
        .open_stable_snapshot(OsStr::new("provider.sqlite"))
        .unwrap()
        .abort()
        .unwrap();
    drop(
        authority
            .open_stable_snapshot(OsStr::new("provider.sqlite"))
            .unwrap(),
    );

    let counters = authority.snapshot_counters();
    assert_eq!(counters.explicit_aborts(), 1);
    assert_eq!(counters.unfinished_drops(), 1);
    assert_eq!(counters.active_snapshots(), 0);
    assert_eq!(staging_entries(data_root.path()), 0);
}

#[test]
fn retained_copy_and_ordering_database_share_one_exact_route_bound() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "aggregate");
    let database_bytes = fs::metadata(&database).unwrap().len();
    let aggregate_limit = database_bytes + 128 * 1024;
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());
    let snapshot = open_root_handle_sqlite_source_snapshot_with_limit_for_test(
        &authority,
        OsStr::new("provider.sqlite"),
        aggregate_limit,
    )
    .unwrap();

    snapshot
        .with_private_scratch_database(
            "aggregate-",
            128 * 1024,
            |scratch, _| -> Result<(), SqliteSourceAccessError> {
                scratch
                    .execute_batch(
                        "CREATE TABLE ordered(value BLOB NOT NULL);
                         INSERT INTO ordered VALUES (zeroblob(32768));",
                    )
                    .map_err(|source| {
                        SqliteSourceAccessError::private_scratch_sqlite(
                            "writing aggregate scratch fixture",
                            source,
                        )
                    })?;
                Ok(())
            },
        )
        .unwrap();
    let counters = authority.snapshot_counters();
    assert!(counters.max_route_scratch_bytes() > database_bytes);
    assert!(counters.max_route_scratch_bytes() <= aggregate_limit);
    assert_eq!(counters.scratch_admissions(), 2);
    snapshot.finish().unwrap();
}
