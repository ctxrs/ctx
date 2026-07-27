use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, MutexGuard},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use ctx_history_core::platform_security::restrict_private_directory;
use ctx_pro_host_protocol::{
    BlameMatch, BlameResult, BlameTarget, CommitBlameMatch, CommitFactType, CommitPredicate,
    FactConfidence, FactState, ResolvedBlameTarget, ResourceKind, ResourceRef,
};
use rusqlite::{params, Connection, ErrorCode};
use serde_json::json;

use super::store::usage_path;
use super::{
    read_report, reset, store, CliUsage, CompletedOperation, McpInvocation, ProOutcome, Surface,
    TargetType, ValueClass, CTX_VERSION, DEFINITION_VERSION,
};

mod schema_tests;

fn operation(name: &'static str) -> CompletedOperation {
    CompletedOperation::cli(name, true, Duration::from_millis(4))
}

fn private_tempdir() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    restrict_private_directory(root.path()).unwrap();
    root
}

fn auxiliary(path: &Path, suffix: &str) -> PathBuf {
    let mut value = OsString::from(path.as_os_str());
    value.push(suffix);
    PathBuf::from(value)
}

fn directory_bytes(path: &Path) -> Vec<(OsString, Vec<u8>)> {
    let mut entries = fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let name = entry.file_name();
            let bytes = fs::read(entry.path()).unwrap();
            (name, bytes)
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    entries
}

#[derive(Debug, PartialEq, Eq)]
struct FamilyMemberSnapshot {
    name: OsString,
    bytes: Vec<u8>,
    len: u64,
    modified: Option<SystemTime>,
    readonly: bool,
    #[cfg(unix)]
    mode: u32,
    #[cfg(unix)]
    uid: u32,
    #[cfg(unix)]
    gid: u32,
    #[cfg(unix)]
    links: u64,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

fn sqlite_family_snapshot(path: &Path) -> Vec<FamilyMemberSnapshot> {
    ["", "-wal", "-shm"]
        .into_iter()
        .filter_map(|suffix| {
            let member = if suffix.is_empty() {
                path.to_path_buf()
            } else {
                auxiliary(path, suffix)
            };
            let bytes = match fs::read(&member) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
                Err(error) => panic!("read {}: {error}", member.display()),
            };
            let metadata = member.metadata().unwrap();
            #[cfg(unix)]
            use std::os::unix::fs::MetadataExt as _;
            Some(FamilyMemberSnapshot {
                name: member.file_name().unwrap().to_os_string(),
                bytes,
                len: metadata.len(),
                modified: metadata.modified().ok(),
                readonly: metadata.permissions().readonly(),
                #[cfg(unix)]
                mode: metadata.mode(),
                #[cfg(unix)]
                uid: metadata.uid(),
                #[cfg(unix)]
                gid: metadata.gid(),
                #[cfg(unix)]
                links: metadata.nlink(),
                #[cfg(unix)]
                device: metadata.dev(),
                #[cfg(unix)]
                inode: metadata.ino(),
            })
        })
        .collect()
}

struct LocalUsageEnvGuard {
    _lock: MutexGuard<'static, ()>,
    saved: Option<OsString>,
}

impl LocalUsageEnvGuard {
    fn unset() -> Self {
        let lock = crate::config::TEST_LOCAL_USAGE_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let saved = env::var_os("CTX_LOCAL_USAGE_ENABLED");
        env::remove_var("CTX_LOCAL_USAGE_ENABLED");
        Self { _lock: lock, saved }
    }
}

impl Drop for LocalUsageEnvGuard {
    fn drop(&mut self) {
        match &self.saved {
            Some(value) => env::set_var("CTX_LOCAL_USAGE_ENABLED", value),
            None => env::remove_var("CTX_LOCAL_USAGE_ENABLED"),
        }
    }
}

fn mcp_operation(
    name: &'static str,
    success: bool,
    value_class: ValueClass,
    result_count: u64,
) -> CompletedOperation {
    CompletedOperation {
        surface: Surface::Mcp,
        operation: name,
        outcome: if success {
            super::Outcome::Success
        } else {
            super::Outcome::Failure
        },
        value_class,
        duration: super::DurationBucket::Under10Ms,
        target_type: TargetType::NotApplicable,
        pro_outcome: ProOutcome::NotApplicable,
        result_count,
        citation_count: 0,
        response_bytes: 100,
    }
}

#[cfg(unix)]
#[test]
fn report_discovery_opens_the_store_with_read_only_filesystem_access() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = private_tempdir();
    store::record(root.path(), operation("doctor")).unwrap();
    let path = usage_path(root.path());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o500)).unwrap();
    let report = read_report(root.path(), true, false);
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(report.state, "ready", "{:?}", report.error);
    assert_eq!(report.summary.unwrap().calls, 1);
}

