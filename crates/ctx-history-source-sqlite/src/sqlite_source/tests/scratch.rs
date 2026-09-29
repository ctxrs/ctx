use super::*;

#[test]
fn private_staging_open_failures_are_typed_as_scratch_io() {
    let temp = tempdir().unwrap();
    let missing = temp.path().join("missing");
    let error = open_private_sqlite_staging_file(&missing).unwrap_err();
    assert!(matches!(
        error,
        SqliteSourceAccessError::ScratchIoUnavailable {
            operation: "creating a private provider SQLite staging file",
            path,
            source,
        } if path == missing && source.kind() == io::ErrorKind::NotFound
    ));

    fail_next_private_sqlite_staging_operation_for_test(
        SqliteSourceStagingOperationForTest::Open,
        io::ErrorKind::PermissionDenied,
    );
    let error = open_private_sqlite_staging_file(temp.path()).unwrap_err();
    assert!(matches!(
        error,
        SqliteSourceAccessError::ScratchIoUnavailable {
            operation: "creating a private provider SQLite staging file",
            path,
            source,
        } if path == temp.path() && source.kind() == io::ErrorKind::PermissionDenied
    ));
}

#[test]
fn every_post_open_staging_operation_preserves_scratch_io_provenance() {
    let temp = tempdir().unwrap();
    let mut staging = open_private_sqlite_staging_file(temp.path()).unwrap();
    for (operation, expected_operation) in [
        (
            SqliteSourceStagingOperationForTest::Write,
            "writing a private provider SQLite staging file",
        ),
        (
            SqliteSourceStagingOperationForTest::Flush,
            "flushing a private provider SQLite staging file",
        ),
        (
            SqliteSourceStagingOperationForTest::Rewind,
            "rewinding a private provider SQLite staging file",
        ),
        (
            SqliteSourceStagingOperationForTest::Read,
            "reading a private provider SQLite staging file",
        ),
    ] {
        fail_next_private_sqlite_staging_operation_for_test(
            operation,
            io::ErrorKind::PermissionDenied,
        );
        let error = match operation {
            SqliteSourceStagingOperationForTest::Write => staging.write_all(b"staged").unwrap_err(),
            SqliteSourceStagingOperationForTest::Flush => staging.flush().unwrap_err(),
            SqliteSourceStagingOperationForTest::Rewind => staging.rewind().unwrap_err(),
            SqliteSourceStagingOperationForTest::Read => staging
                .reader()
                .read_line(&mut String::new())
                .map(|_| ())
                .unwrap_err(),
            SqliteSourceStagingOperationForTest::Open => unreachable!(),
        };
        assert!(matches!(
            error,
            SqliteSourceAccessError::ScratchIoUnavailable {
                operation,
                path,
                source,
            } if operation == expected_operation
                && path == temp.path()
                && source.kind() == io::ErrorKind::PermissionDenied
        ));
    }
}

#[test]
fn private_scratch_cleanup_failure_is_explicit_and_typed_unavailable() {
    let temp = tempdir().unwrap();
    let provider_root = temp.path().join("provider");
    let data_root = temp.path().join("ctx-data");
    fs::create_dir_all(&provider_root).unwrap();
    let database = provider_root.join("provider.sqlite");
    create_database(&database, "expected");
    let parent = retain_parent_in_data_root(&data_root, &provider_root);
    let snapshot =
        open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite")).unwrap();
    let moved_scratch = temp.path().join("moved-scratch");

    let result: Result<(), SqliteSourceAccessError> = snapshot
        .with_private_scratch_database_after_use_for_test(
            "cleanup-proof-",
            1024 * 1024,
            |_scratch, _path| Ok(()),
            |scratch_directory| {
                fs::rename(scratch_directory, &moved_scratch).unwrap();
                fs::write(scratch_directory, b"blocks remove_dir_all").unwrap();
            },
        );

    let error = result.as_ref().unwrap_err();
    let diagnostic = error.diagnostic().unwrap();
    assert_eq!(diagnostic.phase, SqliteFailurePhase::Cleanup);
    assert_eq!(diagnostic.artifact, SqliteArtifactKind::PrivateScratch);
    assert_eq!(diagnostic.cleanup, SqliteCleanupStatus::Failed);
    assert!(matches!(
        error,
        SqliteSourceAccessError::Diagnosed { source, .. }
            if matches!(
                source.as_ref(),
                SqliteSourceAccessError::ScratchIoUnavailable {
                    operation: "cleaning the private provider SQLite scratch directory",
                    ..
                }
            )
    ));
    assert!(error.is_systemic_resource_failure());
    fs::remove_file(
        data_root
            .join("tmp/provider-sqlite-scratch")
            .read_dir()
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path(),
    )
    .unwrap();
    fs::remove_dir_all(&moved_scratch).unwrap();
    snapshot.finish().unwrap();
}

