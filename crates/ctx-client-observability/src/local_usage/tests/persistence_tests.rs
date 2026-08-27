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