#[test]
fn reporting_never_changes_checkpointed_or_active_sqlite_families() {
    let root = private_tempdir();
    store::record(root.path(), operation("doctor")).unwrap();
    let path = usage_path(root.path());
    assert!(!auxiliary(&path, "-wal").exists());
    assert!(!auxiliary(&path, "-shm").exists());
    let checkpointed_before = directory_bytes(root.path());
    let report = read_report(root.path(), true, true);
    assert_eq!(report.state, "ready", "{:?}", report.error);
    assert_eq!(directory_bytes(root.path()), checkpointed_before);
    let mut snapshot = store::open_read_only(&path).unwrap();
    assert_eq!(
        snapshot
            .connection_mut()
            .pragma_query_value(None, "temp_store", |row| row.get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert!(snapshot
        .connection_mut()
        .pragma_query_value(None, "query_only", |row| row.get::<_, bool>(0))
        .unwrap());
    let write = snapshot
        .connection_mut()
        .execute("DELETE FROM daily_usage", []);
    assert!(
        matches!(
            write,
            Err(ref error)
                if error.sqlite_error_code() == Some(ErrorCode::ReadOnly)
        ),
        "{write:?}"
    );
    snapshot.verify_unchanged().unwrap();

    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    conn.execute(
        r#"
        INSERT INTO daily_usage (
            day_utc, definition_version, ctx_version, surface, operation,
            outcome, value_class, duration_bucket, target_type, pro_outcome,
            calls, result_count, citation_count, response_bytes
        ) VALUES (
            date('now'), 1, '0.26.0-active-wal', 'cli', 'doctor',
            'success', 'not_applicable', '10_to_49_ms', 'not_applicable',
            'not_applicable', 1, 0, 0, 0
        )
        "#,
        [],
    )
    .unwrap();
    let wal = auxiliary(&path, "-wal");
    assert!(fs::metadata(&wal).unwrap().len() > 0);
    let active_before = directory_bytes(root.path());
    let report = read_report(root.path(), true, true);
    assert_eq!(report.state, "error");
    assert_eq!(report.error.unwrap().code, "usage_store_unavailable");
    assert_eq!(directory_bytes(root.path()), active_before);
    drop(conn);
}

#[test]
fn double_read_snapshot_detects_same_size_restored_mtime_mutation() {
    let root = private_tempdir();
    store::record(root.path(), operation("doctor")).unwrap();
    let path = usage_path(root.path());
    let original = fs::read(&path).unwrap();
    let original_modified = path.metadata().unwrap().modified().unwrap();
    let mut changed = original.clone();
    let last = changed.len() - 1;
    changed[last] ^= 1;

    let result = store::capture_with_between_reads_for_test(root.path(), || {
        fs::write(&path, &changed).unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(original_modified))
            .unwrap();
    });
    assert!(matches!(
        result,
        Err(store::UsageStoreError::UnsafeReadState)
    ));
    fs::write(path, original).unwrap();
}

#[cfg(unix)]
#[test]
fn symlink_roots_dangling_store_links_and_unsafe_auxiliaries_are_rejected() {
    use std::os::unix::fs::{symlink, PermissionsExt as _};

    let parent = private_tempdir();
    let real = parent.path().join("real");
    fs::create_dir(&real).unwrap();
    fs::set_permissions(&real, fs::Permissions::from_mode(0o700)).unwrap();
    let linked = parent.path().join("linked");
    symlink(&real, &linked).unwrap();
    super::record_best_effort(&linked, true, operation("doctor"));
    assert!(!usage_path(&real).exists());
    assert_eq!(read_report(&linked, true, false).state, "error");

    let dangling_root = private_tempdir();
    let dangling = usage_path(dangling_root.path());
    symlink(dangling_root.path().join("missing-target"), &dangling).unwrap();
    super::record_best_effort(dangling_root.path(), true, operation("doctor"));
    assert!(dangling
        .symlink_metadata()
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        read_report(dangling_root.path(), true, false).state,
        "error"
    );

    let auxiliary_root = private_tempdir();
    let wal = auxiliary(&usage_path(auxiliary_root.path()), "-wal");
    fs::write(&wal, b"preexisting").unwrap();
    fs::set_permissions(&wal, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(store::record(auxiliary_root.path(), operation("doctor")).is_err());
    assert!(!usage_path(auxiliary_root.path()).exists());

    let existing_root = private_tempdir();
    store::record(existing_root.path(), operation("doctor")).unwrap();
    let shm = auxiliary(&usage_path(existing_root.path()), "-shm");
    if shm.exists() {
        fs::remove_file(&shm).unwrap();
    }
    symlink(existing_root.path().join("missing-shm"), &shm).unwrap();
    assert!(store::record(existing_root.path(), operation("doctor")).is_err());
    assert!(shm.symlink_metadata().unwrap().file_type().is_symlink());
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn non_utf8_data_root_uses_os_native_auxiliary_paths() {
    use std::os::unix::{ffi::OsStringExt as _, fs::PermissionsExt as _};

    let parent = private_tempdir();
    let root = parent
        .path()
        .join(OsString::from_vec(vec![b'u', b's', b'a', b'g', b'e', 0xff]));
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    store::record(&root, operation("doctor")).unwrap();
    assert_eq!(read_report(&root, true, false).summary.unwrap().calls, 1);
}

#[cfg(target_os = "macos")]
#[test]
fn macos_representable_hostile_auxiliary_path_is_rejected() {
    use std::os::unix::fs::{symlink, PermissionsExt as _};

    // APFS rejects the invalid-byte pathname used by the Unix test above before
    // ctx runs. Keep the path representable here and exercise the same native
    // auxiliary-path boundary with a hostile dangling WAL instead.
    let parent = private_tempdir();
    let root = parent.path().join("usage-\u{e9}-\u{301}");
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let wal = auxiliary(&usage_path(&root), "-wal");
    symlink(root.join("missing-wal-target"), &wal).unwrap();

    assert!(store::record(&root, operation("doctor")).is_err());
    assert!(!usage_path(&root).exists());
    assert!(wal.symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(read_report(&root, true, false).state, "error");
}

#[test]
fn fixed_initializer_slots_survive_crowded_roots_and_repeated_crashes() {
    let root = private_tempdir();
    let old = SystemTime::now() - Duration::from_secs(2 * 60 * 60);
    for index in 0..1_000 {
        fs::write(root.path().join(format!("unrelated-{index:04}")), b"x").unwrap();
    }
    for slot in 0..7 {
        let path = auxiliary(&usage_path(root.path()), &format!(".init-{slot}"));
        fs::write(&path, b"stale private staging data").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(old))
            .unwrap();
    }
    let fresh = auxiliary(&usage_path(root.path()), ".init-7");
    fs::write(&fresh, b"fresh private staging data").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&fresh, fs::Permissions::from_mode(0o600)).unwrap();
    }

    store::record(root.path(), operation("doctor")).unwrap();
    assert!(fresh.exists(), "fresh initializer must not be removed");
    fs::remove_file(&fresh).unwrap();

    for _ in 0..4 {
        for slot in 0..8 {
            let path = auxiliary(&usage_path(root.path()), &format!(".init-{slot}"));
            fs::write(&path, b"simulated crashed initializer").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            }
            fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(old))
                .unwrap();
        }
        store::record(root.path(), operation("doctor")).unwrap();
        for slot in 0..8 {
            assert!(!auxiliary(&usage_path(root.path()), &format!(".init-{slot}")).exists());
        }
    }
    assert!(root.path().join("unrelated-0999").exists());
}

