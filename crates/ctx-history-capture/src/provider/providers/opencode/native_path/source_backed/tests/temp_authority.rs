use std::{collections::BTreeMap, fs, path::Path};

#[cfg(unix)]
use std::{os::unix::fs::MetadataExt, process::Command};

use rusqlite::{params, Connection};
use serde_json::json;

use super::*;

fn assert_batched_ordering(scan: &OpenCodeSourceBackedScan, rows: u64) {
    assert!(scan.bounds.fallback_disk_sort);
    assert_eq!(scan.bounds.fallback_sort_rows, rows);
    assert_eq!(scan.bounds.fallback_payload_hydrations, rows);
    assert!(scan.bounds.max_sort_key_batch_rows <= OPENCODE_HYDRATION_BATCH_ROWS as u64);
    assert!(scan.bounds.max_buffered_payload_rows <= OPENCODE_HYDRATION_BATCH_ROWS as u64);
    assert!(scan.bounds.max_buffered_payload_bytes <= OPENCODE_HYDRATION_BATCH_BYTES);
    assert_eq!(
        scan.bounds.ordering_data_statements,
        2 + scan.bounds.ordering_sort_key_batches + scan.bounds.ordering_hydration_batches
    );
    if rows >= 64 {
        assert!(scan.bounds.ordering_data_statements < rows / 8);
        assert!(scan.bounds.ordering_sort_key_batches < rows / 8);
        assert!(scan.bounds.ordering_hydration_batches < rows / 8);
    }
}

fn drop_message_part_stream_indexes(path: &Path) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "drop index message_session_time_created_id_idx;
             drop index part_message_id_id_idx;
             drop index part_message_time_id_idx;
             drop index part_session_idx;",
        )
        .unwrap();
}

fn directory_file_bytes(path: &Path) -> BTreeMap<std::ffi::OsString, Vec<u8>> {
    fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (entry.file_name(), fs::read(entry.path()).unwrap())
        })
        .collect()
}

