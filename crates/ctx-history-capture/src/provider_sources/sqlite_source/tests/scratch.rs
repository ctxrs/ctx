use super::*;

#[test]
fn private_scratch_cleanup_failure_is_explicit_and_typed_unavailable() {
    let temp = tempfile::tempdir().unwrap();
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
    assert_eq!(diagnostic.retry, SqliteRetryDecision::RouteFatalResource);
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
    assert!(error.is_retryable_resource_unavailable());
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