#[test]
fn schema_identity_rejects_same_version_table_definition_drift() {
    let root = private_tempdir();
    store::record(root.path(), operation("doctor")).unwrap();
    let path = usage_path(root.path());
    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "writable_schema", true).unwrap();
    conn.execute(
        "UPDATE sqlite_schema SET sql = sql || ' /* altered */' \
         WHERE type = 'table' AND name = 'daily_usage'",
        [],
    )
    .unwrap();
    conn.pragma_update(None, "writable_schema", false).unwrap();
    drop(conn);

    let report = read_report(root.path(), true, false);
    assert_eq!(report.state, "error");
    assert_eq!(
        report.error.unwrap().message,
        "local usage store format is not supported"
    );
}

#[test]
fn writable_open_rejects_extra_schema_objects_before_they_can_execute() {
    let extra_table = private_tempdir();
    store::record(extra_table.path(), operation("doctor")).unwrap();
    let path = usage_path(extra_table.path());
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE timestamp_canary(exact_timestamp TEXT) STRICT;")
        .unwrap();
    drop(conn);
    let before = fs::read(&path).unwrap();
    assert!(store::record(extra_table.path(), operation("doctor")).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);

    let trigger = private_tempdir();
    store::record(trigger.path(), operation("doctor")).unwrap();
    let path = usage_path(trigger.path());
    let conn = Connection::open(&path).unwrap();
    let marker_before: String = conn
        .query_row(
            "SELECT last_retention_day FROM maintenance WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    conn.execute_batch(
        r#"
        CREATE TRIGGER exact_timestamp_canary
        AFTER INSERT ON daily_usage
        BEGIN
            UPDATE maintenance
            SET last_retention_day = '2099-12-31'
            WHERE singleton = 1;
        END;
        "#,
    )
    .unwrap();
    drop(conn);
    assert!(store::record(trigger.path(), operation("docs")).is_err());
    let conn = Connection::open(path).unwrap();
    let marker_after: String = conn
        .query_row(
            "SELECT last_retention_day FROM maintenance WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        marker_after, marker_before,
        "the canary trigger must not run"
    );
    let docs_rows: u64 = conn
        .query_row(
            "SELECT COUNT(*) FROM daily_usage WHERE operation = 'docs'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(docs_rows, 0);
}

#[cfg(unix)]
#[test]
fn multiply_linked_main_wal_and_shm_members_are_rejected() {
    use std::os::unix::fs::MetadataExt as _;

    let root = private_tempdir();
    let aliases = private_tempdir();
    store::record(root.path(), operation("doctor")).unwrap();
    let path = usage_path(root.path());
    assert_eq!(fs::metadata(&path).unwrap().nlink(), 1);

    let main_alias = aliases.path().join("main-alias");
    fs::hard_link(&path, &main_alias).unwrap();
    assert!(store::record(root.path(), operation("docs")).is_err());
    fs::remove_file(main_alias).unwrap();

    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    conn.execute(
        "UPDATE daily_usage SET calls = calls + 1 WHERE operation = 'doctor'",
        [],
    )
    .unwrap();
    for suffix in ["-wal", "-shm"] {
        let member = auxiliary(&path, suffix);
        assert!(member.exists(), "{suffix}");
        let alias = aliases.path().join(format!("alias{suffix}"));
        fs::hard_link(&member, &alias).unwrap();
        assert!(store::record(root.path(), operation("docs")).is_err());
        fs::remove_file(alias).unwrap();
    }
    drop(conn);
}

#[test]
fn windows_single_link_guard_uses_stable_handle_api_contract() {
    let source = include_str!("store/file_family.rs");
    let guard = source
        .split_once("fn verify_single_link")
        .unwrap()
        .1
        .split_once("\n}\n\nfn verify_family_size")
        .unwrap()
        .0;

    assert!(guard.contains("GetFileInformationByHandle"));
    assert!(guard.contains("BY_HANDLE_FILE_INFORMATION"));
    assert!(guard.contains("nNumberOfLinks != 1"));
    assert!(guard.contains("io::Error::last_os_error()"));
    assert!(!guard.contains("std::os::windows::fs::MetadataExt"));
    assert!(!guard.contains("number_of_links()"));
}

#[test]
fn windows_reopen_identity_guard_uses_stable_handle_metadata_contract() {
    let source = include_str!("store/file_family.rs");
    let reopen = source
        .split_once("fn reopen_same_file")
        .unwrap()
        .1
        .split_once("\n}\n\n#[cfg(all(test, windows))]")
        .unwrap()
        .0;
    let open = source
        .split_once("fn open_nofollow")
        .unwrap()
        .1
        .split_once("\n}\n\npub(super) fn verify_private_directory_and_owner")
        .unwrap()
        .0;

    assert!(reopen.contains("same_file::Handle::from_file"));
    assert!(reopen.contains("GetFileInformationByHandle volume serial and file index"));
    assert!(reopen.contains("try_clone()?"));
    assert!(reopen.contains("FILE_ATTRIBUTE_REPARSE_POINT"));
    assert!(!reopen.contains("unsafe"));
    assert!(open.contains("FILE_FLAG_OPEN_REPARSE_POINT"));
    assert!(open.contains("FILE_SHARE_READ | FILE_SHARE_WRITE"));
    let open_code = open
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<String>();
    assert!(!open_code.contains("FILE_SHARE_DELETE"));
}

#[cfg(windows)]
#[test]
fn windows_reopened_path_rejects_private_single_link_replacement() {
    use std::os::windows::fs::OpenOptionsExt as _;

    use ctx_history_core::platform_security::restrict_private_file;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let root = private_tempdir();
    store::record(root.path(), operation("doctor")).unwrap();
    let path = usage_path(root.path());
    let displaced = root.path().join("usage-original.sqlite");
    let retained = fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&path)
        .unwrap();

    fs::rename(&path, &displaced).unwrap();
    fs::copy(&displaced, &path).unwrap();
    restrict_private_file(&path).unwrap();
    let replacement = fs::File::open(&path).unwrap();
    store::assert_single_link_for_test(&replacement).unwrap();
    assert_ne!(
        same_file::Handle::from_file(retained.try_clone().unwrap()).unwrap(),
        same_file::Handle::from_file(replacement.try_clone().unwrap()).unwrap()
    );
    let replacement_before = fs::read(&path).unwrap();

    assert!(matches!(
        store::verify_same_file_for_test(&path, &retained),
        Err(store::UsageStoreError::SchemaIdentity)
    ));
    assert_eq!(fs::read(path).unwrap(), replacement_before);
}

