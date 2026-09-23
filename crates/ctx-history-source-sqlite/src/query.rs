use std::collections::BTreeSet;

use ctx_history_core::compute_payload_hash;
use rusqlite::{limits::Limit, Connection};
use serde_json::json;

use crate::{Result, SqliteIoError};

pub fn sqlite_table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let exists: i64 = conn.query_row(
        "select count(*) from sqlite_schema where type = 'table' and name = ?1",
        [table],
        |row| row.get(0),
    )?;
    Ok(exists > 0)
}

pub fn sqlite_table_columns(conn: &Connection, table: &str) -> Result<BTreeSet<String>> {
    let mut stmt = conn.prepare(&format!("pragma table_info({})", sqlite_ident(table)))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    rows.collect::<std::result::Result<BTreeSet<_>, _>>()
        .map_err(SqliteIoError::from)
}

pub fn ensure_sqlite_table_columns(
    columns: &BTreeSet<String>,
    label: &str,
    required: &[&str],
) -> Result<()> {
    let missing = required
        .iter()
        .copied()
        .filter(|column| !columns.contains(*column))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(SqliteIoError::InvalidPayload(format!(
            "{label} missing required column(s): {}",
            missing.join(", ")
        )))
    }
}

pub fn optional_column_expr<'a>(
    columns: &BTreeSet<String>,
    column: &'a str,
    fallback: &'a str,
) -> &'a str {
    if columns.contains(column) {
        column
    } else {
        fallback
    }
}

pub fn optional_text_column_expr(
    columns: &BTreeSet<String>,
    column: &str,
    fallback: &str,
) -> String {
    if columns.contains(column) {
        format!("CAST({column} AS TEXT)")
    } else {
        fallback.to_owned()
    }
}

pub fn optional_timestamp_millis_expr(
    columns: &BTreeSet<String>,
    column: &str,
    fallback: &str,
) -> String {
    if !columns.contains(column) {
        return fallback.to_owned();
    }
    let text = format!("trim(CAST({column} AS TEXT))");
    let numeric_body = format!(
        "CASE WHEN substr({text}, 1, 1) IN ('+', '-') THEN substr({text}, 2) ELSE {text} END"
    );
    let numeric_value = format!(
        "CASE WHEN abs(CAST({column} AS REAL)) < 100000000000 \
         THEN CAST(ROUND(CAST({column} AS REAL) * 1000) AS INTEGER) \
         ELSE CAST(ROUND(CAST({column} AS REAL)) AS INTEGER) END"
    );
    format!(
        "CASE WHEN {column} IS NULL THEN NULL \
         WHEN typeof({column}) IN ('integer', 'real') THEN {numeric_value} \
         WHEN {numeric_body} != '' \
              AND {numeric_body} != '.' \
              AND {numeric_body} NOT GLOB '*[^0-9.]*' \
              AND length({numeric_body}) - length(replace({numeric_body}, '.', '')) <= 1 \
         THEN {numeric_value} \
         ELSE CAST(ROUND(unixepoch({column}, 'subsec') * 1000) AS INTEGER) END"
    )
}

pub fn sqlite_ident(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

/// Provider schema preflight policy. Raw value hydration must happen after
/// this guard restores the configured provider value limit.
#[must_use = "the SQLite length limit is restored when this guard is dropped"]
pub struct SqliteLengthPreflightGuard<'connection> {
    conn: &'connection Connection,
    prior_limit: i32,
}

impl<'connection> SqliteLengthPreflightGuard<'connection> {
    pub fn new(conn: &'connection Connection) -> rusqlite::Result<Self> {
        Ok(Self {
            conn,
            prior_limit: conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, i32::MAX)?,
        })
    }
}

impl Drop for SqliteLengthPreflightGuard<'_> {
    fn drop(&mut self) {
        // The same supported limit and SQLite-returned nonnegative value
        // cannot fail the range checks in set_limit, including during unwind.
        let _ = self
            .conn
            .set_limit(Limit::SQLITE_LIMIT_LENGTH, self.prior_limit);
    }
}

/// How far a unique-index probe will walk a schema before refusing it.
#[derive(Clone, Copy, Debug)]
pub struct UniqueIndexProbe<'a> {
    /// Provider display name used in bound-rejection prose.
    pub provider: &'a str,
    /// Maximum indexes, and maximum columns per index, to inspect.
    pub max_rows: usize,
}

