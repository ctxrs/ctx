use super::super::{
    runtime_limits::SEMANTIC_EXACT_TOP_K_MAX,
    vector_store_schema::{
        semantic_vector_failure_kind, SemanticVectorFailureKind, SEMANTIC_VECTOR_BACKEND_FLAT_F32,
        SEMANTIC_VECTOR_SCHEMA_VERSION,
    },
    vector_store_search::scan_exact_generation,
};
use super::*;

fn exact_search(
    store: &SemanticVectorStore,
    query_embedding: &[f32],
    limit: usize,
) -> Result<SemanticVectorSearch> {
    let pinned = store
        .flat_pin_generation()?
        .expect("fixture must publish a flat generation");
    scan_exact_generation(
        &pinned,
        query_embedding,
        limit,
        None,
        std::time::Instant::now(),
    )
}

fn active_counts(store: &SemanticVectorStore) -> Result<(usize, usize)> {
    let pinned = store
        .flat_pin_generation()?
        .expect("fixture must publish a flat generation");
    Ok((pinned.stats().active_events, pinned.stats().active_chunks))
}

#[test]
fn flat_store_control_catalog_has_no_vectors_or_plaintext() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = source_backed_semantic_vector_path(temp.path());
    assert_eq!(root, temp.path().join("search").join("semantic"));
    let store = SemanticVectorStore::open(&root)?;

    assert!(root.is_dir());
    assert!(root.join("state.sqlite").is_file());
    assert_eq!(
        store
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?,
        SEMANTIC_VECTOR_SCHEMA_VERSION
    );
    let schema = store.conn.query_row(
        "SELECT group_concat(COALESCE(sql, ''), '\n') FROM sqlite_schema",
        [],
        |row| row.get::<_, String>(0),
    )?;
    assert!(schema.contains("semantic_source_documents"));
    assert!(!schema.contains("locator_json"));
    assert!(!schema.contains("embedding_f32"));
    assert!(!schema.contains("event_embedding"));
    assert!(!schema.contains("chunk_text"));
    assert!(!schema.contains("USING vec"));
    Ok(())
}

#[test]
fn flat_search_is_unique_deterministic_bounded_and_exact() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut store = SemanticVectorStore::open(&source_backed_semantic_vector_path(temp.path()))?;
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let first_chunk = test_chunk_at(first, 2, "first", 0, 2);
    let first_hash = first_chunk.source_text_hash.clone();
    store.upsert_chunk_embeddings(&[
        (first_chunk, test_embedding(1.0, 0.0)),
        (
            test_chunk_at(first, 2, "first", 1, 2),
            test_embedding(0.8, 0.6),
        ),
        (test_chunk(second, 1, "second"), test_embedding(1.0, 0.0)),
    ])?;

    let search = exact_search(&store, &test_embedding(1.0, 0.0), 2)?;
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
    assert_eq!(search.stats.backend, Some(SEMANTIC_VECTOR_BACKEND_FLAT_F32));
    assert_eq!(search.stats.chunks_scanned, 3);
    assert_eq!(search.stats.events_scored, 2);
    assert_eq!(
        search
            .hits
            .iter()
            .find(|hit| hit.event_id == first)
            .expect("first event")
            .source_text_hash,
        first_hash
    );
    let error = exact_search(
        &store,
        &test_embedding(1.0, 0.0),
        SEMANTIC_EXACT_TOP_K_MAX + 1,
    )
    .err()
    .expect("unsafe exact top-k must be refused");
    assert_eq!(
        semantic_vector_failure_kind(&error),
        Some(SemanticVectorFailureKind::Unavailable)
    );
    Ok(())
}

#[test]
fn flat_rewrite_truncation_delete_and_restart_do_not_resurrect_chunks() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = source_backed_semantic_vector_path(temp.path());
    let rewritten = Uuid::new_v4();
    let deleted = Uuid::new_v4();
    let rewritten_chunk = test_chunk(rewritten, 4, "rewrite-new");
    let rewritten_hash = rewritten_chunk.source_text_hash.clone();
    {
        let mut store = SemanticVectorStore::open(&root)?;
        store.upsert_chunk_embeddings(&[
            (
                test_chunk_at(rewritten, 4, "rewrite-old", 0, 2),
                test_embedding(1.0, 0.0),
            ),
            (
                test_chunk_at(rewritten, 4, "rewrite-old", 1, 2),
                test_embedding(1.0, 0.0),
            ),
            (
                test_chunk(deleted, 5, "delete-me"),
                test_embedding(0.0, 1.0),
            ),
        ])?;
        store.upsert_chunk_embeddings(&[(rewritten_chunk, test_embedding(0.0, 1.0))])?;
        assert_eq!(active_counts(&store)?, (2, 2));
        let rewritten_hit = exact_search(&store, &test_embedding(1.0, 0.0), 2)?
            .hits
            .into_iter()
            .find(|hit| hit.event_id == rewritten)
            .expect("rewritten event");
        assert_eq!(rewritten_hit.similarity, 0.0);
        assert_eq!(rewritten_hit.source_text_hash, rewritten_hash);
        assert_eq!(store.delete_events(&[deleted])?, 1);
    }

    let store = SemanticVectorStore::open(&root)?;
    assert_eq!(active_counts(&store)?, (1, 1));
    let hits = exact_search(&store, &test_embedding(1.0, 0.0), 10)?.hits;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].event_id, rewritten);
    assert_eq!(hits[0].similarity, 0.0);
    assert_eq!(hits[0].source_text_hash, rewritten_hash);
    Ok(())
}
