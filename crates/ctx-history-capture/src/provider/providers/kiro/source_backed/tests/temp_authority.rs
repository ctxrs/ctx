use std::{collections::BTreeMap, fs, os::unix::fs::MetadataExt, path::Path, process::Command};

use ctx_history_core::CaptureProvider;
use ctx_history_index::WriterOptions;
use rusqlite::{params, Connection};
use serde_json::json;

use super::super::{kiro_source_key, scan_kiro_snapshot, KiroSourceBackedErrorV0};
use crate::{
    provider::providers::kiro::native_path::{
        scan::{
            KIRO_HYDRATION_BATCH_BYTES, KIRO_HYDRATION_BATCH_ROWS,
            KIRO_HYDRATION_SINGLETON_MAX_BYTES, KIRO_KEY_BATCH_BYTES, KIRO_KEY_BATCH_ROWS,
            KIRO_NATIVE_KEY_MAX_BYTES, KIRO_ORDER_SCRATCH_MAX_BYTES,
        },
        KiroSqliteDatabase,
    },
    provider::source_backed::{
        refresh_source_backed_generation, SourceBackedCoordinatorError,
        SourceBackedProviderRegistry, SourceBackedRouteErrorKind, SourceBackedRouteSelection,
    },
    provider_sources::{
        fail_next_opened_snapshot_cleanup_for_test, provider_source_for_path,
        SqliteSourceAccessError,
    },
};

const LARGE_ROWS: u64 = 8_192;

