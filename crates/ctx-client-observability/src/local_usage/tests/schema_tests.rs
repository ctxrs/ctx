use std::time::Duration;

use super::{operation, private_tempdir};
use crate::{
    local_usage::{store, CompletedOperation, McpCompletionFacts, McpInvocation, UsageStoreError},
    operation_descriptor::ObservedMcpProductOperation,
};
use rusqlite::Connection;

fn assert_schema_identity_rejected(schema_version: i64, mutation: &str, schema: &str) {
    let result = store::verify_released_schema_text_for_test(schema_version, schema);
    assert!(
        matches!(result, Err(UsageStoreError::SchemaIdentity)),
        "schema v{schema_version} did not reject {mutation} as an identity error: {result:?}"
    );
}

#[test]
fn released_predecessor_ddl_identities_are_exact_for_every_version() {
    for schema_version in 1..=4 {
        let schema = store::released_schema_for_test(schema_version).unwrap();
        assert_eq!(
            store::verify_released_schema_text_for_test(schema_version, &schema).unwrap(),
            schema_version
        );

        let extra_column = schema.replacen(
            "    calls INTEGER NOT NULL CHECK (calls > 0),",
            "    unexpected INTEGER NOT NULL DEFAULT 0,\n    calls INTEGER NOT NULL CHECK (calls > 0),",
            1,
        );
        assert_schema_identity_rejected(schema_version, "an extra column", &extra_column);

        let altered_constraint = schema.replacen(
            "calls INTEGER NOT NULL CHECK (calls > 0)",
            "calls INTEGER NOT NULL CHECK (calls >= 0)",
            1,
        );
        assert_schema_identity_rejected(
            schema_version,
            "an altered constraint",
            &altered_constraint,
        );

        let without_strict = schema.replacen(") WITHOUT ROWID, STRICT;", ") WITHOUT ROWID;", 1);
        assert_schema_identity_rejected(schema_version, "removal of STRICT", &without_strict);

        let with_rowid = schema.replacen(") WITHOUT ROWID, STRICT;", ") STRICT;", 1);
        assert_schema_identity_rejected(schema_version, "removal of WITHOUT ROWID", &with_rowid);
    }

    let initial_v1 = store::released_initial_v1_schema_for_test();
    assert_eq!(
        store::verify_released_schema_text_for_test(1, &initial_v1).unwrap(),
        1
    );
}

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
            "context_coverage",
            "calls",
            "result_count",
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
fn active_v5_admits_current_core_and_blame_but_rejects_retired_sql() {
    let root = private_tempdir();
    store::record(root.path(), operation("doctor")).unwrap();
    let mcp_status = McpInvocation::from_operation(ObservedMcpProductOperation::Status).completed(
        &McpCompletionFacts {
            delivered_output_bytes: 17,
            ..McpCompletionFacts::default()
        },
        Duration::ZERO,
    );
    store::record(root.path(), mcp_status).unwrap();

    let conn = Connection::open(store::usage_path(root.path())).unwrap();
    let schema = conn
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'daily_usage'",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap();
    assert!(schema.contains("'locate'"));
    assert!(!schema.contains("'sql'"));

    assert_eq!(
        conn.execute(
            "UPDATE daily_usage SET operation = 'locate' WHERE surface = 'cli'",
            [],
        )
        .unwrap(),
        1
    );
    crate::local_usage::report::validate_rows(&conn).unwrap();
    assert!(conn
        .execute(
            "UPDATE daily_usage SET operation = 'sql' WHERE surface = 'cli'",
            [],
        )
        .is_err());
    assert!(conn
        .execute(
            "UPDATE daily_usage SET operation = 'sql', value_class = 'result_bearing', \
             result_count = calls WHERE surface = 'mcp'",
            [],
        )
        .is_err());

    conn.pragma_update(None, "ignore_check_constraints", true)
        .unwrap();
    assert_eq!(
        conn.execute(
            "UPDATE daily_usage SET operation = 'sql' WHERE surface = 'cli'",
            [],
        )
        .unwrap(),
        1
    );
    assert!(matches!(
        crate::local_usage::report::validate_rows(&conn),
        Err(UsageStoreError::Integrity)
    ));
}

#[test]
fn schema_rejects_invalid_context_classification() {
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
}

#[test]
fn schema_requires_positive_definition_three_core_success_and_mcp_output() {
    let root = private_tempdir();
    store::record(root.path(), operation("doctor")).unwrap();
    store::record(
        root.path(),
        CompletedOperation::cli(
            crate::operation_descriptor::LocalUsageOperation::Doctor,
            false,
            Duration::ZERO,
        ),
    )
    .unwrap();
    let mcp_failure = McpInvocation::from_operation(ObservedMcpProductOperation::Status).completed(
        &McpCompletionFacts {
            failed: true,
            delivered_output_bytes: 17,
            ..McpCompletionFacts::default()
        },
        Duration::ZERO,
    );
    store::record(root.path(), mcp_failure).unwrap();

    let conn = Connection::open(store::usage_path(root.path())).unwrap();
    assert!(conn
        .execute(
            "UPDATE daily_usage SET delivered_output_bytes = 0 \
             WHERE definition_version = 3 AND surface = 'cli' AND outcome = 'success'",
            [],
        )
        .is_err());
    assert!(conn
        .execute(
            "UPDATE daily_usage SET delivered_output_bytes = 0 \
             WHERE definition_version = 3 AND surface = 'mcp'",
            [],
        )
        .is_err());
    assert_eq!(
        conn.query_row(
            "SELECT delivered_output_bytes FROM daily_usage \
             WHERE definition_version = 3 AND surface = 'cli' AND outcome = 'failure'",
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
            let failed = McpInvocation::from_operation(ObservedMcpProductOperation::Status)
                .completed(
                    &McpCompletionFacts {
                        failed: true,
                        delivered_output_bytes: 19,
                        ..McpCompletionFacts::default()
                    },
                    Duration::ZERO,
                );
            store::record(root.path(), failed).unwrap();
        }
        let conn = Connection::open(store::usage_path(root.path())).unwrap();
        conn.pragma_update(None, "ignore_check_constraints", true)
            .unwrap();
        assert_eq!(
            conn.execute(
                "UPDATE daily_usage SET delivered_output_bytes = 0 \
                 WHERE definition_version = 3 AND surface = ?1 AND outcome = ?2",
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
        "user_id",
        "machine_id",
        "telemetry_id",
    ] {
        assert!(!schema.contains(forbidden), "{forbidden}");
        assert!(!report.contains(forbidden), "{forbidden}");
    }
}
