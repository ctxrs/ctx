use super::*;

#[test]
fn sqlite_vec0_full_scan_matches_rust_scan() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
    let close_event = Uuid::new_v4();
    let far_event = Uuid::new_v4();
    store.upsert_chunk_embeddings(&[
        (
            test_chunk(close_event, 2, "close"),
            test_embedding(1.0, 0.0),
        ),
        (test_chunk(far_event, 1, "far"), test_embedding(0.0, 1.0)),
    ])?;

    assert!(store.sqlite_vec0_ready()?);

    let query = test_embedding(1.0, 0.0);
    let sqlite_hits = store.search(&query, 2)?;
    let rust_hits = store.search_event_ids(&query, &[close_event, far_event], 2)?;

    assert_eq!(
        sqlite_hits.stats.backend,
        Some(SEMANTIC_VECTOR_BACKEND_SQLITE_VEC)
    );
    assert_eq!(rust_hits.stats.backend, Some(SEMANTIC_VECTOR_BACKEND_RUST));
    assert_eq!(sqlite_hits.hits.len(), 2);
    assert_eq!(rust_hits.hits.len(), 2);
    assert_eq!(sqlite_hits.hits[0].event_id, close_event);
    assert_eq!(rust_hits.hits[0].event_id, close_event);
    assert_eq!(sqlite_hits.hits[1].event_id, far_event);
    assert_eq!(rust_hits.hits[1].event_id, far_event);
    Ok(())
}

#[test]
fn sqlite_vec0_caps_large_k_without_falling_back() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
    let close_event = Uuid::new_v4();
    let far_event = Uuid::new_v4();
    store.upsert_chunk_embeddings(&[
        (
            test_chunk(close_event, 2, "close"),
            test_embedding(1.0, 0.0),
        ),
        (test_chunk(far_event, 1, "far"), test_embedding(0.0, 1.0)),
    ])?;

    let search = store.search(&test_embedding(1.0, 0.0), SEMANTIC_SQLITE_VEC0_MAX_K + 1)?;

    assert_eq!(
        search.stats.backend,
        Some(SEMANTIC_VECTOR_BACKEND_SQLITE_VEC)
    );
    assert_eq!(search.hits.len(), 2);
    assert_eq!(search.hits[0].event_id, close_event);
    Ok(())
}

#[test]
fn rust_full_scan_requires_sidecar_within_cap_without_vec0() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
    let chunk_limit = semantic_rust_full_scan_chunk_limit();
    store.conn.execute(
        r#"
            INSERT INTO semantic_index_stats
                (model_key, embedded_items, embedded_chunks, updated_at_ms)
            VALUES (?1, 1, ?2, 1)
            "#,
        params![semantic_model_key(), (chunk_limit + 1) as i64],
    )?;

    assert!(!semantic_full_corpus_vector_scan_ready(&store)?);

    store.conn.execute(
        r#"
            UPDATE semantic_index_stats
            SET embedded_chunks = ?2
            WHERE model_key = ?1
            "#,
        params![semantic_model_key(), chunk_limit as i64],
    )?;
    assert!(semantic_full_corpus_vector_scan_ready(&store)?);
    Ok(())
}

#[test]
fn opening_vector_store_preserves_other_embedding_spaces_and_current_cursor() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let vector_path = temp.path().join("vectors.sqlite");
    let old_model_key = "fastembed:all-MiniLM-L6-v2:old";
    {
        let store = SemanticVectorStore::open(&vector_path)?;
        store.conn.execute(
                r#"
                INSERT INTO embedding_models
                    (model_key, backend, model_id, dimensions, distance, normalized, created_at_ms)
                VALUES (?1, 'fastembed', 'sentence-transformers/all-MiniLM-L6-v2', 384, 'cosine', 1, 1)
                "#,
                [old_model_key],
            )?;
        store.conn.execute(
            r#"
                INSERT INTO event_embedding_chunks
                    (event_id, model_key, event_seq, chunk_index, chunk_count,
                     source_text_sha256, chunk_text_sha256, start_char, end_char,
                     dimensions, embedding_f32, embedded_at_ms)
                VALUES (?1, ?2, 1, 0, 1, 'source', 'chunk', 0, 5, 384, ?3, 1)
                "#,
            params![
                Uuid::new_v4().to_string(),
                old_model_key,
                serialize_f32_blob(&test_embedding(1.0, 0.0))
            ],
        )?;
        store.set_backfill_cursor(Some((123, 456)))?;
    }

    let store = SemanticVectorStore::open(&vector_path)?;
    let old_rows = store.conn.query_row(
        "SELECT COUNT(*) FROM event_embedding_chunks WHERE model_key = ?1",
        [old_model_key],
        |row| row.get::<_, i64>(0),
    )?;
    let old_models = store.conn.query_row(
        "SELECT COUNT(*) FROM embedding_models WHERE model_key = ?1",
        [old_model_key],
        |row| row.get::<_, i64>(0),
    )?;

    assert_eq!(old_rows, 1);
    assert_eq!(old_models, 1);
    assert_eq!(store.backfill_cursor()?, Some((123, 456)));
    Ok(())
}