fn create_fixture(path: &Path, rows: u64) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "create table conversations_v2 (
                 key text not null,
                 conversation_id text not null,
                 value text not null,
                 created_at integer,
                 updated_at integer
             )",
        )
        .unwrap();
    let transaction = connection.transaction().unwrap();
    let payload = json!({
        "history": [{
            "user": {
                "content": {"Prompt": {"prompt": "bounded Kiro event"}},
                "timestamp": "2026-08-02T00:00:00Z"
            }
        }]
    })
    .to_string();
    for sequence in 0..rows {
        let descending = rows - sequence;
        transaction
            .execute(
                "insert into conversations_v2 values (?1, ?2, ?3, ?4, ?4)",
                params![
                    format!("/workspace/{descending:08}/{}", "k".repeat(96)),
                    format!("conversation-{sequence:08}"),
                    payload,
                    i64::try_from(sequence).unwrap(),
                ],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
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

fn directory_write_stamp(path: &Path) -> (i64, i64, i64, i64) {
    let metadata = fs::metadata(path).unwrap();
    (
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

fn scan_fixture(
    database_path: &Path,
    data_root: &Path,
    scratch_limit: u64,
) -> Result<(super::super::KiroSourceBackedScan, u64), KiroSourceBackedErrorV0> {
    let database = KiroSqliteDatabase::open(data_root, database_path)?;
    let source = kiro_source_key()?;
    let terminal_fence = database.evidence().clone();
    let mut emitted = 0_u64;
    let scan = database.with_private_scratch_database(
        "kiro-order-test-",
        scratch_limit,
        |scratch, scratch_path| {
            scan_kiro_snapshot(
                database.connection(database_path)?,
                scratch,
                scratch_path,
                source,
                terminal_fence,
                &mut |page| {
                    emitted = emitted
                        .checked_add(u64::try_from(page.len()).unwrap())
                        .unwrap();
                    Ok(())
                },
            )
        },
    )?;
    database.revalidate(database_path)?;
    database.finish(database_path)?;
    Ok((scan, emitted))
}

fn assert_bounded_ordering(scan: &super::super::KiroSourceBackedScan, rows: u64) {
    let ordering = scan.ordering;
    assert_eq!(ordering.rows, rows);
    assert_eq!(scan.decoded_rows, rows);
    assert!(ordering.max_key_batch_rows <= KIRO_KEY_BATCH_ROWS as u64);
    assert!(ordering.max_key_batch_bytes <= KIRO_KEY_BATCH_BYTES as u64);
    assert!(ordering.max_hydration_batch_rows <= KIRO_HYDRATION_BATCH_ROWS as u64);
    assert!(ordering.max_hydration_batch_bytes <= KIRO_HYDRATION_SINGLETON_MAX_BYTES);
    if ordering.max_hydration_batch_bytes > KIRO_HYDRATION_BATCH_BYTES {
        assert_eq!(ordering.max_hydration_batch_rows, 1);
    }
    assert_eq!(
        ordering.data_statements,
        ordering.phases * 4 + ordering.key_batches + ordering.hydration_batches
    );
    assert!(ordering.data_statements < rows / 8);
    assert!(ordering.key_batches < rows / 8);
    assert!(ordering.hydration_batches < rows / 8);
}

#[test]
fn kiro_valid_payload_over_rollover_target_is_one_finite_singleton() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("provider/kiro.sqlite");
    let data_root = temp.path().join("ctx-data");
    fs::create_dir_all(database.parent().unwrap()).unwrap();
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "create table conversations_v2 (
                 key text not null,
                 conversation_id text not null,
                 value text not null,
                 created_at integer,
                 updated_at integer
             )",
        )
        .unwrap();
    let payload = json!({
        "history": [{
            "user": {
                "content": {"Prompt": {"prompt": "p".repeat(9 * 1024 * 1024)}},
                "timestamp": "2026-08-02T00:00:00Z"
            }
        }]
    })
    .to_string();
    assert!(payload.len() as u64 > KIRO_HYDRATION_BATCH_BYTES);
    connection
        .execute(
            "insert into conversations_v2 values ('/workspace/large', 'large-conversation', ?1, 1, 1)",
            [payload],
        )
        .unwrap();
    drop(connection);

    let (scan, emitted) =
        scan_fixture(&database, &data_root, KIRO_ORDER_SCRATCH_MAX_BYTES).unwrap();
    assert_eq!(emitted, 1);
    assert_eq!(scan.ordering.max_hydration_batch_rows, 1);
    assert!(scan.ordering.max_hydration_batch_bytes > KIRO_HYDRATION_BATCH_BYTES);
    assert!(scan.ordering.max_hydration_batch_bytes <= KIRO_HYDRATION_SINGLETON_MAX_BYTES);
}

#[test]
fn kiro_key_over_one_mib_is_rejected_before_key_batching() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("provider/kiro.sqlite");
    let data_root = temp.path().join("ctx-data");
    fs::create_dir_all(database.parent().unwrap()).unwrap();
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "create table conversations_v2 (
                 key text not null,
                 conversation_id text not null,
                 value text not null,
                 created_at integer,
                 updated_at integer
             )",
        )
        .unwrap();
    let key = format!("/workspace/{}", "k".repeat(1024 * 1024));
    assert!(key.len() > KIRO_KEY_BATCH_BYTES);
    assert!(key.len() > KIRO_NATIVE_KEY_MAX_BYTES);
    connection
        .execute(
            "insert into conversations_v2 values (?1, 'oversized-key', ?2, 1, 1)",
            params![
                key,
                json!({"history": [{"user": {"content": {"Prompt": {"prompt": "small"}}}}]})
                    .to_string()
            ],
        )
        .unwrap();
    drop(connection);

    let error = scan_fixture(&database, &data_root, KIRO_ORDER_SCRATCH_MAX_BYTES).unwrap_err();
    assert!(matches!(
        error,
        KiroSourceBackedErrorV0::UncertifiableRow {
            reason: "Kiro conversation key exceeds the Core typed-key bound",
            ..
        }
    ));
}

