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
    let result = reader
        .query("SELECT 1 AS one", RawSqlOptions::default())
        .unwrap();
    assert_eq!(result.rows[0][0], RawSqlValue::Integer(1));
    let snapshot = result.snapshot.unwrap();
    assert_eq!(
        snapshot.projection_status,
        RelationalProjectionStatus::Empty
    );
    assert_eq!(snapshot.relational_core_generation_id, None);
    assert_eq!(snapshot.observed_core_generation_id, None);
    assert!(!snapshot.stale);
    drop(reader);

    let reopened = SqlCompatibility::open_for_data_root(temp.path()).unwrap();
    assert_eq!(
        reopened.metadata().unwrap().status,
        RelationalProjectionStatus::Empty
    );
}

#[test]
fn repeated_stale_ready_reads_progress_while_observed_core_advances() {
    let temp = tempfile::tempdir().unwrap();
    let projection_path = sql_compatibility_path(temp.path());
    let writer = SourceBackedRelationalProjection::open(&projection_path).unwrap();
    drop(writer);
    let relational_generation = "11".repeat(32);
    let connection = Connection::open(&projection_path).unwrap();
    insert_test_source(&connection, 1, "stable-source");
    mark_ready(&connection, &relational_generation, 1, 1);
    drop(connection);

    for observed_core_generation in ["22".repeat(32), "33".repeat(32), "44".repeat(32)] {
        let reader = SqlCompatibility::open_existing_projection_for_observed(
            projection_path.clone(),
            Some(observed_core_generation.clone()),
        )
        .unwrap();
        let result = reader
            .query("SELECT COUNT(*) FROM ctx_sources", RawSqlOptions::default())
            .unwrap();
        assert_eq!(result.rows[0][0], RawSqlValue::Integer(1));
        let snapshot = result.snapshot.unwrap();
        assert_eq!(
            snapshot.projection_status,
            RelationalProjectionStatus::Ready
        );
        assert_eq!(
            snapshot.relational_core_generation_id.as_deref(),
            Some(relational_generation.as_str())
        );
        assert_eq!(
            snapshot.observed_core_generation_id.as_deref(),
            Some(observed_core_generation.as_str())
        );
        assert!(snapshot.stale);
    }
}

#[test]
fn failed_catch_up_keeps_the_last_coherent_projection_queryable() {
    let temp = tempfile::tempdir().unwrap();
    let projection_path = sql_compatibility_path(temp.path());
    let writer = SourceBackedRelationalProjection::open(&projection_path).unwrap();
    drop(writer);
    let relational_generation = "41".repeat(32);
    let observed_core_generation = "42".repeat(32);
    let connection = Connection::open(&projection_path).unwrap();
    insert_test_source(&connection, 1, "stable-source");
    mark_ready(&connection, &relational_generation, 1, 1);
    connection
        .execute(
            "UPDATE core_relational_state
             SET target_generation_id = ?1,
                 status = 'behind',
                 last_error = 'injected catch-up failure'
             WHERE singleton = 1",
            [&observed_core_generation],
        )
        .unwrap();
    drop(connection);

    let reader = SqlCompatibility::open_existing_projection_for_observed(
        projection_path,
        Some(observed_core_generation.clone()),
    )
    .unwrap();
    let result = reader
        .query("SELECT COUNT(*) FROM ctx_sources", RawSqlOptions::default())
        .unwrap();

    assert_eq!(result.rows[0][0], RawSqlValue::Integer(1));
    let snapshot = result.snapshot.unwrap();
    assert_eq!(
        snapshot.projection_status,
        RelationalProjectionStatus::Behind
    );
    assert_eq!(
        snapshot.relational_core_generation_id.as_deref(),
        Some(relational_generation.as_str())
    );
    assert_eq!(
        snapshot.observed_core_generation_id.as_deref(),
        Some(observed_core_generation.as_str())
    );
    assert!(snapshot.stale);
}

