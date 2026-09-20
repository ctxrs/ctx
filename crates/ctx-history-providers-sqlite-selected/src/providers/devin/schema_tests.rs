use std::path::PathBuf;

use rusqlite::{Connection, OpenFlags};

use super::schema::DevinNativeSchema;
use crate::CaptureError;

/// The committed fixture, as a traversal-free absolute path.
///
/// The provider source authority refuses a path containing `..`, and the route
/// normalizes before opening, so tests use the same normalized form rather
/// than the raw manifest-relative one.
pub(super) fn fixture_path() -> PathBuf {
    super::database::absolute_devin_path(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/provider-history/devin/v17/sessions.db"),
    )
    .unwrap()
}

pub(super) fn fixture_connection() -> Connection {
    Connection::open_with_flags(fixture_path(), OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

/// A writable copy of the fixture, so a test can mutate the schema without
/// touching the committed file.
///
/// The temporary directory is returned alongside the connection; dropping it
/// would delete the database out from under an open handle.
pub(super) fn mutable_fixture() -> (tempfile::TempDir, Connection) {
    let temp = ctx_history_source_sqlite::test_support::tempdir().unwrap();
    let path = temp.path().join("sessions.db");
    std::fs::copy(fixture_path(), &path).unwrap();
    let conn = Connection::open(&path).unwrap();
    // These tests reshape the schema, including dropping tables other tables
    // reference, so referential enforcement is not wanted here.
    conn.pragma_update(None, "foreign_keys", false).unwrap();
    (temp, conn)
}

#[test]
fn fixture_schema_is_admitted_and_its_digest_is_stable() {
    let conn = fixture_connection();
    let schema = DevinNativeSchema::probe(&conn).expect("fixture schema must be admitted");
    assert_eq!(schema.schema_version, 17);
    assert_eq!(schema.capability_digest.len(), 64);
    assert!(schema
        .capability_digest
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()));

    // Probing twice must agree, or a no-op refresh could not be recognized.
    let again = DevinNativeSchema::probe(&fixture_connection()).unwrap();
    assert_eq!(schema, again);

    for column in ["main_chain_id", "hidden", "working_directory"] {
        assert!(schema.has_session_column(column), "{column}");
    }
    for column in ["row_id", "parent_node_id", "chat_message", "metadata"] {
        assert!(schema.has_node_column(column), "{column}");
    }
    for column in ["tool_call_json", "tool_call_update_json"] {
        assert!(schema.has_tool_state_column(column), "{column}");
    }
    assert!(!schema.has_node_column("no_such_column"));
}

/// Admission does not gate on the migration version, but identity does.
///
/// A Devin release that only adds to the schema must keep importing rather
/// than stalling until ctx ships a new pin. The version still reaches the
/// capability digest, so the same rows under a different migration version
/// publish a different digest and force a full re-scan.
#[test]
fn another_refinery_version_is_admitted_and_rotates_the_capability_digest() {
    let baseline = DevinNativeSchema::probe(&fixture_connection())
        .unwrap()
        .capability_digest;
    let mut digests = std::collections::BTreeSet::new();
    for version in [16, 18, 41] {
        let (_temp, conn) = mutable_fixture();
        conn.execute("delete from refinery_schema_history", [])
            .unwrap();
        conn.execute(
            "insert into refinery_schema_history(version, name, applied_on, checksum) \
             values (?1, 'test', '', '')",
            [version],
        )
        .unwrap();
        let schema = DevinNativeSchema::probe(&conn)
            .unwrap_or_else(|error| panic!("version {version} must be admitted, got {error:?}"));
        assert_eq!(schema.schema_version, version);
        assert_ne!(
            schema.capability_digest, baseline,
            "version {version} must not reuse the fixture digest"
        );
        digests.insert(schema.capability_digest);
    }
    assert_eq!(digests.len(), 3, "each version must have its own digest");
}

/// Structure, not the version, is what refuses a source: a migration that
/// removes a column the importer reads is rejected whatever version it claims.
#[test]
fn a_newer_version_missing_a_required_column_is_still_refused() {
    let (_temp, conn) = mutable_fixture();
    conn.execute("delete from refinery_schema_history", [])
        .unwrap();
    conn.execute(
        "insert into refinery_schema_history(version, name, applied_on, checksum) \
         values (18, 'test', '', '')",
        [],
    )
    .unwrap();
    conn.execute_batch("alter table sessions drop column main_chain_id")
        .unwrap();
    match DevinNativeSchema::probe(&conn) {
        Err(CaptureError::InvalidPayload(detail)) => {
            assert!(detail.contains("main_chain_id"), "{detail}");
        }
        other => panic!("a missing required column must be refused, got {other:?}"),
    }
}

#[test]
fn a_missing_or_empty_migration_history_is_refused() {
    let (_temp, conn) = mutable_fixture();
    conn.execute("delete from refinery_schema_history", [])
        .unwrap();
    assert!(matches!(
        DevinNativeSchema::probe(&conn),
        Err(CaptureError::InvalidPayload(detail)) if detail.contains("records no applied migration")
    ));

    let (_temp, conn) = mutable_fixture();
    conn.execute("drop table refinery_schema_history", [])
        .unwrap();
    assert!(matches!(
        DevinNativeSchema::probe(&conn),
        Err(CaptureError::InvalidPayload(detail))
            if detail.contains("requires the refinery_schema_history table")
    ));
}

#[test]
fn a_missing_table_column_or_unique_index_is_refused() {
    for table in ["sessions", "message_nodes", "tool_call_state"] {
        let (_temp, conn) = mutable_fixture();
        conn.execute_batch(&format!("drop table {table}")).unwrap();
        assert!(
            matches!(
                DevinNativeSchema::probe(&conn),
                Err(CaptureError::InvalidPayload(detail)) if detail.contains(table)
            ),
            "dropping {table} must be refused"
        );
    }

    // The chain walk probes one node per step by (session_id, node_id).
    let (_temp, conn) = mutable_fixture();
    conn.execute_batch(
        "create table nodes_copy as select * from message_nodes;
         drop table message_nodes;
         create table message_nodes (
             row_id integer primary key autoincrement,
             session_id text not null,
             node_id integer not null,
             parent_node_id integer,
             chat_message text not null,
             created_at integer not null,
             metadata text
         );
         insert into message_nodes select * from nodes_copy;",
    )
    .unwrap();
    assert!(matches!(
        DevinNativeSchema::probe(&conn),
        Err(CaptureError::InvalidPayload(detail))
            if detail.contains("UNIQUE BINARY index on (session_id, node_id)")
    ));
}

#[test]
fn row_id_must_be_the_declared_integer_primary_key() {
    let (_temp, conn) = mutable_fixture();
    conn.execute_batch(
        "create table nodes_copy as select * from message_nodes;
         drop table message_nodes;
         create table message_nodes (
             row_id text not null,
             session_id text not null,
             node_id integer not null,
             parent_node_id integer,
             chat_message text not null,
             created_at integer not null,
             metadata text,
             primary key (row_id)
         );
         create unique index message_nodes_session_node on message_nodes (session_id, node_id);
         insert into message_nodes select * from nodes_copy;",
    )
    .unwrap();
    assert!(matches!(
        DevinNativeSchema::probe(&conn),
        Err(CaptureError::InvalidPayload(detail))
            if detail.contains("row_id as its INTEGER primary key")
    ));
}

#[test]
fn a_schema_change_at_the_same_version_rotates_the_capability_digest() {
    let baseline = DevinNativeSchema::probe(&fixture_connection())
        .unwrap()
        .capability_digest;
    let (_temp, conn) = mutable_fixture();
    conn.execute_batch("alter table sessions add column ctx_probe_column text")
        .unwrap();
    let mutated = DevinNativeSchema::probe(&conn).unwrap();
    assert_eq!(mutated.schema_version, 17);
    assert_ne!(
        mutated.capability_digest, baseline,
        "an added column must change the capability digest"
    );
}

/// Devin needs no scratch database, and this is the reason.
///
/// Both ordered scans must ride an index the schema already declares: the
/// session page on `sessions`' primary key, and the per-session node scan on
/// the `UNIQUE(session_id, node_id)` automatic index. If either ever spilled
/// into a temporary B-tree, ordering would consume unbounded storage outside
/// the byte authority and the provider would need the scratch machinery the
/// other selected providers use.
#[test]
fn both_ordered_scans_stay_on_a_declared_index() {
    use ctx_history_source_sqlite::test_support::assert_no_temp_btree;

    let conn = fixture_connection();
    let schema = DevinNativeSchema::probe(&conn).unwrap();
    assert_no_temp_btree(&conn, &super::stream::session_page_sql(&schema));
    assert_no_temp_btree(&conn, super::stream::SESSION_FACTS_SQL);

    // The rejected alternative proves the assertion has teeth: ordering the
    // same scan by `created_at` — the ordering this provider deliberately does
    // not use — spills, because no index covers it.
    let spilled = std::panic::catch_unwind(|| {
        assert_no_temp_btree(
            &fixture_connection(),
            "select node_id from message_nodes where session_id = ?1 order by created_at",
        )
    });
    assert!(spilled.is_err(), "a created_at ordering must be rejected");
}
