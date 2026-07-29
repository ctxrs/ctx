use super::*;

#[test]
fn warm_report_read_latency_has_a_small_sanity_ceiling() {
    let root = private_tempdir();
    for _ in 0..8 {
        store::record(root.path(), operation("search")).unwrap();
    }
    let started = Instant::now();
    for _ in 0..25 {
        assert_eq!(read_report(root.path(), true, true).state, "ready");
    }
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "25 local usage reads exceeded the sanity ceiling"
    );
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
    super::super::record_best_effort(&linked, true, operation("doctor"));
    assert!(!usage_path(&real).exists());
    assert_eq!(read_report(&linked, true, false).state, "error");

    let dangling_root = private_tempdir();
    let dangling = usage_path(dangling_root.path());
    symlink(dangling_root.path().join("missing-target"), &dangling).unwrap();
    super::super::record_best_effort(dangling_root.path(), true, operation("doctor"));
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
    let source = include_str!("../store/file_family.rs");
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
    let source = include_str!("../store/file_family.rs");
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
    super::super::record_best_effort(root.path(), true, operation("docs"));
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
    super::super::record_best_effort_with_ctx_version_for_test(
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
    let mut search = mcp_operation("search", true, ValueClass::ResultBearing, 1);
    search.context.context_searches = 1;
    search.context.context_found = 1;
    store::record_at_for_test(root.path(), search, expired, Duration::from_secs(1)).unwrap();
    let mut opened = mcp_operation("show_session", true, ValueClass::ResultBearing, 1);
    opened.context.context_opened = 1;
    opened.context.validated_discoveries = 1;
    store::record_at_for_test(root.path(), opened, recent, Duration::from_secs(1)).unwrap();
    let summary = read_report(root.path(), true, false).summary.unwrap();
    assert_eq!(summary.calls, 1);
    assert_eq!(summary.active_days, 1);
    assert_eq!(summary.context.context_found, 0);
    assert_eq!(summary.context.context_opened, 1);
    assert_eq!(summary.context.validated_discoveries, 1);
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
fn report_rejects_estimate_arithmetic_overflow_instead_of_saturating() {
    let root = private_tempdir();
    let mut blame = operation("blame");
    blame.value_class = ValueClass::ResultBearing;
    blame.target_type = TargetType::Commit;
    blame.pro_outcome = ProOutcome::Produced;
    blame.result_action = Some(ResultObservationAction::Blame);
    blame.result_count = 1;
    store::record(root.path(), blame).unwrap();

    let conn = Connection::open(usage_path(root.path())).unwrap();
    conn.execute(
        "UPDATE daily_usage SET calls = ?1, result_count = ?1",
        [i64::MAX],
    )
    .unwrap();
    drop(conn);

    let report = read_report(root.path(), true, false);
    assert_eq!(report.state, "error");
    assert!(report.summary.is_none());
    assert!(report.estimates.is_none());
    assert_eq!(
        report.error.unwrap().message,
        "local usage store format is not supported"
    );

    let token_root = private_tempdir();
    store::record(
        token_root.path(),
        mcp_operation("search", true, ValueClass::ResultBearing, 1),
    )
    .unwrap();
    let conn = Connection::open(usage_path(token_root.path())).unwrap();
    conn.execute(
        "UPDATE daily_usage SET context_bytes = ?1, search_result_bytes = ?1",
        [i64::MAX],
    )
    .unwrap();
    drop(conn);

    let report = read_report(token_root.path(), true, false);
    assert_eq!(report.state, "error");
    assert!(report.summary.is_none());
    assert!(report.estimates.is_none());
    assert_eq!(
        report.error.unwrap().message,
        "local usage store format is not supported"
    );
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
