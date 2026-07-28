use super::super::{
    query_service::semantic_candidate_limit,
    runtime_limits::SEMANTIC_EXACT_TOP_K_MAX,
    vector_store_schema::{
        semantic_vector_failure_kind, SemanticVectorFailureKind, SEMANTIC_VECTOR_BACKEND_FLAT_F32,
        SEMANTIC_VECTOR_SCHEMA_VERSION,
    },
};
use super::*;

#[test]
fn flat_store_control_catalog_has_no_vectors_or_plaintext() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("semantic-vectors");
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
    assert!(schema.contains("locator_json"));
    assert!(!schema.contains("embedding_f32"));
    assert!(!schema.contains("event_embedding"));
    assert!(!schema.contains("chunk_text"));
    assert!(!schema.contains("USING vec"));
    assert_eq!(store.plaintext_value_count()?, 0);
    Ok(())
}

#[test]
fn flat_search_is_unique_deterministic_bounded_and_exact() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut store = SemanticVectorStore::open(&temp.path().join("semantic-vectors"))?;
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
    let error = store
        .search(&test_embedding(1.0, 0.0), SEMANTIC_EXACT_TOP_K_MAX + 1)
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
    let root = temp.path().join("semantic-vectors");
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
        assert_eq!(
            store.cached_or_exact_stats()?,
            SemanticSidecarStats {
                embedded_items: 2,
                embedded_chunks: 2,
            }
        );
        let rewritten_hit = store
            .search(&test_embedding(1.0, 0.0), 2)?
            .hits
            .into_iter()
            .find(|hit| hit.event_id == rewritten)
            .expect("rewritten event");
        assert_eq!(rewritten_hit.similarity, 0.0);
        assert_eq!(rewritten_hit.source_text_hash, rewritten_hash);
        assert_eq!(store.delete_embedding_chunks_for_event_ids(&[deleted])?, 1);
    }

    let store = SemanticVectorStore::open(&root)?;
    assert_eq!(
        store.cached_or_exact_stats()?,
        SemanticSidecarStats {
            embedded_items: 1,
            embedded_chunks: 1,
        }
    );
    let hits = store.search(&test_embedding(1.0, 0.0), 10)?.hits;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].event_id, rewritten);
    assert_eq!(hits[0].similarity, 0.0);
    assert_eq!(hits[0].source_text_hash, rewritten_hash);
    Ok(())
}

#[test]
fn semantic_candidate_overfetch_is_clamped_for_exact_scan() {
    for (user_limit, expected_candidate_limit) in [
        (40, 4_000),
        (41, SEMANTIC_EXACT_TOP_K_MAX),
        (200, SEMANTIC_EXACT_TOP_K_MAX),
    ] {
        let options = ctx_history_search::PacketOptions {
            limit: user_limit,
            ..ctx_history_search::PacketOptions::default()
        };
        assert_eq!(semantic_candidate_limit(&options), expected_candidate_limit);
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