#[test]
fn prune_ineligible_events_is_bounded_and_advances_cursor() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let docs = write_searchable_store(temp.path(), SEMANTIC_PRUNE_EVENTS_PER_PASS + 1)?;
    let store = Store::open(database_path(temp.path().to_path_buf()))?;
    let mut vector_store = SemanticVectorStore::open(&semantic_vector_path(temp.path()))?;
    let chunks = docs
        .iter()
        .map(|doc| {
            (
                test_chunk(doc.event_id, doc.seq, "intentionally-stale"),
                test_embedding(1.0, 0.0),
            )
        })
        .collect::<Vec<_>>();
    vector_store.upsert_chunk_embeddings(&chunks)?;
    assert_eq!(
        vector_store.cached_or_exact_stats()?.embedded_items,
        SEMANTIC_PRUNE_EVENTS_PER_PASS + 1
    );

    let first = vector_store.prune_ineligible_events(&store)?;
    assert_eq!(first.queued_stale_events, SEMANTIC_PRUNE_EVENTS_PER_PASS);
    assert_eq!(
        vector_store.cached_or_exact_stats()?.embedded_items,
        1,
        "first pass should leave the oldest event for the next cursor page"
    );

    let second = vector_store.prune_ineligible_events(&store)?;
    assert_eq!(second.queued_stale_events, 1);
    assert_eq!(vector_store.cached_or_exact_stats()?.embedded_items, 0);
    assert_eq!(
        vector_store.dirty_event_count()?,
        SEMANTIC_PRUNE_EVENTS_PER_PASS + 1
    );
    Ok(())
}

#[test]
fn sqlite_vec0_overfetches_until_unique_events_match_rust_scan() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
    let multi_chunk_event = Uuid::new_v4();
    let next_event = Uuid::new_v4();
    store.upsert_chunk_embeddings(&[
        (
            test_chunk_at(multi_chunk_event, 2, "multi", 0, 3),
            test_embedding(1.0, 0.0),
        ),
        (
            test_chunk_at(multi_chunk_event, 2, "multi", 1, 3),
            test_embedding(0.999, 0.044),
        ),
        (
            test_chunk_at(multi_chunk_event, 2, "multi", 2, 3),
            test_embedding(0.995, 0.099),
        ),
        (
            test_chunk_at(next_event, 1, "next", 0, 1),
            test_embedding(0.98, 0.199),
        ),
    ])?;

    let query = test_embedding(1.0, 0.0);
    let sqlite_hits = store.search(&query, 2)?;
    let rust_hits = store.search_event_ids(&query, &[multi_chunk_event, next_event], 2)?;

    assert_eq!(
        sqlite_hits.stats.backend,
        Some(SEMANTIC_VECTOR_BACKEND_SQLITE_VEC)
    );
    assert_eq!(sqlite_hits.hits.len(), 2);
    assert_eq!(sqlite_hits.hits[0].event_id, multi_chunk_event);
    assert_eq!(sqlite_hits.hits[1].event_id, next_event);
    assert_eq!(
        sqlite_hits
            .hits
            .iter()
            .map(|hit| hit.event_id)
            .collect::<Vec<_>>(),
        rust_hits
            .hits
            .iter()
            .map(|hit| hit.event_id)
            .collect::<Vec<_>>()
    );
    Ok(())
}