#[test]
fn disabled_reporting_and_absent_reset_create_no_sidecar() {
    let root = private_tempdir();
    let report = read_report(root.path(), false, false);
    assert_eq!(report.state, "disabled");
    assert!(report.summary.is_none());
    assert!(!usage_path(root.path()).exists());
    assert!(!reset(root.path()).unwrap());
    assert!(!usage_path(root.path()).exists());
}

#[test]
fn reset_logically_deletes_aggregates_without_promising_forensic_erasure() {
    let root = private_tempdir();
    store::record(root.path(), operation("docs")).unwrap();
    assert!(reset(root.path()).unwrap());
    assert!(usage_path(root.path()).exists());
    let report = read_report(root.path(), true, true);
    assert_eq!(report.state, "empty");
    assert_eq!(report.summary.unwrap().calls, 0);
    let conn = Connection::open(usage_path(root.path())).unwrap();
    let rows: u64 = conn
        .query_row("SELECT COUNT(*) FROM daily_usage", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 0);
    let wal = auxiliary(&usage_path(root.path()), "-wal");
    assert!(fs::metadata(wal).map_or(true, |metadata| metadata.len() == 0));
}

#[test]
fn completed_logical_reset_succeeds_even_when_a_reader_blocks_truncate_checkpoint() {
    let root = private_tempdir();
    store::record(root.path(), operation("docs")).unwrap();
    assert!(store::reset_with_post_commit_for_test(root.path(), |path| {
        let reader = Connection::open(path).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        let _: i64 = reader
            .query_row("SELECT COUNT(*) FROM daily_usage", [], |row| row.get(0))
            .unwrap();
        reader
    })
    .unwrap());
    assert_eq!(
        read_report(root.path(), true, false).summary.unwrap().calls,
        0
    );
}

