//! Test-only helpers for provider SQLite suites.
//!
//! Gated behind the `test-support` feature so provider crates can build their
//! own fixtures against the same temporary-directory and query-plan rules the
//! production snapshot code enforces, instead of each re-deriving them.

use rusqlite::Connection;

/// A temporary directory under the canonicalized system temp root.
///
/// Canonicalizing first matters on platforms where the temp root is itself a
/// symlink: the snapshot code refuses symlinked path components, so a fixture
/// built under an uncanonicalized root would fail admission for the wrong
/// reason.
pub fn tempdir() -> std::io::Result<tempfile::TempDir> {
    crate::test_support_paths::tempdir()
}

/// Asserts `sql` executes without SQLite sorting in temporary storage or
/// synthesizing its own index.
///
/// Providers that stream rows in a Core-defined order depend on a covering
/// index actually covering the scan. `AUTOMATIC` in a query plan means SQLite
/// is building that index itself at run time, which is the same latent failure
/// as `USE TEMP B-TREE`: unbounded temporary storage outside the scratch byte
/// authority. Both are rejected here so a schema change surfaces as a test
/// failure rather than as unaccounted temp usage in production.
///
/// Bound parameters are supplied as NULL, so a provider can pass its real
/// parameterized statement rather than a literal-substituted rewrite of it
/// that might not plan the same way.
///
/// # Panics
///
/// Panics if the statement cannot be prepared, or if its plan sorts or
/// auto-indexes.
pub fn assert_no_temp_btree(conn: &Connection, sql: &str) {
    let explain = format!("EXPLAIN QUERY PLAN {sql}");
    let mut statement = conn
        .prepare(&explain)
        .unwrap_or_else(|error| panic!("prepare {explain}: {error}"));
    let parameters = vec![rusqlite::types::Null; statement.parameter_count()];
    let mut rows = statement
        .query(rusqlite::params_from_iter(parameters))
        .unwrap_or_else(|error| panic!("explain {sql}: {error}"));
    while let Some(row) = rows.next().expect("read query plan row") {
        let detail = row.get::<_, String>(3).expect("query plan detail");
        assert!(
            !detail.contains("USE TEMP B-TREE") && !detail.contains("AUTOMATIC"),
            "query plan for {sql} uses ambient temporary storage: {detail}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory database");
        conn.execute_batch(
            "create table t (a integer primary key, b text not null, c text);
             create index t_b on t (b collate binary, a);",
        )
        .expect("fixture schema");
        conn
    }

    #[test]
    fn covered_scans_pass_and_ambient_temp_storage_is_rejected() {
        let conn = fixture();
        assert_no_temp_btree(&conn, "select a from t order by a");
        assert_no_temp_btree(
            &conn,
            "select a from t indexed by t_b order by b collate binary, a",
        );
        // A parameterized keyset page is the shape providers actually run.
        assert_no_temp_btree(&conn, "select a from t where a > ?1 order by a limit ?2");

        // An unindexed sort must spill into a temporary B-tree.
        let sorted = std::panic::catch_unwind(|| {
            assert_no_temp_btree(&fixture(), "select a from t order by c")
        });
        assert!(sorted.is_err(), "unindexed sort should be rejected");

        // A self-join with no usable index makes SQLite synthesize one.
        let auto_indexed = std::panic::catch_unwind(|| {
            assert_no_temp_btree(&fixture(), "select l.a from t l join t r on l.c = r.c")
        });
        assert!(auto_indexed.is_err(), "automatic index should be rejected");
    }
}