#[test]
fn kiro_external_order_ignores_ambient_sqlite_tmpdir_and_bounds_large_corpus_memory() {
    const CHILD_MARKER: &str = "CTX_TEST_KIRO_AMBIENT_TMP_CHILD";
    const CHILD_ROOT: &str = "CTX_TEST_KIRO_AMBIENT_TMP_ROOT";
    if std::env::var_os(CHILD_MARKER).is_some() {
        let root = std::path::PathBuf::from(std::env::var_os(CHILD_ROOT).unwrap());
        let provider = root.join("provider");
        let database = provider.join("kiro.sqlite");
        let data_root = root.join("ctx-data");
        create_fixture(&database, LARGE_ROWS);
        let legacy_connection = Connection::open(&database).unwrap();
        let legacy_plan = legacy_connection
            .prepare(
                "explain query plan select rowid, key, conversation_id, value,
                                           created_at, updated_at
                   from conversations_v2
                  order by typeof(key), key collate binary, rowid",
            )
            .unwrap()
            .query_map([], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(
            legacy_plan
                .iter()
                .any(|step| step.contains("USE TEMP B-TREE FOR ORDER BY")),
            "fixture must exercise the removed corpus-sized SQLite sorter: {legacy_plan:?}"
        );
        let before_bytes = directory_file_bytes(&provider);
        let before_stamp = directory_write_stamp(&provider);

        let (scan, emitted) =
            scan_fixture(&database, &data_root, KIRO_ORDER_SCRATCH_MAX_BYTES).unwrap();

        assert_eq!(emitted, LARGE_ROWS);
        assert_bounded_ordering(&scan, LARGE_ROWS);
        assert!(
            scan.ordering.scratch_bytes > 512 * 1024,
            "fixture must exceed the fixed scratch page cache"
        );
        assert_eq!(directory_file_bytes(&provider), before_bytes);
        assert_eq!(directory_write_stamp(&provider), before_stamp);
        assert_eq!(
            fs::read_dir(data_root.join("tmp/provider-sqlite-scratch"))
                .unwrap()
                .count(),
            0
        );
        return;
    }

    let temp = crate::test_support_paths::tempdir().unwrap();
    let provider = temp.path().join("provider");
    fs::create_dir_all(&provider).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .arg("kiro_external_order_ignores_ambient_sqlite_tmpdir_and_bounds_large_corpus_memory")
        .arg("--nocapture")
        .env(CHILD_MARKER, "1")
        .env(CHILD_ROOT, temp.path())
        .env("SQLITE_TMPDIR", &provider)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "isolated Kiro SQLITE_TMPDIR proof failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn kiro_bounded_scratch_enospc_is_typed_and_preserves_provider() {
    const SCRATCH_LIMIT: u64 = 64 * 1024;
    let temp = crate::test_support_paths::tempdir().unwrap();
    let provider = temp.path().join("provider");
    let database = provider.join("kiro.sqlite");
    let data_root = temp.path().join("ctx-data");
    create_fixture(&database, 2_048);
    let before = directory_file_bytes(&provider);

    let error = scan_fixture(&database, &data_root, SCRATCH_LIMIT).unwrap_err();

    match &error {
        KiroSourceBackedErrorV0::SqliteSource(
            SqliteSourceAccessError::ScratchSqliteUnavailable {
                operation,
                source: rusqlite::Error::SqliteFailure(error, _),
            },
        ) => {
            assert_eq!(*operation, "writing the private Kiro ordering index");
            assert_eq!(error.code, rusqlite::ErrorCode::DiskFull);
        }
        other => panic!("unexpected bounded Kiro scratch error: {other:?}"),
    }
    assert_eq!(
        super::super::registration::kiro_scan_error(error).kind,
        crate::provider::source_backed::SourceBackedRouteErrorKind::ResourceUnavailable
    );
    assert_eq!(directory_file_bytes(&provider), before);
    assert_eq!(
        fs::read_dir(data_root.join("tmp/provider-sqlite-scratch"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn kiro_terminal_revalidation_resource_exhaustion_stays_systemic() {
    let error =
        KiroSourceBackedErrorV0::SqliteSource(SqliteSourceAccessError::ResourceUnavailable {
            operation: "revalidating Kiro SQLite authority",
            path: Path::new("provider.sqlite").to_path_buf(),
            source: std::io::Error::from(std::io::ErrorKind::OutOfMemory),
        });

    assert_eq!(
        super::super::registration::kiro_scan_error(error).kind,
        crate::provider::source_backed::SourceBackedRouteErrorKind::ResourceUnavailable
    );
}

#[test]
fn production_kiro_route_taxonomy_preserves_change_and_corruption_provenance() {
    assert_eq!(
        super::super::registration::route_kiro_sqlite_call::<()>(Err(
            KiroSourceBackedErrorV0::Capture(crate::CaptureError::SourceChangedDuringCapture),
        ))
        .unwrap_err()
        .kind,
        SourceBackedRouteErrorKind::SourceChanged
    );

    let copied_corruption = SqliteSourceAccessError::SqliteControl {
        operation: "querying an exact Kiro provider copy",
        code: rusqlite::ffi::SQLITE_CORRUPT,
    }
    .with_diagnostic(
        crate::provider_sources::SqliteFailurePhase::Projection,
        crate::provider_sources::SqliteArtifactKind::PrivateSourceCopy,
        3,
        12_288,
        crate::provider_sources::SqliteCleanupStatus::NotRequired,
    );
    assert_eq!(
        super::super::registration::route_kiro_sqlite_source_call::<()>(Err(
            copied_corruption.with_exact_provider_content_provenance(),
        ))
        .unwrap_err()
        .kind,
        SourceBackedRouteErrorKind::InvalidSource
    );

    let private_corruption = SqliteSourceAccessError::SqliteControl {
        operation: "querying a damaged ctx-owned Kiro copy",
        code: rusqlite::ffi::SQLITE_CORRUPT,
    }
    .with_diagnostic(
        crate::provider_sources::SqliteFailurePhase::Projection,
        crate::provider_sources::SqliteArtifactKind::PrivateSourceCopy,
        3,
        12_288,
        crate::provider_sources::SqliteCleanupStatus::NotRequired,
    );
    assert_eq!(
        super::super::registration::route_kiro_sqlite_source_call::<()>(Err(private_corruption))
            .unwrap_err()
            .kind,
        SourceBackedRouteErrorKind::Internal
    );
}

#[test]
fn production_kiro_schema_failure_explicitly_reports_cleanup_without_leftovers() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let provider = temp.path().join("provider");
    let database = provider.join("kiro.sqlite");
    let data_root = temp.path().join("ctx-data");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&provider).unwrap();
    let writer = Connection::open(&database).unwrap();
    writer.pragma_update(None, "journal_mode", "WAL").unwrap();
    writer
        .execute_batch(
            "CREATE TABLE unsupported(value TEXT); INSERT INTO unsupported VALUES ('present')",
        )
        .unwrap();
    let source = provider_source_for_path(CaptureProvider::KiroCli, database);
    let mut registry = SourceBackedProviderRegistry::new();
    super::super::registration::register(
        &mut registry,
        source,
        SourceBackedRouteSelection::Automatic,
        &data_root,
    )
    .unwrap();
    fail_next_opened_snapshot_cleanup_for_test();

    let error = refresh_source_backed_generation(
        &index_root,
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap_err();
    let SourceBackedCoordinatorError::RouteScan { source, .. } = error else {
        panic!("unexpected Kiro refresh error: {error:?}");
    };
    assert_eq!(source.kind, SourceBackedRouteErrorKind::ResourceUnavailable);
    assert!(source.detail.contains("cleanup_status=failed"));
    let staging = data_root.join("tmp/provider-sqlite");
    assert!(staging.is_dir());
    assert_eq!(fs::read_dir(staging).unwrap().count(), 0);
}