#[test]
fn one_sql_handle_pins_rows_and_generation_metadata_in_one_transaction() {
    let temp = tempfile::tempdir().unwrap();
    let projection_path = sql_compatibility_path(temp.path());
    let writer = SourceBackedRelationalProjection::open(&projection_path).unwrap();
    drop(writer);
    let first_generation = "51".repeat(32);
    let second_generation = "52".repeat(32);
    let connection = Connection::open(&projection_path).unwrap();
    insert_test_source(&connection, 1, "first-source");
    mark_ready(&connection, &first_generation, 1, 1);
    drop(connection);

    let pinned = SqlCompatibility::open_existing_projection_for_observed(
        projection_path.clone(),
        Some(first_generation.clone()),
    )
    .unwrap();

    let mut publisher = Connection::open(&projection_path).unwrap();
    let transaction = publisher.transaction().unwrap();
    insert_test_source(&transaction, 2, "second-source");
    mark_ready(&transaction, &second_generation, 2, 2);
    transaction.commit().unwrap();
    drop(publisher);

    let pinned_result = pinned
        .query("SELECT COUNT(*) FROM ctx_sources", RawSqlOptions::default())
        .unwrap();
    assert_eq!(pinned_result.rows[0][0], RawSqlValue::Integer(1));
    let pinned_snapshot = pinned_result.snapshot.unwrap();
    assert_eq!(pinned_snapshot.relational_build_generation, 1);
    assert_eq!(
        pinned_snapshot.relational_core_generation_id.as_deref(),
        Some(first_generation.as_str())
    );
    assert!(!pinned_snapshot.stale);

    let current = SqlCompatibility::open_existing_projection_for_observed(
        projection_path,
        Some(second_generation.clone()),
    )
    .unwrap();
    let current_result = current
        .query("SELECT COUNT(*) FROM ctx_sources", RawSqlOptions::default())
        .unwrap();
    assert_eq!(current_result.rows[0][0], RawSqlValue::Integer(2));
    let current_snapshot = current_result.snapshot.unwrap();
    assert_eq!(current_snapshot.relational_build_generation, 2);
    assert_eq!(
        current_snapshot.relational_core_generation_id.as_deref(),
        Some(second_generation.as_str())
    );
    assert!(!current_snapshot.stale);
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
fn newly_published_core_rejects_an_already_created_empty_projection() {
    let temp = tempfile::tempdir().unwrap();
    let projection =
        SourceBackedRelationalProjection::open(sql_compatibility_path(temp.path())).unwrap();
    drop(projection);
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
        .expect("an empty relational snapshot must not claim to be current after Core publishes");
    assert!(matches!(
        error,
        RelationalProjectionError::SourceBackedSqlGenerationMismatch { status, .. }
            if status == "empty"
    ));
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

fn insert_test_source(connection: &Connection, source_key: i64, source_id: &str) {
    connection
        .execute(
            "INSERT INTO core_sources (
                source_key, source_id, source_digest, provider, source_format,
                schema_variant, provider_identity_version, parser_revision,
                revision_digest, indexed_event_count, health
             ) VALUES (?1, ?2, ?3, 'codex', 'codex_session_jsonl',
                       'session', 1, 'parser-v1', ?4, 0, 'ready')",
            params![
                source_key,
                source_id,
                vec![u8::try_from(source_key).unwrap(); 32],
                vec![u8::try_from(source_key + 16).unwrap(); 32]
            ],
        )
        .unwrap();
}

fn mark_ready(
    connection: &Connection,
    generation_id: &str,
    build_generation: i64,
    source_count: i64,
) {
    connection
        .execute(
            "UPDATE core_relational_state
             SET build_generation = ?1,
                 active_generation_id = ?2,
                 active_manifest_version = 6,
                 active_core_record_version = 1,
                 active_core_record_contract_fingerprint = 'test-contract',
                 active_lexical_schema_version = 1,
                 active_policy_schema_hash = 'test-policy',
                 active_materializer_revision = 4,
                 target_generation_id = NULL,
                 status = 'ready',
                 source_count = ?3,
                 last_error = NULL
             WHERE singleton = 1",
            params![build_generation, generation_id, source_count],
        )
        .unwrap();
}