#[test]
fn sqlite_vec0_rebuilds_incompatible_derived_schema() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let vector_path = temp.path().join("vectors.sqlite");
    {
        let conn = Connection::open(&vector_path)?;
        conn.execute_batch(
            r#"
                CREATE TABLE event_embedding_vec0_meta (
                    rowid INTEGER PRIMARY KEY,
                    event_id TEXT NOT NULL
                );
                CREATE TABLE event_embedding_vec0 (
                    rowid INTEGER PRIMARY KEY,
                    embedding BLOB
                );
                "#,
        )?;
    }

    let mut store = SemanticVectorStore::open(&vector_path)?;
    let close_event = Uuid::new_v4();
    store.upsert_chunk_embeddings(&[(
        test_chunk(close_event, 1, "close"),
        test_embedding(1.0, 0.0),
    )])?;

    assert!(store.sqlite_vec0_ready()?);
    let vec0_sql = sqlite_table_sql(&store.conn, "event_embedding_vec0")?.unwrap_or_default();
    assert!(vec0_sql.to_ascii_lowercase().contains("using vec0"));
    assert!(sqlite_table_has_columns(
        &store.conn,
        "event_embedding_vec0_meta",
        &["model_key", "source_text_sha256", "start_char", "end_char"]
    )?);
    Ok(())
}

#[test]
fn sqlite_vec0_rebuilds_when_same_count_meta_rowids_drift() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
    let close_event = Uuid::new_v4();
    let far_event = Uuid::new_v4();
    store.upsert_chunk_embeddings(&[
        (
            test_chunk(close_event, 2, "close"),
            test_embedding(1.0, 0.0),
        ),
        (test_chunk(far_event, 1, "far"), test_embedding(0.0, 1.0)),
    ])?;
    assert!(store.sqlite_vec0_ready()?);

    let canonical_rowid = store.conn.query_row(
        "SELECT rowid FROM event_embedding_chunks WHERE event_id = ?1 AND model_key = ?2",
        params![close_event.to_string(), semantic_model_key()],
        |row| row.get::<_, i64>(0),
    )?;
    store.conn.execute(
	            "UPDATE event_embedding_vec0_meta SET rowid = rowid + 1000 WHERE event_id = ?1 AND model_key = ?2",
	            params![close_event.to_string(), semantic_model_key()],
	        )?;

    assert!(!store.sqlite_vec0_ready()?);
    store.sync_sqlite_vec0_from_chunks_if_needed()?;
    assert!(store.sqlite_vec0_ready()?);

    let repaired_rowid = store.conn.query_row(
        "SELECT rowid FROM event_embedding_vec0_meta WHERE event_id = ?1 AND model_key = ?2",
        params![close_event.to_string(), semantic_model_key()],
        |row| row.get::<_, i64>(0),
    )?;
    assert_eq!(repaired_rowid, canonical_rowid);
    Ok(())
}

#[test]
fn sqlite_vec0_payload_drift_is_repaired_by_maintenance() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut store = SemanticVectorStore::open(&temp.path().join("vectors.sqlite"))?;
    let close_event = Uuid::new_v4();
    let far_event = Uuid::new_v4();
    store.upsert_chunk_embeddings(&[
        (
            test_chunk(close_event, 2, "close"),
            test_embedding(1.0, 0.0),
        ),
        (test_chunk(far_event, 1, "far"), test_embedding(0.0, 1.0)),
    ])?;
    assert!(store.sqlite_vec0_ready()?);

    let close_rowid = store.conn.query_row(
        "SELECT rowid FROM event_embedding_chunks WHERE event_id = ?1 AND model_key = ?2",
        params![close_event.to_string(), semantic_model_key()],
        |row| row.get::<_, i64>(0),
    )?;
    store.conn.execute(
        "DELETE FROM event_embedding_vec0 WHERE rowid = ?1",
        params![close_rowid],
    )?;
    store.conn.execute(
        "INSERT INTO event_embedding_vec0(rowid, embedding) VALUES (?1, ?2)",
        params![close_rowid, serialize_f32_blob(&test_embedding(0.0, 1.0))],
    )?;

    assert!(!store.sqlite_vec0_ready()?);
    assert!(
            store.sqlite_vec0_search_ready()?,
            "search hot path should use cheap count readiness and leave deep integrity checks to maintenance"
        );

    store.sync_sqlite_vec0_from_chunks_if_needed()?;
    assert!(store.sqlite_vec0_ready()?);
    Ok(())
}