#[test]
fn atomic_upsert_is_exact_under_concurrency() {
    let root = Arc::new(private_tempdir());
    let mut threads = Vec::new();
    for _ in 0..6 {
        let root = Arc::clone(&root);
        threads.push(thread::spawn(move || {
            for _ in 0..20 {
                loop {
                    match store::record_at_for_test(
                        root.path(),
                        operation("search"),
                        SystemTime::now(),
                        Duration::from_secs(2),
                    ) {
                        Ok(()) => break,
                        Err(store::UsageStoreError::Sql(error))
                            if error.sqlite_error_code() == Some(ErrorCode::DatabaseBusy) => {}
                        Err(store::UsageStoreError::UnsafeReadState) => {}
                        Err(store::UsageStoreError::SchemaIdentity) => {}
                        Err(store::UsageStoreError::Io(error))
                            if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => panic!("{error}"),
                    }
                }
            }
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(
        read_report(root.path(), true, false).summary.unwrap().calls,
        120
    );
}

#[test]
fn short_contention_timeout_is_fail_open_at_the_recording_boundary() {
    let root = private_tempdir();
    store::record(root.path(), operation("doctor")).unwrap();
    let lock = Connection::open(usage_path(root.path())).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let started = Instant::now();
    super::record_best_effort(root.path(), true, operation("docs"));
    assert!(started.elapsed() < Duration::from_millis(500));
    lock.execute_batch("ROLLBACK").unwrap();
    drop(lock);
    assert_eq!(
        read_report(root.path(), true, false).summary.unwrap().calls,
        1
    );
}

#[test]
fn warm_locked_store_p99_is_bounded_across_one_thousand_samples() {
    let root = private_tempdir();
    store::record(root.path(), operation("doctor")).unwrap();
    let lock = Connection::open(usage_path(root.path())).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut samples = Vec::with_capacity(1_000);
    for _ in 0..1_000 {
        let started = Instant::now();
        let result = store::record_at_for_test(
            root.path(),
            operation("docs"),
            SystemTime::now(),
            Duration::ZERO,
        );
        samples.push(started.elapsed());
        assert!(
            matches!(&result, Err(store::UsageStoreError::Sql(error))
                if error.sqlite_error_code() == Some(ErrorCode::DatabaseBusy))
                || matches!(result, Err(store::UsageStoreError::UnsafeReadState))
        );
    }
    lock.execute_batch("ROLLBACK").unwrap();
    samples.sort_unstable();
    let p99 = samples[989];
    eprintln!("local usage locked-store p99 over 1,000 samples: {p99:?}");
    assert!(p99 < Duration::from_millis(50));
}

#[test]
fn database_full_is_fail_open_and_quiescent_file_family_stays_below_eight_mib() {
    let root = private_tempdir();
    store::record(root.path(), operation("docs")).unwrap();
    let full_version = store::fill_to_capacity_for_test(root.path()).unwrap();
    let path = usage_path(root.path());
    let calls = || {
        let conn = Connection::open(&path).unwrap();
        conn.query_row("SELECT SUM(calls) FROM daily_usage", [], |row| {
            row.get::<_, u64>(0)
        })
        .unwrap()
    };
    let before = calls();
    let error =
        store::record_with_ctx_version_for_test(root.path(), operation("doctor"), &full_version)
            .unwrap_err();
    assert!(
        matches!(error, store::UsageStoreError::Sql(ref error)
            if error.sqlite_error_code() == Some(ErrorCode::DiskFull)),
        "{error}"
    );
    super::record_best_effort_with_ctx_version_for_test(
        root.path(),
        operation("doctor"),
        &full_version,
    );
    assert_eq!(calls(), before);

    let family_bytes = ["", "-wal", "-shm"]
        .into_iter()
        .map(|suffix| {
            let member = if suffix.is_empty() {
                path.clone()
            } else {
                auxiliary(&path, suffix)
            };
            fs::metadata(member).map_or(0, |metadata| metadata.len())
        })
        .sum::<u64>();
    assert!(
        family_bytes < 8 * 1024 * 1024,
        "quiescent usage.sqlite family used {family_bytes} bytes"
    );
}

#[test]
fn retention_keeps_approximately_four_hundred_utc_days() {
    let root = private_tempdir();
    let day = Duration::from_secs(24 * 60 * 60);
    let recent = UNIX_EPOCH + day * 20_000;
    let expired = recent - day * 401;
    store::record_at_for_test(
        root.path(),
        operation("doctor"),
        expired,
        Duration::from_secs(1),
    )
    .unwrap();
    store::record_at_for_test(
        root.path(),
        operation("doctor"),
        recent,
        Duration::from_secs(1),
    )
    .unwrap();
    let summary = read_report(root.path(), true, false).summary.unwrap();
    assert_eq!(summary.calls, 1);
    assert_eq!(summary.active_days, 1);
}

#[test]
fn future_store_dates_are_reported_and_block_older_upserts() {
    let root = private_tempdir();
    let day = Duration::from_secs(24 * 60 * 60);
    let now = SystemTime::now();
    let future = now + day * 30;
    store::record_at_for_test(
        root.path(),
        operation("doctor"),
        future,
        Duration::from_secs(1),
    )
    .unwrap();

    let error = store::record_at_for_test(
        root.path(),
        operation("doctor"),
        now,
        Duration::from_secs(1),
    )
    .unwrap_err();
    assert!(matches!(error, store::UsageStoreError::FutureDate));
    let report = read_report(root.path(), true, false);
    assert_eq!(report.state, "error");
    assert_eq!(
        report.error.unwrap().message,
        "local usage store date is ahead of the current UTC day"
    );
}

#[test]
fn productive_write_repairs_a_future_maintenance_marker() {
    let root = private_tempdir();
    store::record(root.path(), operation("doctor")).unwrap();
    let conn = Connection::open(usage_path(root.path())).unwrap();
    conn.execute(
        "UPDATE maintenance SET last_retention_day = '2999-01-01' WHERE singleton = 1",
        [],
    )
    .unwrap();
    drop(conn);
    assert_eq!(read_report(root.path(), true, false).state, "error");

    store::record(root.path(), operation("docs")).unwrap();
    let report = read_report(root.path(), true, false);
    assert_eq!(report.state, "ready");
    assert_eq!(report.summary.unwrap().calls, 2);
    let conn = Connection::open(usage_path(root.path())).unwrap();
    let (maintenance, latest_usage): (String, String) = conn
        .query_row(
            "SELECT last_retention_day, (SELECT MAX(day_utc) FROM daily_usage) \
             FROM maintenance WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(maintenance, latest_usage);
}

#[test]
fn report_rejects_tampered_impossible_rows_and_aggregate_overflow() {
    let tampered = private_tempdir();
    store::record(tampered.path(), operation("doctor")).unwrap();
    let conn = Connection::open(usage_path(tampered.path())).unwrap();
    conn.pragma_update(None, "ignore_check_constraints", true)
        .unwrap();
    conn.execute("UPDATE daily_usage SET calls = -1", [])
        .unwrap();
    drop(conn);
    let report = read_report(tampered.path(), true, true);
    assert_eq!(report.state, "error");
    assert_eq!(
        report.error.unwrap().message,
        "local usage store format is not supported"
    );

    let overflow = private_tempdir();
    store::record(overflow.path(), operation("doctor")).unwrap();
    store::record(overflow.path(), operation("docs")).unwrap();
    let conn = Connection::open(usage_path(overflow.path())).unwrap();
    conn.pragma_update(None, "ignore_check_constraints", true)
        .unwrap();
    conn.execute("UPDATE daily_usage SET calls = ?1", [i64::MAX])
        .unwrap();
    drop(conn);
    let report = read_report(overflow.path(), true, true);
    assert_eq!(report.state, "error");
    assert!(report.summary.is_none());
    assert_eq!(report.error.unwrap().code, "usage_store_unavailable");
}

#[test]
fn reports_use_one_consistent_snapshot_while_writes_continue() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let root = Arc::new(private_tempdir());
    store::record(root.path(), operation("doctor")).unwrap();
    let finished = Arc::new(AtomicBool::new(false));
    let writer_root = Arc::clone(&root);
    let writer_finished = Arc::clone(&finished);
    let writer = thread::spawn(move || {
        for _ in 0..200 {
            loop {
                match store::record_at_for_test(
                    writer_root.path(),
                    operation("doctor"),
                    SystemTime::now(),
                    Duration::from_secs(1),
                ) {
                    Ok(()) => break,
                    Err(store::UsageStoreError::Sql(error))
                        if error.sqlite_error_code() == Some(ErrorCode::DatabaseBusy) => {}
                    Err(store::UsageStoreError::UnsafeReadState) => {}
                    Err(error) => panic!("{error}"),
                }
            }
        }
        writer_finished.store(true, Ordering::Release);
    });
    while !finished.load(Ordering::Acquire) {
        let report = read_report(root.path(), true, true);
        if report.state == "error" {
            assert!(
                matches!(
                    report.error.unwrap().message,
                    "local usage store could not be read"
                        | "local usage store format is not supported"
                ),
                "only stable content-free report errors are allowed"
            );
            continue;
        }
        assert_eq!(report.state, "ready");
        let summary = report.summary.unwrap();
        assert_eq!(
            summary.successful_calls + summary.failed_calls,
            summary.calls
        );
        assert_eq!(
            summary.result_bearing_calls + summary.empty_calls + summary.not_applicable_calls,
            summary.calls
        );
    }
    writer.join().unwrap();
    assert_eq!(
        read_report(root.path(), true, false).summary.unwrap().calls,
        201
    );
}

#[test]
fn recognized_mcp_calls_are_classified_from_the_flushed_response_shape() {
    let mut invocation = McpInvocation::recognized("blame").unwrap();
    invocation.bind_blame_target(&BlameTarget::Commit {
        oid: "abc1234".to_owned(),
        repository: None,
    });
    let response = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "structuredContent": {
                "matches": [{
                    "kind": "commit",
                    "value": {"predicate": "produced_by", "state": "asserted"}
                }],
                "evidence": [{"number": 1}]
            }
        }
    });
    let completed = invocation.completed(&response, Duration::from_millis(8), 321);
    assert_eq!(completed.surface, Surface::Mcp);
    assert_eq!(completed.target_type, TargetType::Commit);
    assert_eq!(completed.pro_outcome, ProOutcome::Produced);
    assert_eq!(completed.response_bytes, 321);
    assert_eq!(completed.citation_count, 1);
}

#[test]
fn mcp_blame_classification_reads_only_typed_match_fields() {
    let response = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "structuredContent": {
                "matches": [],
                "evidence": [{"display": "{\"predicate\":\"produced_by\"}"}]
            }
        }
    });

    let mut invocation = McpInvocation::recognized("blame").unwrap();
    invocation.bind_blame_target(&BlameTarget::Commit {
        oid: "abc1234".to_owned(),
        repository: None,
    });
    let completed = invocation.completed(&response, Duration::ZERO, 100);
    assert_eq!(completed.pro_outcome, ProOutcome::None);
    assert_eq!(completed.value_class, ValueClass::Empty);
}

