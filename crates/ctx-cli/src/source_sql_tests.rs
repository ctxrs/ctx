use ctx_history_relational::{
    RawSqlOptions, RawSqlValue, RelationalProjectionError, RelationalProjectionStatus,
    SourceBackedRelationalProjection,
};
use rusqlite::{params, Connection};

use super::{sql_compatibility_path, SqlCompatibility};

#[test]
fn sql_compatibility_opens_only_the_independent_projection() {
    let temp = tempfile::tempdir().unwrap();
    let path = sql_compatibility_path(temp.path());
    let writer = SourceBackedRelationalProjection::open(&path).unwrap();
    assert_eq!(
        writer.metadata().unwrap().status,
        RelationalProjectionStatus::Empty
    );
    drop(writer);

    let reader = SqlCompatibility::open(&path).unwrap();
    assert_eq!(
        reader.metadata().unwrap().status,
        RelationalProjectionStatus::Empty
    );
    let result = reader
        .query(
            "SELECT COUNT(*) AS sessions FROM ctx_sessions",
            RawSqlOptions::default(),
        )
        .unwrap();
    assert_eq!(result.columns[0].name, "sessions");
    assert_eq!(result.rows[0][0], RawSqlValue::Integer(0));
    assert!(reader
        .query(
            "DELETE FROM source_backed_relational_state",
            RawSqlOptions::default()
        )
        .is_err());
}

#[test]
fn fresh_sql_compatibility_initializes_only_the_relational_projection() {
    let temp = tempfile::tempdir().unwrap();
    let reader = SqlCompatibility::open_for_data_root(temp.path()).unwrap();

    assert!(sql_compatibility_path(temp.path()).is_file());
    assert!(!temp.path().join("work.sqlite").exists());
    assert_eq!(
        reader
            .query("SELECT 1 AS one", RawSqlOptions::default())
            .unwrap()
            .rows[0][0],
        RawSqlValue::Integer(1)
    );
    drop(reader);

    let reopened = SqlCompatibility::open_for_data_root(temp.path()).unwrap();
    assert_eq!(
        reopened.metadata().unwrap().status,
        RelationalProjectionStatus::Empty
    );
}

#[test]
fn committed_core_generation_without_relational_projection_fails_closed() {
    let temp = tempfile::tempdir().unwrap();
    let generation_root = temp.path().join("search").join("lexical");
    ctx_history_index::GenerationWriter::open(
        &generation_root,
        ctx_history_index::WriterOptions::default(),
    )
    .unwrap()
    .commit(|_| true)
    .unwrap();

    let error = SqlCompatibility::open_for_data_root(temp.path())
        .err()
        .expect("missing relational projection should fail");
    assert!(error.to_string().contains("Core SQL projection is missing"));
    assert!(!temp.path().join("work.sqlite").exists());
    assert!(!sql_compatibility_path(temp.path()).exists());
}

#[test]
fn missing_core_generation_rejects_a_stale_relational_projection() {
    let temp = tempfile::tempdir().unwrap();
    let generation_root = temp.path().join("search").join("lexical");
    let generation_id = ctx_history_index::GenerationWriter::open(
        &generation_root,
        ctx_history_index::WriterOptions::default(),
    )
    .unwrap()
    .commit(|_| true)
    .unwrap()
    .generation_id;
    let projection_path = sql_compatibility_path(temp.path());
    let writer = SourceBackedRelationalProjection::open(&projection_path).unwrap();
    drop(writer);

    let connection = Connection::open(&projection_path).unwrap();
    connection
        .execute(
            "INSERT INTO core_sources (
                source_key, source_id, source_digest, provider, source_format,
                schema_variant, provider_identity_version, parser_revision,
                revision_digest, indexed_event_count, health
             ) VALUES (1, 'stale-source', ?1, 'codex', 'codex_session_jsonl',
                       'session', 1, 'parser-v1', ?2, 0, 'ready')",
            params![vec![1_u8; 32], vec![2_u8; 32]],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE core_relational_state
             SET build_generation = 1,
                 active_generation_id = ?1,
                 active_manifest_version = 1,
                 active_core_record_version = 1,
                 active_core_record_contract_fingerprint = 'stale-contract',
                 active_lexical_schema_version = 1,
                 active_policy_schema_hash = 'stale-policy',
                 active_materializer_revision = 1,
                 status = 'ready',
                 source_count = 1
             WHERE singleton = 1",
            [&generation_id],
        )
        .unwrap();
    drop(connection);

    let current = SqlCompatibility::open_for_data_root(temp.path()).unwrap();
    assert_eq!(
        current
            .query("SELECT COUNT(*) FROM ctx_sources", RawSqlOptions::default())
            .unwrap()
            .rows[0][0],
        RawSqlValue::Integer(1)
    );
    drop(current);

    std::fs::remove_file(generation_root.join("active-generation.json")).unwrap();
    let error = SqlCompatibility::open_for_data_root(temp.path())
        .err()
        .expect("a stale projection must not become authority when Core is absent");

    assert!(matches!(
        error,
        RelationalProjectionError::IncompatibleState(detail)
            if detail.contains("Core generation is absent")
    ));
}
