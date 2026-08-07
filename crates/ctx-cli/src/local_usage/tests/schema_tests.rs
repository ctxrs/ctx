use std::time::Duration;

use rusqlite::Connection;
use serde_json::json;

use super::{operation, private_tempdir};
use crate::local_usage::{store, CompletedOperation, McpInvocation, UsageStoreError};

#[test]
fn schema_has_only_spec_26_aggregate_fields_and_content_free_maintenance() {
    let root = private_tempdir();
    store::record(root.path(), operation("doctor")).unwrap();
    let mut store = store::open_read_only(&store::usage_path(root.path())).unwrap();
    let conn = store.connection_mut();
    let columns = conn
        .prepare("PRAGMA table_info(daily_usage)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        columns,
        [
            "day_utc",
            "definition_version",
            "ctx_version",
            "surface",
            "operation",
            "outcome",
            "value_class",
            "duration_bucket",
            "target_type",
            "pro_outcome",
            "context_coverage",
            "calls",
            "result_count",
            "citation_count",
            "delivered_output_bytes",
            "delivered_context_bytes",
            "matched_normalized_session_bytes",
        ]
    );
    let maintenance = conn
        .prepare("PRAGMA table_info(maintenance)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(maintenance, ["singleton", "last_retention_day"]);
}

#[test]
fn schema_rejects_context_baseline_and_citation_misclassification() {
    let root = private_tempdir();
    store::record(root.path(), operation("doctor")).unwrap();
    let path = store::usage_path(root.path());
    let conn = Connection::open(path).unwrap();
    assert!(conn
        .execute(
            "UPDATE daily_usage SET context_coverage = 'complete', \
             delivered_context_bytes = 10, matched_normalized_session_bytes = 9",
            [],
        )
        .is_err());
    assert!(conn
        .execute(
            "UPDATE daily_usage SET operation = 'search', \
             value_class = 'result_bearing', result_count = 1, \
             context_coverage = 'complete'",
            [],
        )
        .is_err());
    assert!(conn
        .execute("UPDATE daily_usage SET citation_count = 1", [])
        .is_err());
}

#[test]
fn schema_requires_positive_definition_two_success_and_mcp_output() {
    let root = private_tempdir();
    store::record(root.path(), operation("doctor")).unwrap();
    store::record(
        root.path(),
        CompletedOperation::cli("doctor", false, Duration::ZERO),
    )
    .unwrap();
    let mcp_failure = McpInvocation::recognized("status").unwrap().completed(
        &json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32602}}),
        Duration::ZERO,
        17,
    );
    store::record(root.path(), mcp_failure).unwrap();

    let conn = Connection::open(store::usage_path(root.path())).unwrap();
    assert!(conn
        .execute(
            "UPDATE daily_usage SET delivered_output_bytes = 0 \
             WHERE definition_version = 2 AND surface = 'cli' AND outcome = 'success'",
            [],
        )
        .is_err());
    assert!(conn
        .execute(
            "UPDATE daily_usage SET delivered_output_bytes = 0 \
             WHERE definition_version = 2 AND surface = 'mcp'",
            [],
        )
        .is_err());
    assert_eq!(
        conn.query_row(
            "SELECT delivered_output_bytes FROM daily_usage \
             WHERE definition_version = 2 AND surface = 'cli' AND outcome = 'failure'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        0,
        "a CLI failure that delivered no final output remains legitimate"
    );
}

#[test]
fn report_validation_rejects_constraint_bypassed_zero_output_rows() {
    for (surface, outcome) in [("cli", "success"), ("mcp", "failure")] {
        let root = private_tempdir();
        if surface == "cli" {
            store::record(root.path(), operation("doctor")).unwrap();
        } else {
            let failed = McpInvocation::recognized("status").unwrap().completed(
                &json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32602}}),
                Duration::ZERO,
                19,
            );
            store::record(root.path(), failed).unwrap();
        }
        let conn = Connection::open(store::usage_path(root.path())).unwrap();
        conn.pragma_update(None, "ignore_check_constraints", true)
            .unwrap();
        assert_eq!(
            conn.execute(
                "UPDATE daily_usage SET delivered_output_bytes = 0 \
                 WHERE definition_version = 2 AND surface = ?1 AND outcome = ?2",
                [surface, outcome],
            )
            .unwrap(),
            1
        );
        assert!(matches!(
            crate::local_usage::report::validate_rows(&conn),
            Err(UsageStoreError::Integrity)
        ));
    }
}

#[test]
fn sqlite_schema_and_report_contain_no_content_or_identity_dimensions() {
    let root = private_tempdir();
    store::record(root.path(), operation("doctor")).unwrap();
    let report =
        serde_json::to_string(&crate::local_usage::read_report(root.path(), true, true)).unwrap();
    let schema = {
        let mut store = store::open_read_only(&store::usage_path(root.path())).unwrap();
        store
            .connection_mut()
            .query_row(
                "SELECT group_concat(sql, ' ') FROM sqlite_schema WHERE sql IS NOT NULL",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap()
    };
    for forbidden in [
        "query_text",
        "path_value",
        "session_id",
        "event_id",
        "citation_id",
        "user_id",
        "machine_id",
        "telemetry_id",
    ] {
        assert!(!schema.contains(forbidden), "{forbidden}");
        assert!(!report.contains(forbidden), "{forbidden}");
    }
}
