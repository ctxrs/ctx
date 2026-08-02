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

    assert!(matches!(
        result,
        Err(SqliteSourceAccessError::ScratchIoUnavailable {
            operation: "cleaning the private provider SQLite scratch directory",
            ..
        })
    ));
    assert!(result
        .as_ref()
        .unwrap_err()
        .is_retryable_resource_unavailable());
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
