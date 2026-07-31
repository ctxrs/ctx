use std::time::Duration;

use rusqlite::ffi::ErrorCode;

use super::{
    projection, RawSqlOptions, RawSqlValue, RelationalProjectionError, RAW_SQL_MAX_COLUMNS_CAP,
    RAW_SQL_MAX_RESULT_PREVIEW_BYTES, RAW_SQL_MAX_ROWS_CAP,
};

#[test]
fn guarded_raw_sql_rejects_empty_parameters_writes_and_multiple_statements() {
    let (_temp, projection) = projection();

    assert!(matches!(
        projection
            .raw_sql_query("", RawSqlOptions::default())
            .unwrap_err(),
        RelationalProjectionError::RawSqlEmpty
    ));
    assert!(matches!(
        projection
            .raw_sql_query("SELECT ?1", RawSqlOptions::default())
            .unwrap_err(),
        RelationalProjectionError::RawSqlHasParameters
    ));
    assert!(matches!(
        projection
            .raw_sql_query("CREATE TABLE nope(x INTEGER)", RawSqlOptions::default())
            .unwrap_err(),
        RelationalProjectionError::RawSqlNotReadOnly
    ));
    assert!(matches!(
        projection
            .raw_sql_query("SELECT 1; SELECT 2", RawSqlOptions::default())
            .unwrap_err(),
        RelationalProjectionError::Sql(rusqlite::Error::MultipleStatement)
    ));
}

#[test]
fn guarded_raw_sql_caps_rows_and_values() {
    let (_temp, projection) = projection();
    let result = projection
        .raw_sql_query(
            "SELECT 'abcdef' AS text_value, X'01020304' AS blob_value UNION ALL SELECT 'ghijkl', X'05060708'",
            RawSqlOptions {
                max_rows: 1,
                max_value_bytes: 3,
                ..RawSqlOptions::default()
            },
        )
        .unwrap();

    assert_eq!(result.returned_rows, 1);
    assert_eq!(result.columns[0].name, "text_value");
    assert_eq!(result.columns[1].name, "blob_value");
    assert_eq!(
        result.rows[0][0],
        RawSqlValue::Text {
            value: "abc".to_owned(),
            bytes: 6,
            truncated: true,
        }
    );
    assert_eq!(
        result.rows[0][1],
        RawSqlValue::Blob {
            bytes: 4,
            preview_hex: "010203".to_owned(),
            truncated: true,
        }
    );
    assert!(result.truncated.rows);
    assert!(result.truncated.values);
}

#[test]
fn guarded_raw_sql_rejects_excessive_result_preview_budget() {
    let (_temp, projection) = projection();
    let many_columns = (0..RAW_SQL_MAX_COLUMNS_CAP)
        .map(|index| format!("1 AS c{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let error = projection
        .raw_sql_query(
            &format!("SELECT {many_columns}"),
            RawSqlOptions {
                max_rows: RAW_SQL_MAX_ROWS_CAP,
                max_columns: RAW_SQL_MAX_COLUMNS_CAP,
                max_value_bytes: 32,
                ..RawSqlOptions::default()
            },
        )
        .unwrap_err();

    assert!(matches!(
        error,
        RelationalProjectionError::RawSqlResultBudgetTooLarge {
            max_result_bytes: RAW_SQL_MAX_RESULT_PREVIEW_BYTES,
            ..
        }
    ));
}

#[test]
fn guarded_raw_sql_budgets_against_actual_column_count() {
    let (_temp, projection) = projection();
    let result = projection
        .raw_sql_query(
            "SELECT 1",
            RawSqlOptions {
                max_rows: RAW_SQL_MAX_ROWS_CAP,
                max_columns: RAW_SQL_MAX_COLUMNS_CAP,
                max_value_bytes: 32,
                ..RawSqlOptions::default()
            },
        )
        .unwrap();

    assert_eq!(result.returned_rows, 1);
    assert_eq!(result.rows[0][0], RawSqlValue::Integer(1));
}

#[test]
fn guarded_raw_sql_times_out_long_running_queries() {
    let (_temp, projection) = projection();
    let error = projection
        .raw_sql_query(
            r#"
            WITH RECURSIVE numbers(x) AS (
                SELECT 1
                UNION ALL
                SELECT x + 1 FROM numbers WHERE x < 100000000
            )
            SELECT sum(x) FROM numbers
            "#,
            RawSqlOptions {
                timeout: Duration::from_millis(1),
                ..RawSqlOptions::default()
            },
        )
        .unwrap_err();

    assert!(matches!(
        error,
        RelationalProjectionError::RawSqlTimedOut { .. }
    ));
}

#[test]
fn guarded_raw_sql_enforces_sqlite_value_length_limit() {
    let (_temp, projection) = projection();
    let error = projection
        .raw_sql_query(
            "SELECT length(randomblob(200000))",
            RawSqlOptions::default(),
        )
        .unwrap_err();

    assert!(matches!(
        error,
        RelationalProjectionError::Sql(rusqlite::Error::SqliteFailure(error, _))
            if error.code == ErrorCode::TooBig
    ));
}
