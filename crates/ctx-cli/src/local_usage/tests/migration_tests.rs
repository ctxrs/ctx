use std::fs;

use super::{operation, private_tempdir};
use crate::local_usage::{read_report, store};

#[test]
fn v1_migration_preserves_definition_one_and_new_writes_use_definition_two() {
    let root = private_tempdir();
    store::create_mixed_v1_fixture_for_test(root.path()).unwrap();
    store::record(root.path(), operation("doctor")).unwrap();

    let report = read_report(root.path(), true, true);
    let definitions = report.definitions.unwrap();
    assert_eq!(
        definitions
            .iter()
            .map(|definition| definition.definition_version)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    let legacy = &definitions[0];
    assert_eq!(legacy.summary.calls, 9);
    assert_eq!(legacy.summary.delivered_output_bytes, 1_500);
    assert_eq!(legacy.summary.delivered_context_bytes, 0);
    assert_eq!(legacy.summary.complete_context_eligible_calls, 0);
    assert_eq!(legacy.summary.unavailable_context_eligible_calls, 0);
    assert_eq!(legacy.by_operation[0].ctx_version, "0.25.0-legacy");

    let current = &definitions[1];
    assert_eq!(current.summary.calls, 1);
    assert_eq!(current.by_operation[0].operation, "doctor");
}

#[test]
fn read_only_report_migrates_detached_image_without_touching_v1_family() {
    let root = private_tempdir();
    store::create_mixed_v1_fixture_for_test(root.path()).unwrap();
    let path = store::usage_path(root.path());
    let before = fs::read(&path).unwrap();
    let report = read_report(root.path(), true, true);
    assert_eq!(report.definitions.unwrap()[0].definition_version, 1);
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn failed_v1_migration_is_transactional() {
    let root = private_tempdir();
    store::create_mixed_v1_fixture_for_test(root.path()).unwrap();
    assert!(store::fail_v1_migration_before_commit_for_test(root.path()).is_err());
    let report = read_report(root.path(), true, true);
    assert_eq!(report.definitions.unwrap()[0].definition_version, 1);
    store::record(root.path(), operation("doctor")).unwrap();
}

#[test]
fn legacy_impossible_blame_row_remains_definition_one_without_relabeling() {
    let root = private_tempdir();
    store::create_legacy_impossible_blame_v1_fixture_for_test(root.path()).unwrap();
    store::record(root.path(), operation("doctor")).unwrap();
    let definitions = read_report(root.path(), true, true).definitions.unwrap();
    let legacy = definitions
        .iter()
        .find(|definition| definition.definition_version == 1)
        .unwrap();
    assert_eq!(legacy.summary.pro_blame.possible_only_requests, 1);
    assert_eq!(legacy.summary.pro_blame.none_requests, 0);
}

#[test]
fn valid_schema_two_rows_migrate_without_changing_definition_semantics() {
    let root = private_tempdir();
    store::create_v2_fixture_for_test(root.path(), "cli", "success", 23).unwrap();
    let path = store::usage_path(root.path());
    let before = fs::read(&path).unwrap();

    let detached_report = read_report(root.path(), true, true);
    let detached_definition = &detached_report.definitions.unwrap()[0];
    assert_eq!(detached_definition.definition_version, 2);
    assert_eq!(detached_definition.summary.calls, 1);
    assert_eq!(detached_definition.summary.delivered_output_bytes, 23);
    assert_eq!(fs::read(&path).unwrap(), before);

    store::record(root.path(), operation("doctor")).unwrap();
    let connection = rusqlite::Connection::open(path).unwrap();
    let user_version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(user_version, 3);
    drop(connection);
    let report = read_report(root.path(), true, true);
    let definition = &report.definitions.unwrap()[0];
    assert_eq!(definition.definition_version, 2);
    assert_eq!(definition.summary.calls, 2);
    assert_eq!(definition.summary.delivered_output_bytes, 24);
}

#[test]
fn schema_two_cli_failure_without_output_remains_migration_compatible() {
    let root = private_tempdir();
    store::create_v2_fixture_for_test(root.path(), "cli", "failure", 0).unwrap();
    store::record(root.path(), operation("doctor")).unwrap();

    let report = read_report(root.path(), true, true);
    let definition = &report.definitions.unwrap()[0];
    assert_eq!(definition.definition_version, 2);
    assert_eq!(definition.summary.calls, 2);
    assert_eq!(definition.summary.successful_calls, 1);
    assert_eq!(definition.summary.failed_calls, 1);
    assert_eq!(definition.summary.delivered_output_bytes, 1);
}

#[test]
fn impossible_schema_two_zero_output_rows_fail_closed_without_migration() {
    for (surface, outcome) in [("cli", "success"), ("mcp", "failure")] {
        let root = private_tempdir();
        store::create_v2_fixture_for_test(root.path(), surface, outcome, 0).unwrap();
        let path = store::usage_path(root.path());
        let before = fs::read(&path).unwrap();

        let report = read_report(root.path(), true, true);
        assert_eq!(report.state, "error", "{surface} {outcome}");
        assert!(report.definitions.is_none());
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(store::record(root.path(), operation("doctor")).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);

        let connection = rusqlite::Connection::open(path).unwrap();
        let user_version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(user_version, 2);
    }
}
