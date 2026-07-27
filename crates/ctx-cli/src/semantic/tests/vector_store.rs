use super::super::{
    query_service::semantic_candidate_limit,
    vector_store_schema::{
        semantic_vector_failure_kind, set_semantic_before_sqlite_open_hook,
        SemanticVectorFailureKind, SEMANTIC_SQLITE_VEC0_MAX_K, SEMANTIC_VECTOR_APPLICATION_ID,
        SEMANTIC_VECTOR_BACKEND_SQLITE_VEC, SEMANTIC_VECTOR_SCHEMA_VERSION,
    },
    vector_store_search::semantic_sqlite_vec0_query_bounds,
};
use super::*;

fn create_legacy_sidecar(path: &Path, user_version: i64) -> Result<()> {
    assert!(matches!(user_version, 3 | 5));
    let connection = Connection::open(path)?;
    connection.execute_batch(
        r#"
        CREATE TABLE embedding_models (
            model_key TEXT PRIMARY KEY,
            backend TEXT NOT NULL,
            model_id TEXT NOT NULL,
            dimensions INTEGER NOT NULL,
            distance TEXT NOT NULL,
            normalized INTEGER NOT NULL,
            created_at_ms INTEGER NOT NULL
        );
        CREATE TABLE event_embeddings (
            event_id TEXT NOT NULL,
            model_key TEXT NOT NULL,
            history_record_id TEXT,
            session_id TEXT,
            event_seq INTEGER NOT NULL,
            text_sha256 TEXT NOT NULL,
            preview_text TEXT NOT NULL DEFAULT '',
            dimensions INTEGER NOT NULL,
            embedding_f32 BLOB NOT NULL,
            embedded_at_ms INTEGER NOT NULL,
            PRIMARY KEY (event_id, model_key)
        );
        CREATE TABLE event_embedding_chunks (
            event_id TEXT NOT NULL,
            model_key TEXT NOT NULL,
            history_record_id TEXT,
            session_id TEXT,
            event_seq INTEGER NOT NULL,
            chunk_index INTEGER NOT NULL,
            chunk_count INTEGER NOT NULL,
            source_text_sha256 TEXT NOT NULL,
            chunk_text_sha256 TEXT NOT NULL,
            chunk_text TEXT NOT NULL DEFAULT '',
            start_char INTEGER NOT NULL,
            end_char INTEGER NOT NULL,
            dimensions INTEGER NOT NULL,
            embedding_f32 BLOB NOT NULL,
            embedded_at_ms INTEGER NOT NULL,
            PRIMARY KEY (event_id, model_key, chunk_index)
        );
        CREATE TABLE semantic_index_stats (
            model_key TEXT PRIMARY KEY,
            embedded_items INTEGER NOT NULL,
            embedded_chunks INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL
        );
        CREATE TABLE semantic_dirty_events (
            event_id TEXT NOT NULL,
            model_key TEXT NOT NULL,
            queued_at_ms INTEGER NOT NULL,
            priority_seq INTEGER,
            reason TEXT NOT NULL,
            attempts INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (event_id, model_key)
        );
        "#,
    )?;
    connection.pragma_update(None, "user_version", user_version)?;
    Ok(())
}

#[test]
fn clean_v6_schema_has_one_vector_representation_and_no_plaintext() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("vectors.sqlite");
    let store = SemanticVectorStore::open(&path)?;

    let application_id = store
        .conn
        .query_row("PRAGMA application_id", [], |row| row.get::<_, i64>(0))?;
    let user_version = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?;
    let schema = store.conn.query_row(
        "SELECT group_concat(COALESCE(sql, ''), '\n') FROM sqlite_schema",
        [],
        |row| row.get::<_, String>(0),
    )?;

    assert_eq!(application_id, SEMANTIC_VECTOR_APPLICATION_ID);
    assert_eq!(user_version, SEMANTIC_VECTOR_SCHEMA_VERSION);
    assert!(schema.contains("USING vec0"));
    assert!(schema.contains("chunk_id INTEGER PRIMARY KEY"));
    assert!(!schema.contains("embedding_f32"));
    assert!(!schema.contains("chunk_text"));
    assert!(!schema.contains("event_embeddings"));
    assert_eq!(store.plaintext_value_count()?, 0);
    Ok(())
}

#[test]
fn recognized_v3_and_v5_sidecars_reset_to_clean_v6() -> Result<()> {
    for version in [3, 5] {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join(format!("vectors-v{version}.sqlite"));
        create_legacy_sidecar(&path, version)?;

        let store = SemanticVectorStore::open(&path)?;
        let user_version = store
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?;
        let old_embedding_table = store.conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sqlite_schema
                WHERE type = 'table' AND name = 'event_embeddings'
             )",
            [],
            |row| row.get::<_, bool>(0),
        )?;

        assert_eq!(user_version, SEMANTIC_VECTOR_SCHEMA_VERSION);
        assert!(!old_embedding_table);
    }
    Ok(())
}