#[cfg(unix)]
fn directory_write_stamp(path: &Path) -> (i64, i64, i64, i64) {
    let metadata = fs::metadata(path).unwrap();
    (
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

#[test]
fn indexed_message_part_partial_sort_routes_to_private_bounded_scratch() {
    const PARTS: u64 = 4_096;
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("source/opencode.sqlite");
    create_indexed_message_part_fixture(&database, 0);
    let payload_padding = "p".repeat(4 * 1024);
    let mut connection = Connection::open(&database).unwrap();
    connection
        .execute(
            "insert into message values ('message-1', 'session-1', 0, 0, ?1)",
            [json!({"role": "user", "time": {"created": 0}}).to_string()],
        )
        .unwrap();
    let transaction = connection.transaction().unwrap();
    for sequence in 0..PARTS {
        let time = i64::try_from(PARTS - sequence).unwrap();
        transaction
            .execute(
                "insert into part values (?1, 'message-1', 'session-1', ?2, ?2, ?3)",
                params![
                    format!("part-{sequence:08}"),
                    time,
                    json!({
                        "type": "text",
                        "text": format!("event-{sequence}-{payload_padding}")
                    })
                    .to_string()
                ],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
    drop(connection);

    let (_, direct_scan, direct_records) = scan_current_schema(&database);
    assert_batched_ordering(&direct_scan, PARTS);

    let connection = Connection::open(&database).unwrap();
    connection
        .execute("drop index part_message_time_id_idx", [])
        .unwrap();
    let dialect = &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT;
    let schema = OpenCodeNativeSchema::probe(&connection, dialect).unwrap();
    assert!(schema.message_part_indexed_streaming);
    drop(connection);

    let (_, external_scan, external_records) = scan_current_schema(&database);
    assert_eq!(external_records, direct_records);
    assert_eq!(external_scan.source, direct_scan.source);
    assert_eq!(external_scan.certificate, direct_scan.certificate);
    assert_batched_ordering(&external_scan, PARTS);
    assert!(external_scan.bounds.fallback_scratch_bytes > 0);
}

#[test]
fn multisession_missing_index_fallback_is_equivalent_bounded_and_read_only() {
    const SESSIONS: u64 = 2_048;
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("source/opencode.sqlite");
    create_multisession_message_part_fixture(&database, SESSIONS as i64);

    let (_, indexed_scan, indexed_records) = scan_current_schema(&database);
    assert_eq!(indexed_scan.bounds.session_rows_scanned, SESSIONS);
    assert_eq!(indexed_scan.bounds.session_metadata_loads, SESSIONS);
    assert_eq!(indexed_scan.bounds.max_buffered_session_metadata, 1);
    assert_eq!(indexed_scan.bounds.max_session_ancestry_depth, 16);
    assert_batched_ordering(&indexed_scan, SESSIONS);

    drop_message_part_stream_indexes(&database);
    let connection = Connection::open(&database).unwrap();
    let dialect = &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT;
    let fallback_schema = OpenCodeNativeSchema::probe(&connection, dialect).unwrap();
    assert!(!fallback_schema.message_part_indexed_streaming);
    let mut fallback_sql = source_backed_event_sql(&fallback_schema);
    fallback_sql.push_str(source_backed_event_order_sql(&fallback_schema));
    let fallback_plan = connection
        .prepare(&format!("EXPLAIN QUERY PLAN {fallback_sql}"))
        .unwrap()
        .query_map([], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(
        fallback_plan
            .iter()
            .any(|step| step.contains("USE TEMP B-TREE FOR ORDER BY")),
        "missing-index fixture did not exercise the fallback sorter: {fallback_plan:?}"
    );
    let sort_key_plan = connection
        .prepare(&format!(
            "EXPLAIN QUERY PLAN {}",
            source_backed_fallback_sort_key_sql(&fallback_schema)
        ))
        .unwrap()
        .query_map([], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(
        sort_key_plan
            .iter()
            .all(|step| !step.contains("USE TEMP B-TREE")),
        "replacement key scan must not retain the ambient SQLite sorter: {sort_key_plan:?}"
    );
    drop(connection);

    let before_database = fs::read(&database).unwrap();
    let mut before_siblings = fs::read_dir(database.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    before_siblings.sort();
    let (_, fallback_scan, fallback_records) = scan_current_schema(&database);
    let mut after_siblings = fs::read_dir(database.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    after_siblings.sort();

    assert_eq!(fallback_records, indexed_records);
    assert_eq!(fallback_scan.source, indexed_scan.source);
    assert_eq!(fallback_scan.certificate, indexed_scan.certificate);
    assert_eq!(
        fallback_scan.certificate.counts(),
        indexed_scan.certificate.counts()
    );
    assert_eq!(
        fallback_scan.certificate.content_digest(),
        indexed_scan.certificate.content_digest()
    );
    assert_eq!(fallback_scan.bounds.session_rows_scanned, SESSIONS);
    assert_eq!(fallback_scan.bounds.session_metadata_loads, SESSIONS);
    assert_eq!(fallback_scan.bounds.max_buffered_session_metadata, 1);
    assert_eq!(fallback_scan.bounds.max_session_ancestry_depth, 16);
    assert_batched_ordering(&fallback_scan, SESSIONS);
    assert!(fallback_scan.bounds.fallback_scratch_bytes > 0);
    assert_eq!(fs::read(&database).unwrap(), before_database);
    assert_eq!(after_siblings, before_siblings);
}

#[cfg(unix)]
#[test]
fn fallback_external_sort_ignores_ambient_sqlite_tmpdir_without_transient_provider_writes() {
    const CHILD_MARKER: &str = "CTX_TEST_OPENCODE_AMBIENT_TMP_CHILD";
    const CHILD_ROOT: &str = "CTX_TEST_OPENCODE_AMBIENT_TMP_ROOT";
    if std::env::var_os(CHILD_MARKER).is_some() {
        const SESSIONS: u64 = 8_192;
        let root = std::path::PathBuf::from(std::env::var_os(CHILD_ROOT).unwrap());
        let provider = root.join("provider");
        let database = provider.join("opencode.sqlite");
        let data_root = root.join("ctx-data");
        create_multisession_message_part_fixture(&database, SESSIONS as i64);
        drop_message_part_stream_indexes(&database);
        let before_bytes = directory_file_bytes(&provider);
        let before_stamp = directory_write_stamp(&provider);

        let (_, scan, records) =
            scan_current_schema_result(&database, &data_root, OPENCODE_FALLBACK_SCRATCH_MAX_BYTES)
                .unwrap();

        assert_eq!(records.len() as u64, SESSIONS);
        assert_batched_ordering(&scan, SESSIONS);
        assert!(
            scan.bounds.fallback_scratch_bytes > 512 * 1024,
            "fixture must exceed the fixed scratch page cache and exercise disk-backed ordering"
        );
        assert_eq!(directory_file_bytes(&provider), before_bytes);
        assert_eq!(directory_write_stamp(&provider), before_stamp);
        let scratch_root = data_root.join("tmp/provider-sqlite-scratch");
        assert_eq!(fs::read_dir(scratch_root).unwrap().count(), 0);
        return;
    }

    let temp = crate::test_support_paths::tempdir().unwrap();
    let provider = temp.path().join("provider");
    fs::create_dir_all(&provider).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .arg(
            "fallback_external_sort_ignores_ambient_sqlite_tmpdir_without_transient_provider_writes",
        )
        .arg("--nocapture")
        .env(CHILD_MARKER, "1")
        .env(CHILD_ROOT, temp.path())
        .env("SQLITE_TMPDIR", &provider)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "isolated SQLITE_TMPDIR proof failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn fallback_scratch_enospc_is_typed_and_preserves_the_provider() {
    const SESSIONS: u64 = 2_048;
    const SCRATCH_LIMIT: u64 = 64 * 1024;
    let temp = crate::test_support_paths::tempdir().unwrap();
    let provider = temp.path().join("provider");
    let database = provider.join("opencode.sqlite");
    let data_root = temp.path().join("ctx-data");
    create_multisession_message_part_fixture(&database, SESSIONS as i64);
    drop_message_part_stream_indexes(&database);
    let before = directory_file_bytes(&provider);

    let error = scan_current_schema_result(&database, &data_root, SCRATCH_LIMIT).unwrap_err();

    match error {
        OpenCodeSourceBackedError::SqliteSource(SqliteSourceAccessError::Sqlite {
            operation,
            source: rusqlite::Error::SqliteFailure(error, _),
        }) => {
            assert_eq!(operation, "writing the private OpenCode ordering index");
            assert_eq!(error.code, rusqlite::ErrorCode::DiskFull);
        }
        other => panic!("unexpected bounded-scratch error: {other:?}"),
    }
    assert_eq!(directory_file_bytes(&provider), before);
    let scratch_root = data_root.join("tmp/provider-sqlite-scratch");
    assert_eq!(fs::read_dir(scratch_root).unwrap().count(), 0);
}

#[test]
fn unwritable_fallback_scratch_root_is_typed_and_preserves_the_provider() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let provider = temp.path().join("provider");
    let database = provider.join("opencode.sqlite");
    let data_root = temp.path().join("ctx-data");
    create_multisession_message_part_fixture(&database, 64);
    drop_message_part_stream_indexes(&database);
    let before = directory_file_bytes(&provider);
    let authorized = open_root_authorized_snapshot_retained(&data_root, &database).unwrap();
    let observation = observe_logical_source(
        authorized.sqlite_snapshot.connection().unwrap(),
        &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT,
    )
    .unwrap();
    fs::write(
        data_root.join("tmp/provider-sqlite-scratch"),
        b"not a directory",
    )
    .unwrap();

    let error = scan_pinned_source(
        &database,
        &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT,
        &observation,
        authorized.sqlite_snapshot,
        &mut |_| Ok(()),
    )
    .unwrap_err();

    match error {
        OpenCodeSourceBackedError::SqliteSource(SqliteSourceAccessError::Io {
            operation, ..
        }) => assert_eq!(
            operation,
            "creating the private provider SQLite scratch root"
        ),
        other => panic!("unexpected unavailable-scratch error: {other:?}"),
    }
    assert_eq!(directory_file_bytes(&provider), before);
}
