use super::super::resources::override_next_scratch_available_space_for_test;
use super::*;

fn running_as_unix_root() -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn selected(authority: &SqliteSourceDirectoryAuthority) -> SqliteSourceReadSnapshot {
    authority
        .open_selected_tables_snapshot_with_progress(
            OsStr::new("provider.sqlite"),
            &["messages"],
            |_| Ok::<_, ()>(()),
        )
        .unwrap()
}

#[test]
fn selective_copy_admits_conversation_when_unrelated_table_exceeds_free_space() {
    let temp = tempdir().unwrap();
    let data = tempdir().unwrap();
    let path = temp.path().join("provider.sqlite");
    create_database(&path, "retained conversation");
    let writer = Connection::open(&path).unwrap();
    writer
        .execute_batch(
            "CREATE TABLE event(data BLOB);
        INSERT INTO event VALUES (zeroblob(64 * 1024 * 1024));
        CREATE INDEX messages_body ON messages(body);",
        )
        .unwrap();
    drop(writer);
    let before = directory_file_state(temp.path());
    let authority = retain_parent_in_data_root(data.path(), temp.path());
    override_next_scratch_available_space_for_test(32 * 1024 * 1024);
    assert!(authority
        .open_stable_snapshot(OsStr::new("provider.sqlite"))
        .unwrap_err()
        .is_snapshot_capacity_failure());
    override_next_scratch_available_space_for_test(128 * 1024 * 1024);
    let start = Instant::now();
    let full = authority
        .open_stable_snapshot(OsStr::new("provider.sqlite"))
        .unwrap();
    let full_elapsed = start.elapsed();
    let full_bytes = fs::metadata(full.snapshot_directory().unwrap().join("source.sqlite"))
        .unwrap()
        .len();
    assert_eq!(read_values(&full), ["retained conversation"]);
    full.finish().unwrap();
    override_next_scratch_available_space_for_test(32 * 1024 * 1024);
    let start = Instant::now();
    let snapshot = selected(&authority);
    let selected_elapsed = start.elapsed();
    assert_eq!(
        snapshot.strategy(),
        SqliteSourceSnapshotStrategy::SelectiveTables
    );
    assert_eq!(read_values(&snapshot), ["retained conversation"]);
    assert!(snapshot.admitted_revision_is_replay_safe());
    let connection = snapshot.connection().unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name='event'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name='messages_body'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    let bytes = fs::metadata(snapshot.snapshot_directory().unwrap().join("source.sqlite"))
        .unwrap()
        .len();
    assert!(bytes < 64 * 1024, "selected snapshot used {bytes} bytes");
    eprintln!("full capture: {full_bytes} bytes in {full_elapsed:?}; selected capture: {bytes} bytes in {selected_elapsed:?}");
    snapshot.finish().unwrap();
    assert_eq!(directory_file_state(temp.path()), before);
    assert_eq!(staging_entries(data.path()), 0);
}

#[test]
fn selective_wal_read_is_no_write_and_private_after_source_sidecars_disappear() {
    if running_as_unix_root() {
        return;
    }
    let temp = tempdir().unwrap();
    let data = tempdir().unwrap();
    let path = temp.path().join("provider.sqlite");
    let writer = PersistentWalWriterProcess::start(&path, &data.path().join("ready"));
    let before = directory_file_state(temp.path());
    let authority = retain_parent_in_data_root(data.path(), temp.path());
    let snapshot = selected(&authority);
    assert_eq!(directory_file_state(temp.path()), before);
    drop(writer);
    for suffix in ["-wal", "-shm"] {
        let _ = fs::remove_file(temp.path().join(format!("provider.sqlite{suffix}")));
    }
    assert_eq!(read_values(&snapshot), ["from-wal"]);
    snapshot.finish().unwrap();
    assert_eq!(staging_entries(data.path()), 0);
}