#[test]
fn commit_outcome_classifier_is_closed_and_matches_the_wire_projection() {
    for state in [
        FactState::Asserted,
        FactState::Ambiguous,
        FactState::Contradicted,
        FactState::Superseded,
    ] {
        for predicate in [
            CommitPredicate::ProducedBy,
            CommitPredicate::PossiblyProducedBy,
            CommitPredicate::AmendedBy,
            CommitPredicate::CherryPickedFrom,
            CommitPredicate::Reverts,
            CommitPredicate::PushedBy,
            CommitPredicate::InspectedBy,
            CommitPredicate::ReferencedBy,
        ] {
            let structured = json!({
                "matches": [{
                    "kind": "commit",
                    "value": {"predicate": predicate, "state": state}
                }]
            });
            assert_eq!(
                super::classify_blame_json(Some(&structured)),
                super::classify_commit_predicate(predicate, state),
                "{predicate:?}/{state:?}"
            );
        }
    }
    assert_eq!(
        super::classify_commit_predicate(CommitPredicate::ProducedBy, FactState::Asserted),
        ProOutcome::Produced
    );
    assert_eq!(
        super::classify_commit_predicate(CommitPredicate::ReferencedBy, FactState::Asserted),
        ProOutcome::Possible
    );
    for state in [
        FactState::Ambiguous,
        FactState::Contradicted,
        FactState::Superseded,
    ] {
        assert_ne!(
            super::classify_commit_predicate(CommitPredicate::ProducedBy, state),
            ProOutcome::Produced
        );
    }
}

#[test]
fn typed_commit_blame_produces_only_for_asserted_produced_by() {
    let resource = |id: &str, kind| ResourceRef {
        id: id.to_owned(),
        kind,
        display: id.to_owned(),
    };
    let commit = resource("commit:abc1234", ResourceKind::Commit);
    for state in [
        FactState::Asserted,
        FactState::Ambiguous,
        FactState::Contradicted,
        FactState::Superseded,
    ] {
        let result = BlameResult {
            target: ResolvedBlameTarget::Commit {
                commit: commit.clone(),
                repository: resource("repository:ctx", ResourceKind::Repository),
            },
            git_snapshot: None,
            matches: vec![BlameMatch::Commit(CommitBlameMatch {
                fact_id: format!("fact:{state:?}"),
                fact_type: CommitFactType::Produced,
                predicate: CommitPredicate::ProducedBy,
                subject: commit.clone(),
                object: Some(resource("session:producer", ResourceKind::Session)),
                fact_occurred_at_ms: None,
                confidence: FactConfidence::Explicit,
                state,
                direct_actor: None,
                owning_root: None,
                evidence_numbers: Vec::new(),
            })],
            evidence: Vec::new(),
            next: None,
        };
        assert_eq!(
            super::classify_blame(&result),
            match state {
                FactState::Asserted => ProOutcome::Produced,
                FactState::Ambiguous => ProOutcome::Possible,
                FactState::Contradicted | FactState::Superseded => ProOutcome::None,
            },
            "{state:?}"
        );
    }
}

#[test]
fn file_and_pr_production_states_use_the_same_conservative_classifier() {
    for state in [
        FactState::Asserted,
        FactState::Ambiguous,
        FactState::Contradicted,
        FactState::Superseded,
    ] {
        assert_eq!(
            super::classify_production(
                ctx_pro_host_protocol::ProductionRelationship::ProducedBy,
                state,
            ),
            match state {
                FactState::Asserted => ProOutcome::Produced,
                FactState::Ambiguous => ProOutcome::Possible,
                FactState::Contradicted | FactState::Superseded => ProOutcome::None,
            }
        );
        assert_eq!(
            super::classify_production(
                ctx_pro_host_protocol::ProductionRelationship::PossiblyProducedBy,
                state,
            ),
            match state {
                FactState::Asserted | FactState::Ambiguous => ProOutcome::Possible,
                FactState::Contradicted | FactState::Superseded => ProOutcome::None,
            }
        );
    }
}