#[test]
fn future_owned_schema_is_preserved_and_rejected() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("vectors.sqlite");
    drop(SemanticVectorStore::open(&path)?);
    {
        let connection = Connection::open(&path)?;
        connection.execute_batch(
            r#"
            CREATE TABLE future_marker(value TEXT NOT NULL);
            INSERT INTO future_marker(value) VALUES ('preserve-me');
            PRAGMA user_version = 7;
            "#,
        )?;
    }

    let error = SemanticVectorStore::open(&path)
        .err()
        .expect("future schema must be rejected");
    assert_eq!(
        semantic_vector_failure_kind(&error),
        Some(SemanticVectorFailureKind::NewerSchema)
    );
    let connection = Connection::open(&path)?;
    let marker = connection.query_row("SELECT value FROM future_marker", [], |row| {
        row.get::<_, String>(0)
    })?;
    assert_eq!(marker, "preserve-me");
    assert_eq!(
        connection.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?,
        7
    );
    Ok(())
}

#[test]
fn unrecognized_and_corrupt_files_are_preserved_for_manual_repair() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let unrecognized = temp.path().join("unrecognized.sqlite");
    {
        let connection = Connection::open(&unrecognized)?;
        connection.execute_batch(
            "CREATE TABLE user_data(value TEXT NOT NULL);
             INSERT INTO user_data VALUES ('preserve-me');
             PRAGMA user_version = 5;",
        )?;
    }
    let error = SemanticVectorStore::open(&unrecognized)
        .err()
        .expect("unrecognized v5 must be preserved");
    assert_eq!(
        semantic_vector_failure_kind(&error),
        Some(SemanticVectorFailureKind::StorageConflict)
    );
    let connection = Connection::open(&unrecognized)?;
    assert_eq!(
        connection.query_row("SELECT value FROM user_data", [], |row| row
            .get::<_, String>(0))?,
        "preserve-me"
    );

    let corrupt = temp.path().join("corrupt.sqlite");
    fs::write(&corrupt, b"not a sqlite database; preserve these bytes")?;
    let before = fs::read(&corrupt)?;
    let error = SemanticVectorStore::open(&corrupt)
        .err()
        .expect("ambiguous corruption must require manual repair");
    assert_eq!(
        semantic_vector_failure_kind(&error),
        Some(SemanticVectorFailureKind::ResetRequired)
    );
    assert_eq!(fs::read(&corrupt)?, before);
    Ok(())
}

#[test]
fn sqlite_open_replacement_race_is_rejected_before_validation_writes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("vectors.sqlite");
    let displaced = temp.path().join("displaced.sqlite");
    {
        let connection = Connection::open(&path)?;
        connection.pragma_update(None, "user_version", 41)?;
    }
    let hook_path = path.clone();
    set_semantic_before_sqlite_open_hook(move |path| {
        fs::rename(path, &displaced)?;
        let replacement = Connection::open(&hook_path)?;
        replacement.execute_batch(
            r#"
            CREATE TABLE replacement_marker(value TEXT NOT NULL);
            INSERT INTO replacement_marker(value) VALUES ('untouched');
            PRAGMA user_version = 42;
            "#,
        )?;
        Ok(())
    });

    let error = SemanticVectorStore::open(&path)
        .err()
        .expect("replacement race must fail closed");
    assert_eq!(
        semantic_vector_failure_kind(&error),
        Some(SemanticVectorFailureKind::Unavailable)
    );
    let replacement = Connection::open(&path)?;
    assert_eq!(
        replacement.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?,
        42
    );
    assert_eq!(
        replacement.query_row("SELECT value FROM replacement_marker", [], |row| {
            row.get::<_, String>(0)
        })?,
        "untouched"
    );
    Ok(())
}

#[test]
fn chunk_ids_survive_delete_and_vacuum_aligned_with_vec0() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
    let removed = Uuid::new_v4();
    let retained = Uuid::new_v4();
    store.upsert_chunk_embeddings(&[
        (test_chunk(removed, 2, "removed"), test_embedding(1.0, 0.0)),
        (
            test_chunk(retained, 1, "retained"),
            test_embedding(0.0, 1.0),
        ),
    ])?;
    let retained_id = store.conn.query_row(
        "SELECT chunk_id FROM event_embedding_chunks WHERE event_id = ?1",
        [retained.to_string()],
        |row| row.get::<_, i64>(0),
    )?;

    assert_eq!(store.delete_embedding_chunks_for_event_ids(&[removed])?, 1);
    store.conn.execute_batch("VACUUM;")?;

    assert_eq!(
        store.conn.query_row(
            "SELECT chunk_id FROM event_embedding_chunks WHERE event_id = ?1",
            [retained.to_string()],
            |row| row.get::<_, i64>(0)
        )?,
        retained_id
    );
    assert_eq!(
        store.conn.query_row(
            "SELECT COUNT(*) FROM event_embedding_chunks AS m
             JOIN event_embedding_vec0 AS v ON v.rowid = m.chunk_id",
            [],
            |row| row.get::<_, i64>(0)
        )?,
        1
    );
    Ok(())
}