#[test]
fn scratch_callback_preserves_simultaneous_directory_cleanup_failure() {
    let temp = tempdir().unwrap();
    let provider_root = temp.path().join("provider");
    let data_root = temp.path().join("ctx-data");
    fs::create_dir_all(&provider_root).unwrap();
    create_database(&provider_root.join("provider.sqlite"), "callback");
    let parent = retain_parent_in_data_root(&data_root, &provider_root);
    let snapshot =
        open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite")).unwrap();
    fail_next_private_directory_cleanup_for_test();

    let error = snapshot
        .with_private_scratch_database(
            "callback-cleanup-",
            1024 * 1024,
            |_scratch, _path| -> Result<(), SqliteSourceAccessError> {
                Err(SqliteSourceAccessError::SnapshotUnavailable {
                    reason: "injected scratch callback failure".to_owned(),
                })
            },
        )
        .unwrap_err();

    match error {
        SqliteSourceAccessError::Finalization { primary, cleanup } => {
            assert!(primary
                .to_string()
                .contains("injected scratch callback failure"));
            assert!(cleanup
                .to_string()
                .contains("injected private SQLite directory cleanup failure"));
        }
        other => panic!("expected callback plus cleanup failure, got {other:?}"),
    }
    snapshot.finish().unwrap();
}

#[test]
fn scratch_callback_preserves_simultaneous_connection_close_failure() {
    let temp = tempdir().unwrap();
    let provider_root = temp.path().join("provider");
    let data_root = temp.path().join("ctx-data");
    fs::create_dir_all(&provider_root).unwrap();
    create_database(&provider_root.join("provider.sqlite"), "close");
    let parent = retain_parent_in_data_root(&data_root, &provider_root);
    let snapshot =
        open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite")).unwrap();
    fail_next_private_scratch_close_for_test();

    let error = snapshot
        .with_private_scratch_database(
            "callback-close-",
            1024 * 1024,
            |_scratch, _path| -> Result<(), SqliteSourceAccessError> {
                Err(SqliteSourceAccessError::SnapshotUnavailable {
                    reason: "injected scratch callback failure".to_owned(),
                })
            },
        )
        .unwrap_err();

    match error {
        SqliteSourceAccessError::Finalization { primary, cleanup } => {
            assert!(primary
                .to_string()
                .contains("injected scratch callback failure"));
            assert!(cleanup
                .to_string()
                .contains("closing the private provider SQLite scratch database"));
        }
        other => panic!("expected callback plus close failure, got {other:?}"),
    }
    snapshot.finish().unwrap();
}

#[test]
fn scratch_open_preserves_simultaneous_directory_cleanup_failure() {
    let temp = tempdir().unwrap();
    let provider_root = temp.path().join("provider");
    let data_root = temp.path().join("ctx-data");
    fs::create_dir_all(&provider_root).unwrap();
    create_database(&provider_root.join("provider.sqlite"), "open");
    let parent = retain_parent_in_data_root(&data_root, &provider_root);
    let snapshot =
        open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite")).unwrap();
    fail_next_private_scratch_open_for_test();
    fail_next_private_directory_cleanup_for_test();

    let error = snapshot
        .with_private_scratch_database(
            "open-cleanup-",
            1024 * 1024,
            |_scratch, _path| -> Result<(), SqliteSourceAccessError> { Ok(()) },
        )
        .unwrap_err();

    match error {
        SqliteSourceAccessError::Finalization { primary, cleanup } => {
            assert!(primary
                .to_string()
                .contains("creating the private provider SQLite scratch database"));
            assert!(cleanup
                .to_string()
                .contains("injected private SQLite directory cleanup failure"));
        }
        other => panic!("expected open plus cleanup failure, got {other:?}"),
    }
    snapshot.finish().unwrap();
}
