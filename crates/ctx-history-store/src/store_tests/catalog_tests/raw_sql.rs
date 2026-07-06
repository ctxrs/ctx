#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn raw_sql_query_reads_stable_views() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let schema = store.schema().unwrap();
    for view in [
        "CREATE VIEW ctx_sessions",
        "CREATE VIEW ctx_events",
        "CREATE VIEW ctx_files_touched",
        "CREATE VIEW ctx_sources",
    ] {
        assert!(schema.contains(view), "schema missing {view}");
    }

    let result = store
        .raw_sql_query(
            "SELECT COUNT(*) AS session_count FROM ctx_sessions",
            RawSqlOptions::default(),
        )
        .unwrap();
    assert_eq!(result.columns[0].name, "session_count");
    assert_eq!(result.returned_rows, 1);
    assert_eq!(result.rows[0][0], RawSqlValue::Integer(0));
}

#[test]
pub(crate) fn raw_sql_query_rejects_writes_parameters_and_multiple_statements() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();

    assert!(matches!(
        store
            .raw_sql_query("", RawSqlOptions::default())
            .unwrap_err(),
        StoreError::RawSqlEmpty
    ));
    assert!(matches!(
        store
            .raw_sql_query("SELECT ?1", RawSqlOptions::default())
            .unwrap_err(),
        StoreError::RawSqlHasParameters
    ));
    assert!(matches!(
        store
            .raw_sql_query("CREATE TABLE nope(x INTEGER)", RawSqlOptions::default())
            .unwrap_err(),
        StoreError::RawSqlNotReadOnly
    ));
    assert!(matches!(
        store
            .raw_sql_query("SELECT 1; SELECT 2", RawSqlOptions::default())
            .unwrap_err(),
        StoreError::Sql(rusqlite::Error::MultipleStatement)
    ));
}

#[test]
pub(crate) fn raw_sql_query_caps_rows_and_values() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let result = store
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
pub(crate) fn raw_sql_query_rejects_excessive_result_preview_budget() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let many_columns = (0..RAW_SQL_MAX_COLUMNS_CAP)
        .map(|index| format!("1 AS c{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let err = store
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
        err,
        StoreError::RawSqlResultBudgetTooLarge {
            max_result_bytes: RAW_SQL_MAX_RESULT_PREVIEW_BYTES,
            ..
        }
    ));
}

#[test]
pub(crate) fn raw_sql_query_budgets_against_actual_column_count() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let result = store
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
pub(crate) fn raw_sql_query_times_out_long_running_queries() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let err = store
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
    assert!(matches!(err, StoreError::RawSqlTimedOut { .. }));
}

#[test]
pub(crate) fn raw_sql_query_enforces_sqlite_value_length_limit() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let err = store
        .raw_sql_query(
            "SELECT length(randomblob(200000))",
            RawSqlOptions::default(),
        )
        .unwrap_err();
    assert!(matches!(
        err,
        StoreError::Sql(rusqlite::Error::SqliteFailure(error, _))
            if error.code == ErrorCode::TooBig
    ));
}
