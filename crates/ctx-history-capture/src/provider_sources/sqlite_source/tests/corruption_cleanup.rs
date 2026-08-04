use super::*;

#[test]
fn malformed_provider_is_source_local_and_is_not_retried_as_a_full_copy() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let _writer = create_persistent_wal(&database);
    let mut database_file = OpenOptions::new().write(true).open(&database).unwrap();
    database_file.seek(SeekFrom::Start(0)).unwrap();
    database_file.write_all(&[0_u8; 100]).unwrap();
    database_file.sync_all().unwrap();
    drop(database_file);
    let wal = database.with_file_name("provider.sqlite-wal");
    let mut wal_file = OpenOptions::new().write(true).open(&wal).unwrap();
    wal_file.seek(SeekFrom::Start(0)).unwrap();
    wal_file.write_all(&[0_u8; 32]).unwrap();
    wal_file.sync_all().unwrap();
    drop(wal_file);
    let expected_copy_bytes =
        fs::metadata(&database).unwrap().len() + fs::metadata(&wal).unwrap().len();
    let before = directory_file_bytes(temp.path());
    let parent = retain_parent(temp.path());

    let error = parent
        .open_logical_online_backup_snapshot(OsStr::new("provider.sqlite"))
        .unwrap_err();

    let diagnostic = error.diagnostic().unwrap();
    assert_eq!(diagnostic.phase, SqliteFailurePhase::SourceValidation);
    assert_eq!(diagnostic.artifact, SqliteArtifactKind::ProviderDatabase);
    assert!(matches!(
        diagnostic.sqlite_primary_code,
        Some(ffi::SQLITE_CORRUPT) | Some(ffi::SQLITE_NOTADB)
    ));
    assert_eq!(diagnostic.retry, SqliteRetryDecision::DoNotRetryCorrupt);
    assert_eq!(
        parent.snapshot_counters().source_bytes_copied(),
        expected_copy_bytes
    );
    assert_eq!(directory_file_bytes(temp.path()), before);
}

#[test]
fn malformed_private_source_copy_is_ctx_owned_and_not_retried() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let _writer = create_persistent_wal(&database);
    let expected_copy_bytes = fs::metadata(&database).unwrap().len()
        + fs::metadata(database.with_file_name("provider.sqlite-wal"))
            .unwrap()
            .len();
    let before = directory_file_bytes(temp.path());
    let parent = retain_parent(temp.path());

    let error = open_root_handle_sqlite_source_online_backup_after_private_source_copy_for_test(
        &parent,
        OsStr::new("provider.sqlite"),
        |source_copy| {
            let mut copy = OpenOptions::new().write(true).open(source_copy).unwrap();
            copy.seek(SeekFrom::Start(0)).unwrap();
            copy.write_all(&[0_u8; 100]).unwrap();
            copy.sync_all().unwrap();
        },
    )
    .unwrap_err();

    let diagnostic = error.diagnostic().unwrap();
    assert_eq!(diagnostic.phase, SqliteFailurePhase::SourceValidation);
    assert_eq!(diagnostic.artifact, SqliteArtifactKind::PrivateSourceCopy);
    assert!(matches!(
        diagnostic.sqlite_primary_code,
        Some(ffi::SQLITE_CORRUPT) | Some(ffi::SQLITE_NOTADB)
    ));
    assert_eq!(diagnostic.retry, SqliteRetryDecision::DoNotRetryCorrupt);
    assert!(error.is_ctx_owned_corruption());
    assert_eq!(
        parent.snapshot_counters().source_bytes_copied(),
        expected_copy_bytes
    );
    assert_eq!(directory_file_bytes(temp.path()), before);
}

