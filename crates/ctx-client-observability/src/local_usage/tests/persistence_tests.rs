use std::{fs, time::Duration};

use super::{operation, private_tempdir};
use crate::local_usage::{
    read_report, reset, store, CompletedOperation, ContextCoverage, Surface, DEFINITION_VERSION,
};

#[test]
fn repeated_completions_upsert_aggregate_without_per_call_rows() {
    let root = private_tempdir();
    store::record(root.path(), operation("doctor")).unwrap();
    store::record(root.path(), operation("doctor")).unwrap();
    let report = read_report(root.path(), true, true);
    let definition = &report.definitions.unwrap()[0];
    assert_eq!(definition.summary.calls, 2);
    assert_eq!(definition.by_operation.len(), 1);
    assert_eq!(definition.by_operation[0].calls, 2);
}

#[test]
fn core_and_blame_completions_share_definition_three_without_extra_schema() {
    let root = private_tempdir();
    store::record(root.path(), operation("doctor")).unwrap();
    store::record(
        root.path(),
        CompletedOperation::blame(Surface::Cli, true, 0, Duration::from_millis(60)),
    )
    .unwrap();
    store::record(
        root.path(),
        CompletedOperation::blame(Surface::Mcp, true, 101, Duration::from_millis(4)),
    )
    .unwrap();
    store::record(
        root.path(),
        CompletedOperation::blame(Surface::Mcp, false, 79, Duration::from_secs(1)),
    )
    .unwrap();

    let report = read_report(root.path(), true, true);
    let definitions = report.definitions.unwrap();
    assert_eq!(definitions.len(), 1);
    let definition = &definitions[0];
    assert_eq!(definition.definition_version, DEFINITION_VERSION);
    assert_eq!(definition.summary.calls, 4);
    assert_eq!(definition.summary.result_bearing_calls, 0);
    assert_eq!(definition.summary.empty_calls, 0);
    assert_eq!(definition.summary.not_applicable_calls, 4);
    assert_eq!(definition.summary.result_count, 0);
    assert_eq!(definition.summary.delivered_output_bytes, 181);
    assert_eq!(definition.by_operation.len(), 3);
    assert!(definition
        .by_operation
        .iter()
        .any(|row| row.surface == "cli" && row.operation == "blame" && row.result_count == 0));
    assert!(definition
        .by_operation
        .iter()
        .any(|row| row.surface == "mcp" && row.operation == "blame" && row.calls == 2));
}

