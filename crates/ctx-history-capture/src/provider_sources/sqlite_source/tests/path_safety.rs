use super::*;

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
    assert!(matches!(&result, Err(error) if error.is_source_changed()));
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

    let result = open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite"));
    assert!(matches!(
        result,
        Err(SqliteSourceAccessError::UnsupportedSidecarIdentity {
            component: SqliteSourceComponent::RollbackJournal,
            ..
        })
    ));
}