#[test]
fn local_mcp_vocabulary_is_closed() {
    for name in [
        "status",
        "sources",
        "search",
        "sql",
        "show_session",
        "show_event",
        "pro_status",
        "blame",
    ] {
        assert!(McpInvocation::recognized(name).is_some(), "{name}");
    }
    for name in [
        "initialize",
        "ping",
        "tools/list",
        "unknown",
        "private query",
    ] {
        assert!(McpInvocation::recognized(name).is_none(), "{name}");
    }
}

#[test]
fn mcp_recorder_observes_same_size_and_mtime_persistent_disable() {
    let _env = LocalUsageEnvGuard::unset();
    let root = private_tempdir();
    let config_path = root.path().join("config.toml");
    fs::write(&config_path, "[local_usage]\nenabled = true \n").unwrap();
    let original_modified = config_path.metadata().unwrap().modified().unwrap();
    let original_len = config_path.metadata().unwrap().len();
    let mut recorder = super::McpUsageRecorder::start(root.path().to_path_buf());
    let invocation = McpInvocation::recognized("status").unwrap();
    let response = json!({"jsonrpc": "2.0", "id": 1, "result": {
        "structuredContent": {"schema_version": 1}
    }});
    recorder.record_delivered(invocation, &response, Duration::ZERO, 40);
    fs::write(&config_path, "[local_usage]\nenabled = false\n").unwrap();
    assert_eq!(config_path.metadata().unwrap().len(), original_len);
    fs::File::options()
        .write(true)
        .open(&config_path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(original_modified))
        .unwrap();
    recorder.record_delivered(invocation, &response, Duration::ZERO, 40);
    assert_eq!(
        read_report(root.path(), true, false).summary.unwrap().calls,
        1
    );
}

#[test]
fn mcp_recorder_retains_last_known_control_only_on_unrelated_config_failure() {
    let _env = LocalUsageEnvGuard::unset();
    let root = private_tempdir();
    let config_path = root.path().join("config.toml");
    fs::write(&config_path, "[local_usage]\nenabled = true\n").unwrap();
    let mut recorder = super::McpUsageRecorder::start(root.path().to_path_buf());
    let invocation = McpInvocation::recognized("status").unwrap();
    let response = json!({"jsonrpc": "2.0", "id": 1, "result": {
        "structuredContent": {"schema_version": 1}
    }});
    recorder.record_delivered(invocation, &response, Duration::ZERO, 40);
    fs::write(&config_path, "unrelated malformed line\n").unwrap();
    recorder.record_delivered(invocation, &response, Duration::ZERO, 40);
    assert_eq!(
        read_report(root.path(), true, false).summary.unwrap().calls,
        2
    );

    let disabled = private_tempdir();
    let disabled_config = disabled.path().join("config.toml");
    fs::write(&disabled_config, "[local_usage]\nenabled = false\n").unwrap();
    let mut recorder = super::McpUsageRecorder::start(disabled.path().to_path_buf());
    fs::write(
        &disabled_config,
        "unrelated malformed line without a local usage key\n",
    )
    .unwrap();
    recorder.record_delivered(invocation, &response, Duration::ZERO, 40);
    assert!(!usage_path(disabled.path()).exists());
}

#[test]
fn malformed_local_control_disables_mcp_refresh_and_startup() {
    let _env = LocalUsageEnvGuard::unset();
    let invocation = McpInvocation::recognized("status").unwrap();
    let response = json!({"jsonrpc": "2.0", "id": 1, "result": {
        "structuredContent": {"schema_version": 1}
    }});
    for (name, malformed) in [
        ("invalid_value", "[local_usage]\nenabled = malformed\n"),
        ("bare", "local_usage = true\n"),
        ("inline_table", "local_usage = { enabled = true }\n"),
        ("quoted_dotted", "\"local_usage\".enabled = true\n"),
        (
            "unicode_u_escaped_key",
            "\"local\\u005Fusage\".enabled = false\n",
        ),
        (
            "unicode_upper_u_escaped_table_path",
            "[\"\\U0000006Cocal_usage\".nested]\nvalue = false\n",
        ),
        (
            "owned_prefix_before_malformed_escape",
            "\"local\\u005Fusage.\\uZZZZ\" = false\n",
        ),
        (
            "duplicate_table",
            "[local_usage]\nenabled = true\n[local_usage]\n",
        ),
    ] {
        let root = private_tempdir();
        let config_path = root.path().join("config.toml");
        fs::write(&config_path, "[local_usage]\nenabled = true\n").unwrap();
        let mut recorder = super::McpUsageRecorder::start(root.path().to_path_buf());
        recorder.record_delivered(invocation, &response, Duration::ZERO, 40);
        fs::write(&config_path, malformed).unwrap();
        recorder.record_delivered(invocation, &response, Duration::ZERO, 40);
        assert_eq!(
            read_report(root.path(), true, false).summary.unwrap().calls,
            1,
            "{name}"
        );

        let startup = private_tempdir();
        fs::write(startup.path().join("config.toml"), malformed).unwrap();
        let mut recorder = super::McpUsageRecorder::start(startup.path().to_path_buf());
        recorder.record_delivered(invocation, &response, Duration::ZERO, 40);
        assert!(!usage_path(startup.path()).exists(), "{name}");
    }
}

#[test]
fn malformed_store_reports_error_instead_of_zero() {
    let root = private_tempdir();
    fs::write(usage_path(root.path()), b"not sqlite").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(usage_path(root.path()), fs::Permissions::from_mode(0o600)).unwrap();
    }
    let report = read_report(root.path(), true, false);
    assert_eq!(report.state, "error");
    assert!(report.summary.is_none());
    assert_eq!(report.error.unwrap().code, "usage_store_unavailable");
}