#[test]
fn selected_tables_share_one_transaction_even_when_later_table_changes() {
    if running_as_unix_root() {
        return;
    }
    let temp = tempdir().unwrap();
    let data = tempdir().unwrap();
    let path = temp.path().join("provider.sqlite");
    let writer = create_persistent_wal(&path);
    writer
        .execute_batch(
            "CREATE TABLE later(value TEXT);
        INSERT INTO later VALUES ('before');
        WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<4096)
        INSERT INTO messages SELECT 'row-'||x FROM n;",
        )
        .unwrap();
    let authority = retain_parent_in_data_root(data.path(), temp.path());
    let mut changed = false;
    let snapshot = authority
        .open_selected_tables_snapshot_with_progress(
            OsStr::new("provider.sqlite"),
            &["messages", "later"],
            |progress| {
                if !changed && progress.snapshot_bytes_completed.unwrap_or_default() > 0 {
                    writer
                        .execute("UPDATE later SET value='after'", [])
                        .unwrap();
                    changed = true;
                }
                Ok::<_, ()>(())
            },
        )
        .unwrap();
    assert!(changed);
    assert!(!snapshot.admitted_revision_is_replay_safe());
    assert_eq!(
        snapshot
            .connection()
            .unwrap()
            .query_row("SELECT value FROM later", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "before"
    );
    snapshot.finish().unwrap();
    let next = authority
        .open_selected_tables_snapshot_with_progress(
            OsStr::new("provider.sqlite"),
            &["later"],
            |_| Ok::<_, ()>(()),
        )
        .unwrap();
    assert_eq!(
        next.connection()
            .unwrap()
            .query_row("SELECT value FROM later", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "after"
    );
    next.finish().unwrap();
}

#[test]
fn selective_cancel_and_disk_bound_clean_up_without_provider_mutation() {
    let temp = tempdir().unwrap();
    let data = tempdir().unwrap();
    let path = temp.path().join("provider.sqlite");
    create_database(&path, "conversation");
    let writer = Connection::open(&path).unwrap();
    writer
        .execute("INSERT INTO messages VALUES (zeroblob(1024*1024))", [])
        .unwrap();
    drop(writer);
    let before = directory_file_state(temp.path());
    let authority = retain_parent_in_data_root(data.path(), temp.path());
    let cancelled = authority.open_selected_tables_snapshot_with_progress(
        OsStr::new("provider.sqlite"),
        &["messages"],
        |_| Err("cancelled"),
    );
    assert!(matches!(
        cancelled,
        Err(SqliteSourceProgressError::Progress("cancelled"))
    ));
    assert_eq!(staging_entries(data.path()), 0);
    override_next_scratch_available_space_for_test(16 * 1024 * 1024 + 32 * 1024);
    let failed = authority.open_selected_tables_snapshot_with_progress(
        OsStr::new("provider.sqlite"),
        &["messages"],
        |_| Ok::<_, ()>(()),
    );
    assert!(
        matches!(failed, Err(SqliteSourceProgressError::Source(error))
        if error.is_systemic_resource_failure())
    );
    assert_eq!(staging_entries(data.path()), 0);
    assert_eq!(directory_file_state(temp.path()), before);
    // A failed candidate must not poison the next attempt after space returns.
    override_next_scratch_available_space_for_test(32 * 1024 * 1024);
    let snapshot = selected(&authority);
    assert_eq!(
        snapshot
            .connection()
            .unwrap()
            .query_row(
                "SELECT count(*),length(body) FROM messages WHERE typeof(body)='blob'",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .unwrap(),
        (1, 1024 * 1024)
    );
    snapshot.finish().unwrap();
    assert_eq!(staging_entries(data.path()), 0);
    assert_eq!(directory_file_state(temp.path()), before);
}

#[test]
fn selected_schema_values_and_native_rowids_are_preserved() {
    let temp = tempdir().unwrap();
    let data = tempdir().unwrap();
    let path = temp.path().join("provider.sqlite");
    let source = Connection::open(&path).unwrap();
    source.execute_batch("PRAGMA foreign_keys=OFF; CREATE TABLE messages (
        id TEXT PRIMARY KEY, body TEXT, integer_value INTEGER, real_value REAL, bytes BLOB,
        parent TEXT REFERENCES omitted(id), CHECK(json_valid(body)));
        CREATE INDEX by_body ON messages(body COLLATE NOCASE DESC) WHERE body IS NOT NULL;
        CREATE TABLE omitted(id TEXT PRIMARY KEY);
        CREATE TRIGGER ignored AFTER INSERT ON messages BEGIN
            INSERT INTO omitted VALUES (new.id); END;
        PRAGMA ignore_check_constraints=ON;
        INSERT INTO messages(rowid,id,body,integer_value,real_value,bytes,parent)
            VALUES(-7, 'one', '{malformed', -9223372036854775808, 1.25, x'00ff80', 'absent');
        INSERT INTO messages(rowid,id,body,integer_value,real_value,bytes,parent)
            VALUES(9223372036854775807, NULL, CAST(x'ff80' AS TEXT), 'bad-number', NULL, NULL, NULL);
        CREATE TABLE aliases(id INTEGER PRIMARY KEY, data BLOB);
        INSERT INTO aliases VALUES(314, x'808100');").unwrap();
    let before = directory_file_state(temp.path());
    let authority = retain_parent_in_data_root(data.path(), temp.path());
    let snapshot = authority
        .open_selected_tables_snapshot_with_progress(
            OsStr::new("provider.sqlite"),
            &["messages", "aliases"],
            |_| Ok::<_, ()>(()),
        )
        .unwrap();
    let copied = snapshot.connection().unwrap();
    // SQL values are compared as storage-class tags and raw cell bytes, including
    // invalid UTF-8 TEXT, BLOBs, NULLs, malformed JSON and non-integer values.
    fn cells(connection: &Connection, sql: &str) -> Vec<Vec<(i32, Vec<u8>)>> {
        use rusqlite::types::ValueRef;
        let mut query = connection.prepare(sql).unwrap();
        let columns = query.column_count();
        query
            .query_map([], |row| {
                Ok((0..columns)
                    .map(|column| match row.get_ref(column).unwrap() {
                        ValueRef::Null => (0, vec![]),
                        ValueRef::Integer(value) => (1, value.to_le_bytes().to_vec()),
                        ValueRef::Real(value) => (2, value.to_bits().to_le_bytes().to_vec()),
                        ValueRef::Text(value) => (3, value.to_vec()),
                        ValueRef::Blob(value) => (4, value.to_vec()),
                    })
                    .collect())
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }
    for table in ["messages", "aliases"] {
        let sql = format!("SELECT rowid,* FROM {table} ORDER BY rowid");
        assert_eq!(cells(&source, &sql), cells(copied, &sql));
        let schema = format!(
            "SELECT type,name,sql FROM sqlite_schema WHERE tbl_name='{table}' AND type IN ('table','index') ORDER BY name"
        );
        assert_eq!(cells(&source, &schema), cells(copied, &schema));
    }
    assert_eq!(
        copied
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE type='trigger'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    snapshot.finish().unwrap();
    assert_eq!(directory_file_state(temp.path()), before);
}

#[test]
fn selected_schema_execution_cannot_attach_or_change_unrelated_objects() {
    use super::super::snapshot::selective::execute_schema;
    let target = Connection::open_in_memory().unwrap();
    for sql in [
        "ATTACH ':memory:' AS other",
        "CREATE TABLE unrelated(value)",
        "PRAGMA writable_schema=ON",
        "CREATE TRIGGER trap AFTER INSERT ON messages BEGIN SELECT 1; END",
    ] {
        assert!(
            execute_schema(&target, "messages", sql).is_err(),
            "accepted {sql}"
        );
        target
            .execute_batch(
                "CREATE TABLE scratch_after_denial(value); DROP TABLE scratch_after_denial;",
            )
            .expect("a denied schema statement must clear its authorizer");
    }
    execute_schema(&target, "messages", "CREATE TABLE messages(value)").unwrap();
    target
        .execute("CREATE TABLE scratch_after_success(value)", [])
        .expect("an allowed schema statement must clear its authorizer");
    assert!(execute_schema(
        &target,
        "messages",
        "CREATE INDEX custom_collation ON messages(value COLLATE not_installed)"
    )
    .is_err());
    assert!(execute_schema(
        &target,
        "messages",
        "CREATE INDEX custom_function ON messages(not_installed(value))"
    )
    .is_err());
    assert!(execute_schema(
        &target,
        "messages",
        "CREATE INDEX foreign_table ON messages((SELECT value FROM other))"
    )
    .is_err());
}

#[test]
fn selected_read_preserves_uri_leaf_and_caller_directory() {
    for wal in [false, true] {
        if wal && running_as_unix_root() {
            continue;
        }
        let temp = tempdir().unwrap();
        let data = tempdir().unwrap();
        let name = OsStr::new(if cfg!(windows) {
            "provider #%.sqlite"
        } else {
            "provider #?%.sqlite"
        });
        let path = temp.path().join(name);
        let writer = if wal {
            Some(PersistentWalWriterProcess::start(
                &path,
                &data.path().join("ready"),
            ))
        } else {
            create_database(&path, "ordinary");
            None
        };
        let before = directory_file_state(temp.path());
        let cwd = std::env::current_dir().unwrap();
        let snapshot = retain_parent_in_data_root(data.path(), temp.path())
            .open_selected_tables_snapshot_with_progress(name, &["messages"], |_| Ok::<_, ()>(()))
            .unwrap();
        assert_eq!(std::env::current_dir().unwrap(), cwd);
        assert_eq!(
            snapshot.strategy(),
            SqliteSourceSnapshotStrategy::SelectiveTables
        );
        assert_eq!(
            read_values(&snapshot),
            [if wal { "from-wal" } else { "ordinary" }]
        );
        snapshot.finish().unwrap();
        assert_eq!(directory_file_state(temp.path()), before);
        drop(writer);
    }
}

#[test]
fn sidecar_free_capture_rejects_changes_during_copy_and_cleans_candidate() {
    let temp = tempdir().unwrap();
    let data = tempdir().unwrap();
    let path = temp.path().join("provider.sqlite");
    create_database(&path, "before");
    let writer = Connection::open(&path).unwrap();
    writer
        .execute_batch(
            "CREATE TABLE later(body TEXT); INSERT INTO later VALUES ('before');
        WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<4096)
        INSERT INTO messages SELECT 'row-'||x FROM n;",
        )
        .unwrap();
    let authority = retain_parent_in_data_root(data.path(), temp.path());
    let mut changed = false;
    let captured = authority.open_selected_tables_snapshot_with_progress(
        OsStr::new("provider.sqlite"),
        &["messages", "later"],
        |progress| {
            if !changed && progress.snapshot_bytes_completed.unwrap_or_default() > 0 {
                writer.execute("UPDATE later SET body='after'", []).unwrap();
                changed = true;
            }
            Ok::<_, ()>(())
        },
    );
    assert!(changed);
    assert!(
        matches!(captured, Err(SqliteSourceProgressError::Source(error)) if error.is_source_changed())
    );
    assert_eq!(staging_entries(data.path()), 0);
    selected(&authority).finish().unwrap();
}

#[test]
fn wal_without_shared_memory_keeps_the_private_family_fallback() {
    let original = tempdir().unwrap();
    let _writer = create_persistent_wal(&original.path().join("provider.sqlite"));
    let temp = tempdir().unwrap();
    let data = tempdir().unwrap();
    for name in ["provider.sqlite", "provider.sqlite-wal"] {
        fs::copy(original.path().join(name), temp.path().join(name)).unwrap();
    }
    let before = directory_file_state(temp.path());
    let snapshot = selected(&retain_parent_in_data_root(data.path(), temp.path()));
    assert_eq!(
        snapshot.strategy(),
        SqliteSourceSnapshotStrategy::CopiedFamily
    );
    assert_eq!(read_values(&snapshot), ["from-wal"]);
    snapshot.finish().unwrap();
    assert_eq!(directory_file_state(temp.path()), before);
    assert_eq!(staging_entries(data.path()), 0);
}

#[test]
fn selected_capture_rejects_or_prevents_source_replacement_and_releases_handles() {
    for replace_parent in [false, true] {
        let temp = tempdir().unwrap();
        let data = tempdir().unwrap();
        let parent = temp.path().join("source");
        fs::create_dir(&parent).unwrap();
        let path = parent.join("provider.sqlite");
        create_database(&path, "retained");
        let authority = retain_parent_in_data_root(data.path(), &parent);
        let from = if replace_parent { &parent } else { &path };
        let moved = temp.path().join("moved");
        let mut attempted = false;
        let captured = authority.open_selected_tables_snapshot_with_progress(
            OsStr::new("provider.sqlite"),
            &["messages"],
            |_| {
                if !attempted {
                    let renamed = fs::rename(from, &moved);
                    #[cfg(windows)]
                    assert!(renamed.is_err(), "live source path must remain pinned");
                    #[cfg(unix)]
                    renamed.unwrap();
                    attempted = true;
                }
                Ok::<_, ()>(())
            },
        );
        assert!(attempted);
        #[cfg(windows)]
        {
            let snapshot = captured.unwrap();
            assert_eq!(read_values(&snapshot), ["retained"]);
            snapshot.finish().unwrap();
            fs::rename(from, &moved).expect("reader must release its no-delete handles");
        }
        #[cfg(unix)]
        assert!(
            matches!(captured, Err(SqliteSourceProgressError::Source(error)) if error.is_source_changed())
        );
        assert_eq!(staging_entries(data.path()), 0);
    }
}