/// Finds a total, non-partial unique index whose key columns are exactly
/// `expected`, ascending, with binary collation.
///
/// A partial or descending index cannot back a keyset scan, and a non-binary
/// collation would order rows differently than the projection expects, so both
/// are skipped rather than accepted.
pub fn sqlite_unique_index_for_columns(
    conn: &Connection,
    table: &str,
    expected: &[&str],
    probe: &UniqueIndexProbe<'_>,
) -> Result<Option<String>> {
    let provider = probe.provider;
    let max_rows = probe.max_rows;
    let sql = format!("pragma index_list(\"{}\")", table.replace('"', "\"\""));
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)? != 0,
            row.get::<_, i64>(4)? != 0,
        ))
    })?;
    let mut indexes = Vec::new();
    for row in rows {
        if indexes.len() >= max_rows {
            return Err(SqliteIoError::InvalidPayload(format!(
                "{provider} {table} schema exceeds the {max_rows}-index inspection bound"
            )));
        }
        indexes.push(row?);
    }
    for (index, unique, partial) in indexes {
        if !unique || partial {
            continue;
        }
        let sql = format!("pragma index_xinfo(\"{}\")", index.replace('"', "\"\""));
        let mut statement = conn.prepare(&sql)?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)? != 0,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)? != 0,
            ))
        })?;
        let mut columns = Vec::new();
        for row in rows {
            if columns.len() >= max_rows {
                return Err(SqliteIoError::InvalidPayload(format!(
                    "{provider} {table} index exceeds the {max_rows}-column inspection bound"
                )));
            }
            columns.push(row?);
        }
        let key_columns = columns
            .iter()
            .filter(|(_, _, _, key)| *key)
            .collect::<Vec<_>>();
        if key_columns.len() == expected.len()
            && key_columns.iter().zip(expected).all(
                |((column, descending, collation, _), expected)| {
                    column.as_deref() == Some(*expected)
                        && !descending
                        && collation
                            .as_deref()
                            .is_some_and(|value| value.eq_ignore_ascii_case("binary"))
                },
            )
        {
            return Ok(Some(index));
        }
    }
    Ok(None)
}

pub fn sqlite_schema_fingerprint(conn: &Connection) -> Result<String> {
    let mut stmt = conn.prepare(
        "select name, sql from sqlite_schema where type in ('table','index') order by name",
    )?;
    let rows = stmt.query_map([], |row| {
        let name: String = row.get(0)?;
        let sql: Option<String> = row.get(1)?;
        Ok(format!("{name}:{}", sql.unwrap_or_default()))
    })?;
    let schema = rows.collect::<std::result::Result<Vec<_>, _>>()?.join("\n");
    Ok(compute_payload_hash(&json!({ "schema": schema }))?)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        panic::{catch_unwind, AssertUnwindSafe},
    };

    use rusqlite::{params, types::Value as SqlValue};

    use super::*;

    const TEST_LENGTH_LIMIT: i32 = 16 * 1024;

    fn connection_with_test_length_limit() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, TEST_LENGTH_LIMIT)
            .unwrap();
        assert_eq!(
            conn.limit(Limit::SQLITE_LIMIT_LENGTH).unwrap(),
            TEST_LENGTH_LIMIT
        );
        conn
    }

    #[test]
    fn length_preflight_guard_restores_prior_limit_on_success_and_unwind() {
        let conn = connection_with_test_length_limit();
        {
            let _guard = SqliteLengthPreflightGuard::new(&conn).unwrap();
            assert!(conn.limit(Limit::SQLITE_LIMIT_LENGTH).unwrap() > TEST_LENGTH_LIMIT);
        }
        assert_eq!(
            conn.limit(Limit::SQLITE_LIMIT_LENGTH).unwrap(),
            TEST_LENGTH_LIMIT
        );
        let unwind = catch_unwind(AssertUnwindSafe(|| {
            let _guard = SqliteLengthPreflightGuard::new(&conn).unwrap();
            panic!("exercise SQLite preflight guard cleanup");
        }));
        assert!(unwind.is_err());
        assert_eq!(
            conn.limit(Limit::SQLITE_LIMIT_LENGTH).unwrap(),
            TEST_LENGTH_LIMIT
        );
    }

    #[test]
    fn optional_sqlite_casts_normalize_provider_timestamp_shapes() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE samples (position INTEGER, value)", [])
            .unwrap();
        let samples = [
            (SqlValue::Integer(1_783_653_514), Some(1_783_653_514_000)),
            (SqlValue::Real(1_783_653_514.491), Some(1_783_653_514_491)),
            (
                SqlValue::Text("1783653514491".into()),
                Some(1_783_653_514_491),
            ),
            (
                SqlValue::Text("2026-07-10T03:18:34.491Z".into()),
                Some(1_783_653_514_491),
            ),
            (SqlValue::Text("not-a-timestamp".into()), None),
            (SqlValue::Null, None),
        ];
        for (position, (value, _)) in samples.iter().enumerate() {
            conn.execute(
                "INSERT INTO samples VALUES (?1, ?2)",
                params![position as i64, value],
            )
            .unwrap();
        }
        let columns = BTreeSet::from(["value".to_owned()]);
        let timestamp = optional_timestamp_millis_expr(&columns, "value", "NULL");
        let actual = conn
            .prepare(&format!(
                "SELECT {timestamp} FROM samples ORDER BY position"
            ))
            .unwrap()
            .query_map([], |row| row.get::<_, Option<i64>>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            actual,
            samples
                .iter()
                .map(|(_, expected)| *expected)
                .collect::<Vec<_>>()
        );
    }
}