#[test]
fn vec0_search_is_unique_deterministic_bounded_and_refuses_unsafe_k() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    store.upsert_chunk_embeddings(&[
        (
            test_chunk_at(first, 2, "first", 0, 2),
            test_embedding(1.0, 0.0),
        ),
        (
            test_chunk_at(first, 2, "first", 1, 2),
            test_embedding(0.99, 0.01),
        ),
        (test_chunk(second, 1, "second"), test_embedding(1.0, 0.0)),
    ])?;

    let search = store.search(&test_embedding(1.0, 0.0), 2)?;
    let mut expected = vec![first, second];
    expected.sort();
    assert_eq!(
        search
            .hits
            .iter()
            .map(|hit| hit.event_id)
            .collect::<Vec<_>>(),
        expected
    );
    assert_eq!(
        search.stats.backend,
        Some(SEMANTIC_VECTOR_BACKEND_SQLITE_VEC)
    );
    let error = store
        .search(&test_embedding(1.0, 0.0), SEMANTIC_SQLITE_VEC0_MAX_K + 1)
        .err()
        .expect("unsafe sqlite-vec k must be refused");
    assert_eq!(
        semantic_vector_failure_kind(&error),
        Some(SemanticVectorFailureKind::Unavailable)
    );
    Ok(())
}

#[test]
fn semantic_candidate_overfetch_is_clamped_before_vec0_query() {
    for (user_limit, expected_candidate_limit) in [
        (40, 4_000),
        (41, SEMANTIC_SQLITE_VEC0_MAX_K),
        (200, SEMANTIC_SQLITE_VEC0_MAX_K),
    ] {
        let options = ctx_history_search::PacketOptions {
            limit: user_limit,
            ..ctx_history_search::PacketOptions::default()
        };
        let candidate_limit = semantic_candidate_limit(&options);
        assert_eq!(candidate_limit, expected_candidate_limit);

        let (initial_k, maximum_k) = semantic_sqlite_vec0_query_bounds(candidate_limit, usize::MAX);
        assert_eq!(maximum_k, SEMANTIC_SQLITE_VEC0_MAX_K);
        assert_eq!(initial_k, SEMANTIC_SQLITE_VEC0_MAX_K);
    }
}

#[test]
fn clamped_semantic_candidates_preserve_ranked_limits_and_pagination() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let documents = write_late_activity_searchable_store(temp.path(), 201)?;
    let store = Store::open(database_path(temp.path().to_path_buf()))?;
    let mut vector_store = SemanticVectorStore::open(&semantic_vector_path(temp.path()))?;
    let expected_event_ids = documents
        .iter()
        .map(|document| document.event_id)
        .collect::<Vec<_>>();
    let embedded = documents
        .iter()
        .enumerate()
        .map(|(rank, document)| {
            let source_text = semantic_source_text(&document.text);
            let source_hash = semantic_document_hash(document, &source_text);
            (
                test_chunk(document.event_id, document.seq, &source_hash),
                test_embedding(1.0, rank as f32 * 0.01),
            )
        })
        .collect::<Vec<_>>();
    vector_store.upsert_chunk_embeddings(&embedded)?;
    let query_embedding = test_embedding(1.0, 0.0);

    for user_limit in [40, 41, 200] {
        let options = ctx_history_search::PacketOptions {
            limit: user_limit,
            ..ctx_history_search::PacketOptions::default()
        };
        let candidate_limit = semantic_candidate_limit(&options);
        let semantic_search =
            semantic_hits_for_query(&store, &vector_store, &query_embedding, candidate_limit)?;
        assert_eq!(semantic_search.hits.len(), expected_event_ids.len());
        assert_eq!(
            semantic_search
                .hits
                .iter()
                .map(|hit| hit.hit.event_id)
                .collect::<Vec<_>>(),
            expected_event_ids
        );

        let packet = ctx_history_search::semantic_event_search_packet(
            &store,
            "semantic daemon scheduling fixture",
            &options,
            &semantic_search.hits,
            1.0,
            false,
        )?;
        assert_eq!(packet.results.len(), user_limit);
        assert_eq!(
            packet
                .results
                .iter()
                .map(|result| result.event_id.expect("semantic event result"))
                .collect::<Vec<_>>(),
            expected_event_ids[..user_limit]
        );
        assert!(packet.pagination.has_more);
        let expected_cursor = format!("offset:{user_limit}");
        assert_eq!(
            packet.pagination.cursor.as_deref(),
            Some(expected_cursor.as_str())
        );
        assert!(packet.truncation.truncated);
        assert_eq!(packet.truncation.reason.as_deref(), Some("limit"));
    }
    Ok(())
}