#[test]
fn oversized_store_image_reports_a_bounded_content_free_error() {
    let root = private_tempdir();
    let path = usage_path(root.path());
    let file = fs::File::create(&path).unwrap();
    file.set_len(6 * 1024 * 1024 + 4096).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let report = read_report(root.path(), true, false);
    assert_eq!(report.state, "error");
    let encoded = serde_json::to_string(&report).unwrap();
    assert!(!encoded.contains(root.path().to_string_lossy().as_ref()));
    assert_eq!(
        report.error.unwrap().message,
        "local usage store exceeds its size limit"
    );
}

#[test]
fn public_usage_errors_never_serialize_raw_paths_or_causes() {
    let marker = "SECRET_PATH_TOKEN_7f98";
    let raw_cause = format!("database at /tmp/{marker} contains bearer-secret");
    let report = super::UsageReport::config_error();
    let encoded = serde_json::to_string(&report).unwrap();
    assert!(!encoded.contains(marker));
    assert!(!encoded.contains("bearer-secret"));
    assert!(raw_cause.contains(marker));
    assert_eq!(
        serde_json::to_value(report).unwrap()["error"]["message"],
        "local usage configuration could not be read"
    );
}

#[test]
fn recording_hot_path_benchmark_smoke() {
    let root = private_tempdir();
    for _ in 0..10 {
        store::record(root.path(), operation("search")).unwrap();
    }
    let mut samples = Vec::with_capacity(1_000);
    for _ in 0..1_000 {
        let started = Instant::now();
        store::record(root.path(), operation("search")).unwrap();
        samples.push(started.elapsed());
    }
    samples.sort_unstable();
    let p50 = samples[499];
    let p90 = samples[899];
    let p95 = samples[949];
    let p99 = samples[989];
    let maximum = samples[999];
    eprintln!(
        "local usage warm upsert over 1,000 samples: \
         p50={p50:?} p90={p90:?} p95={p95:?} p99={p99:?} max={maximum:?}"
    );
    // Fastbuild/debug runs inside the broad unit-test binary and owns only a
    // coarse runaway-I/O smoke ceiling. Exclusive release qualification owns
    // the product contract's <=10 ms p99.
    #[cfg(debug_assertions)]
    assert!(
        p99 <= Duration::from_millis(500),
        "local usage warm upsert exceeded the debug smoke ceiling: \
         p50={p50:?} p90={p90:?} p95={p95:?} p99={p99:?} max={maximum:?}"
    );
    #[cfg(not(debug_assertions))]
    assert!(
        p99 <= Duration::from_millis(10),
        "local usage warm upsert exceeded its release p99 contract: \
         p50={p50:?} p90={p90:?} p95={p95:?} p99={p99:?} max={maximum:?}"
    );
    assert_eq!(DEFINITION_VERSION, 1);
}

#[test]
fn local_control_refresh_p99_is_bounded_across_one_thousand_samples() {
    let _env = LocalUsageEnvGuard::unset();
    let root = private_tempdir();
    fs::write(
        root.path().join("config.toml"),
        "[local_usage]\nenabled = true\n",
    )
    .unwrap();
    let mut samples = Vec::with_capacity(1_000);
    for _ in 0..1_000 {
        let started = Instant::now();
        assert!(
            crate::config::read_local_usage_control(root.path())
                .unwrap()
                .effective_enabled
        );
        samples.push(started.elapsed());
    }
    samples.sort_unstable();
    let p99 = samples[989];
    eprintln!("local usage control refresh p99 over 1,000 samples: {p99:?}");
    assert!(p99 < Duration::from_millis(25));
}

#[test]
fn cli_controls_and_mcp_serve_do_not_create_duplicate_observations() {
    use clap::Parser as _;

    for args in [
        ["ctx", "status", "--usage", "reset"].as_slice(),
        ["ctx", "status", "--usage", "disable"].as_slice(),
        ["ctx", "mcp", "serve"].as_slice(),
        ["ctx", "daemon", "run"].as_slice(),
    ] {
        let cli = crate::Cli::try_parse_from(args).unwrap();
        assert!(CliUsage::from_command(&cli.command)
            .completed(true, Duration::ZERO)
            .is_none());
    }
}

#[test]
fn replacement_helper_is_excluded_while_manual_upgrade_remains_eligible() {
    use clap::Parser as _;

    let helper = crate::Cli::try_parse_from(["ctx", "upgrade", "--replacement-helper"]).unwrap();
    assert!(
        CliUsage::from_command(&helper.command)
            .completed(false, Duration::ZERO)
            .is_none(),
        "the automatic replacement helper must not create a usage descriptor"
    );

    let manual = crate::Cli::try_parse_from(["ctx", "upgrade", "--dry-run"]).unwrap();
    let completed = CliUsage::from_command(&manual.command)
        .completed(true, Duration::ZERO)
        .expect("ordinary manual upgrade must remain usage-eligible");
    assert_eq!(completed.surface, Surface::Cli);
    assert_eq!(completed.operation, "upgrade");
}

#[test]
fn conversion_action_is_limited_to_trial_and_locked_access() {
    let trial = super::pro_conversion_action(Some("trial")).unwrap();
    assert_eq!(trial["kind"], "pro_monthly_conversion");
    assert_eq!(trial["price"], "$20/month");
    assert_eq!(trial["command"], "ctx pro manage");

    let locked = super::pro_conversion_action(Some("locked")).unwrap();
    assert_eq!(locked["kind"], "pro_restore_access");
    assert_eq!(locked["reason"], "access_locked");
    assert_eq!(locked["graph_preserved"], true);
    assert!(locked.get("price").is_none());

    for state in ["active", "canceling_paid", "offline_grace", "grace"] {
        assert!(
            super::pro_conversion_action(Some(state)).is_none(),
            "{state}"
        );
    }
    assert!(super::pro_conversion_action(None).is_none());
}