#[test]
fn malformed_private_backup_is_distinguished_from_source_corruption() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "expected");
    let before = directory_file_bytes(temp.path());
    let parent = retain_parent(temp.path());

    let error = open_root_handle_sqlite_source_online_backup_after_backup_for_test(
        &parent,
        OsStr::new("provider.sqlite"),
        |backup_path| {
            let mut backup = OpenOptions::new().write(true).open(backup_path).unwrap();
            backup.seek(SeekFrom::Start(0)).unwrap();
            backup.write_all(&[0_u8; 100]).unwrap();
            backup.sync_all().unwrap();
        },
    )
    .unwrap_err();

    let diagnostic = error.diagnostic().unwrap();
    assert_eq!(diagnostic.phase, SqliteFailurePhase::BackupValidation);
    assert_eq!(diagnostic.artifact, SqliteArtifactKind::PrivateBackup);
    assert!(matches!(
        diagnostic.sqlite_primary_code,
        Some(ffi::SQLITE_CORRUPT) | Some(ffi::SQLITE_NOTADB)
    ));
    assert_eq!(diagnostic.retry, SqliteRetryDecision::DoNotRetryCorrupt);
    assert!(diagnostic.copied_pages > 0);
    assert!(diagnostic.copied_bytes > 0);
    assert_eq!(directory_file_bytes(temp.path()), before);
}

#[test]
fn private_source_copy_cleanup_failure_is_explicit_and_route_fatal() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let _writer = create_persistent_wal(&database);
    let parent = retain_parent(temp.path());
    let moved = temp.path().join("retained-source-copy-for-cleanup-test");

    let error = open_root_handle_sqlite_source_online_backup_after_private_source_copy_for_test(
        &parent,
        OsStr::new("provider.sqlite"),
        |source_copy| {
            let directory = source_copy.parent().unwrap();
            fs::rename(directory, &moved).unwrap();
            fs::write(directory, b"blocks directory cleanup").unwrap();
        },
    )
    .unwrap_err();

    let diagnostic = error.diagnostic().unwrap();
    assert_eq!(diagnostic.phase, SqliteFailurePhase::Cleanup);
    assert_eq!(diagnostic.artifact, SqliteArtifactKind::PrivateSourceCopy);
    assert_eq!(diagnostic.cleanup, SqliteCleanupStatus::Failed);
    assert_eq!(diagnostic.retry, SqliteRetryDecision::RouteFatalResource);
    assert!(error.is_systemic_resource_failure());
    fs::remove_file(error_cleanup_path(&error)).unwrap();
    fs::remove_dir_all(moved).unwrap();
}

#[test]
fn private_backup_cleanup_failure_is_explicit_and_route_fatal() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "expected");
    let parent = retain_parent(temp.path());
    let moved = temp.path().join("retained-backup-for-cleanup-test");

    let error = open_root_handle_sqlite_source_online_backup_after_backup_for_test(
        &parent,
        OsStr::new("provider.sqlite"),
        |backup| {
            let directory = backup.parent().unwrap();
            fs::rename(directory, &moved).unwrap();
            fs::write(directory, b"blocks directory cleanup").unwrap();
        },
    )
    .unwrap_err();

    let diagnostic = error.diagnostic().unwrap();
    assert_eq!(diagnostic.phase, SqliteFailurePhase::Cleanup);
    assert_eq!(diagnostic.artifact, SqliteArtifactKind::PrivateBackup);
    assert_eq!(diagnostic.cleanup, SqliteCleanupStatus::Failed);
    assert_eq!(diagnostic.retry, SqliteRetryDecision::RouteFatalResource);
    assert!(error.is_systemic_resource_failure());
    fs::remove_file(error_cleanup_path(&error)).unwrap();
    fs::remove_dir_all(moved).unwrap();
}

fn error_cleanup_path(error: &SqliteSourceAccessError) -> &Path {
    match error {
        SqliteSourceAccessError::Diagnosed { source, .. } => error_cleanup_path(source),
        SqliteSourceAccessError::ScratchIoUnavailable { path, .. } => path,
        other => panic!("unexpected cleanup error: {other:?}"),
    }
}