#[test]
fn stats_read_is_byte_for_byte_self_excluding() {
    let root = private_tempdir();
    store::record(root.path(), operation("doctor")).unwrap();
    let path = store::usage_path(root.path());
    let before = fs::read(&path).unwrap();
    let first = serde_json::to_value(read_report(root.path(), true, true)).unwrap();
    let second = serde_json::to_value(read_report(root.path(), true, true)).unwrap();
    assert_eq!(first, second);
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn compact_report_omits_operation_and_duration_details_without_changing_totals() {
    let root = private_tempdir();
    store::record(root.path(), operation("doctor")).unwrap();

    let detailed = read_report(root.path(), true, true);
    let compact = read_report(root.path(), true, false);
    let detailed_definition = &detailed.definitions.as_ref().unwrap()[0];
    let compact_definition = &compact.definitions.as_ref().unwrap()[0];

    assert_eq!(
        compact_definition.summary.calls,
        detailed_definition.summary.calls
    );
    assert!(!detailed_definition.by_operation.is_empty());
    assert!(!detailed_definition.duration_buckets.is_empty());
    assert!(compact_definition.by_operation.is_empty());
    assert!(compact_definition.duration_buckets.is_empty());

    let encoded = serde_json::to_value(compact).unwrap();
    let definition = &encoded["definitions"][0];
    assert!(definition.get("by_operation").is_none());
    assert!(definition.get("duration_buckets").is_none());
}

#[test]
fn reset_removes_aggregates_without_recreating_usage() {
    let root = private_tempdir();
    store::record(root.path(), operation("doctor")).unwrap();
    let path = store::usage_path(root.path());
    assert!(reset(root.path()).unwrap());
    let report = read_report(root.path(), true, true);
    assert_eq!(report.state, "empty");
    assert!(report.definitions.unwrap().is_empty());
    let connection = rusqlite::Connection::open(path).unwrap();
    let maintenance_rows: i64 = connection
        .query_row("SELECT COUNT(*) FROM maintenance", [], |row| row.get(0))
        .unwrap();
    assert_eq!(maintenance_rows, 0);
}

#[test]
fn unavailable_search_coverage_never_extrapolates_an_estimate() {
    let root = private_tempdir();
    let mut search = operation("search").with_value(crate::local_usage::ValueClass::ResultBearing);
    search.result_count = 1;
    search.context_coverage = ContextCoverage::Unavailable;
    store::record(root.path(), search).unwrap();
    let report = read_report(root.path(), true, true);
    assert!(report.estimates.is_none());
    let summary = &report.definitions.unwrap()[0].summary;
    assert_eq!(summary.unavailable_context_eligible_calls, 1);
}

#[test]
fn held_writer_is_best_effort_and_does_not_block_primary_path_long() {
    let root = private_tempdir();
    store::record(root.path(), operation("doctor")).unwrap();
    let path = store::usage_path(root.path());
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    let started = std::time::Instant::now();
    let result = store::record_at_for_test(
        root.path(),
        operation("doctor"),
        std::time::SystemTime::now(),
        Duration::from_millis(25),
    );
    assert!(result.is_err());
    assert!(started.elapsed() < Duration::from_millis(100));
}

fn write_private_file(path: &std::path::Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn capture_uncheckpointed_wal(
    db_path: &std::path::Path,
    mutate: impl FnOnce(&rusqlite::Connection),
) -> (Vec<u8>, Vec<u8>) {
    let original_main = fs::read(db_path).unwrap();
    let scratch = private_tempdir();
    let scratch_db = scratch.path().join("usage.sqlite");
    write_private_file(&scratch_db, &original_main);
    let conn = rusqlite::Connection::open(&scratch_db).unwrap();
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    mutate(&conn);
    let wal_bytes = fs::read(scratch.path().join("usage.sqlite-wal")).unwrap();
    assert!(!wal_bytes.is_empty());
    drop(conn);
    (original_main, wal_bytes)
}

#[test]
fn stale_shm_with_empty_wal_keeps_reads_strict_and_recovers_on_write() {
    let root = private_tempdir();
    store::record(root.path(), operation("doctor")).unwrap();
    let wal_path = root.path().join("usage.sqlite-wal");
    let shm_path = root.path().join("usage.sqlite-shm");
    write_private_file(&wal_path, &[]);
    write_private_file(&shm_path, &[0, 0, 0]);

    let blocked = read_report(root.path(), true, true);
    assert_eq!(blocked.state, "error");
    assert_eq!(blocked.error.unwrap().code, "usage_store_unavailable");
    assert_eq!(fs::read(&shm_path).unwrap(), vec![0, 0, 0]);
    assert_eq!(fs::read(&wal_path).unwrap(), Vec::<u8>::new());

    store::record(root.path(), operation("doctor")).unwrap();
    assert!(!wal_path.exists());
    assert!(!shm_path.exists());

    let recovered = read_report(root.path(), true, true);
    assert_eq!(recovered.state, "ready");
    assert_eq!(recovered.definitions.unwrap()[0].summary.calls, 2);
}

#[test]
fn stale_shm_recovers_on_reset() {
    let root = private_tempdir();
    store::record(root.path(), operation("doctor")).unwrap();
    let wal_path = root.path().join("usage.sqlite-wal");
    let shm_path = root.path().join("usage.sqlite-shm");
    write_private_file(&wal_path, &[]);
    write_private_file(&shm_path, &[0, 0, 0]);

    assert!(reset(root.path()).unwrap());
    assert!(!wal_path.exists());
    assert!(!shm_path.exists());

    let report = read_report(root.path(), true, true);
    assert_eq!(report.state, "empty");
}

#[test]
fn replayable_nonempty_wal_and_stale_shm_recover_without_data_loss() {
    let root = private_tempdir();
    store::record(root.path(), operation("doctor")).unwrap();
    let db_path = store::usage_path(root.path());
    let (original_main, wal_bytes) = capture_uncheckpointed_wal(&db_path, |conn| {
        conn.execute("UPDATE daily_usage SET calls = calls + 1", [])
            .unwrap();
    });
    let wal_path = root.path().join("usage.sqlite-wal");
    let shm_path = root.path().join("usage.sqlite-shm");
    write_private_file(&db_path, &original_main);
    write_private_file(&wal_path, &wal_bytes);
    write_private_file(&shm_path, &[0, 0, 0]);

    let blocked = read_report(root.path(), true, true);
    assert_eq!(blocked.state, "error");
    assert_eq!(fs::read(&wal_path).unwrap(), wal_bytes);

    store::record(root.path(), operation("doctor")).unwrap();
    assert!(!wal_path.exists());
    assert!(!shm_path.exists());

    let recovered = read_report(root.path(), true, true);
    assert_eq!(recovered.state, "ready");
    assert_eq!(recovered.definitions.unwrap()[0].summary.calls, 3);
}

#[test]
fn corrupt_nonempty_wal_is_rejected_without_modifying_live_store() {
    let root = private_tempdir();
    store::record(root.path(), operation("doctor")).unwrap();
    let db_path = store::usage_path(root.path());
    let before_main = fs::read(&db_path).unwrap();
    let wal_path = root.path().join("usage.sqlite-wal");
    let shm_path = root.path().join("usage.sqlite-shm");
    let junk = b"not a valid sqlite wal payload for usage store recovery";
    write_private_file(&wal_path, junk);
    write_private_file(&shm_path, &[0, 0, 0]);

    let err = store::record(root.path(), operation("doctor")).unwrap_err();
    assert!(matches!(
        err,
        crate::local_usage::UsageStoreError::UnsafeReadState
    ));
    assert_eq!(fs::read(&db_path).unwrap(), before_main);
    assert_eq!(fs::read(&wal_path).unwrap(), junk);
}

#[test]
fn tampered_wal_violating_row_invariants_is_rejected_before_live_open() {
    let root = private_tempdir();
    store::record(root.path(), operation("doctor")).unwrap();
    let db_path = store::usage_path(root.path());
    let (original_main, tampered_wal) = capture_uncheckpointed_wal(&db_path, |conn| {
        conn.pragma_update(None, "ignore_check_constraints", true)
            .unwrap();
        conn.execute("UPDATE daily_usage SET delivered_output_bytes = 0", [])
            .unwrap();
    });
    let wal_path = root.path().join("usage.sqlite-wal");
    write_private_file(&db_path, &original_main);
    write_private_file(&wal_path, &tampered_wal);

    let err = store::record(root.path(), operation("doctor")).unwrap_err();
    assert!(matches!(
        err,
        crate::local_usage::UsageStoreError::Integrity
    ));
    assert_eq!(fs::read(&db_path).unwrap(), original_main);
    assert_eq!(fs::read(&wal_path).unwrap(), tampered_wal);
}
