use super::super::{
    vector_store_schema::{
        semantic_vector_failure_kind, SemanticVectorFailureKind, SEMANTIC_VECTOR_BACKEND_FLAT_F32,
        SEMANTIC_VECTOR_SCHEMA_VERSION,
    },
    vector_store_search::{scan_exact_generation, SEMANTIC_EXACT_TOP_K_MAX},
};
use super::*;

fn test_event_identity_digest(event_id: Uuid) -> Option<[u8; 32]> {
    let mut digest = [0; 32];
    digest[..16].copy_from_slice(event_id.as_bytes());
    digest[16..].copy_from_slice(event_id.as_bytes());
    Some(digest)
}

fn exact_search(
    store: &SemanticVectorStore,
    query_embedding: &[f32],
    limit: usize,
) -> Result<SemanticVectorSearch> {
    let pinned = store
        .flat_pin_generation()?
        .expect("fixture must publish a flat generation");
    let query_embeddings = vec![query_embedding.to_vec()];
    scan_exact_generation(
        &pinned,
        &query_embeddings,
        limit,
        &test_event_identity_digest,
        std::time::Instant::now(),
    )
}

fn exact_search_multi(
    store: &SemanticVectorStore,
    query_embeddings: &[Vec<f32>],
    limit: usize,
) -> Result<SemanticVectorSearch> {
    let pinned = store
        .flat_pin_generation()?
        .expect("fixture must publish a flat generation");
    scan_exact_generation(
        &pinned,
        query_embeddings,
        limit,
        &test_event_identity_digest,
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
    let contract = test_contract();
    assert_eq!(root, temp.path().join("search").join("semantic"));
    let store = SemanticVectorStore::open(&root, &contract)?;

    assert_eq!(store.contract(), &contract);
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
    assert!(!schema.contains("semantic_source_documents"));
    assert!(!schema.contains("locator_json"));
    assert!(!schema.contains("embedding_f32"));
    assert!(!schema.contains("event_embedding"));
    assert!(!schema.contains("chunk_text"));
    assert!(!schema.contains("USING vec"));
    let read_only = SemanticVectorStore::open_read_only(&root, &contract)?
        .expect("created semantic store must reopen read-only");
    assert_eq!(read_only.contract(), &contract);
    Ok(())
}

#[test]
fn passive_snapshot_open_leaves_the_durable_tree_unchanged() -> Result<()> {
    fn snapshot(root: &Path) -> Result<Vec<(std::path::PathBuf, Vec<u8>)>> {
        fn visit(
            root: &Path,
            path: &Path,
            files: &mut Vec<(std::path::PathBuf, Vec<u8>)>,
        ) -> Result<()> {
            if path.is_file() {
                files.push((path.strip_prefix(root)?.to_path_buf(), std::fs::read(path)?));
                return Ok(());
            }
            for entry in std::fs::read_dir(path)? {
                visit(root, &entry?.path(), files)?;
            }
            Ok(())
        }
        let mut files = Vec::new();
        visit(root, root, &mut files)?;
        files.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(files)
    }

    let temporary = tempfile::tempdir()?;
    let root = source_backed_semantic_vector_path(temporary.path());
    let contract = test_contract();
    let store = SemanticVectorStore::open(&root, &contract)?;
    drop(store);
    let before = snapshot(&root)?;

    let passive = SemanticVectorStore::open_passive_snapshot(&root, &contract)?;

    assert!(passive.is_some());
    assert_eq!(snapshot(&root)?, before);
    assert!(!root.join("state.sqlite-wal").exists());
    assert!(!root.join("state.sqlite-shm").exists());
    Ok(())
}

#[test]
fn passive_snapshot_refuses_a_live_wal_without_touching_it() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = source_backed_semantic_vector_path(temporary.path());
    let contract = test_contract();
    let store = SemanticVectorStore::open(&root, &contract)?;
    store
        .conn
        .execute("UPDATE semantic_index_stats SET dirty_items = 0", [])?;
    let wal = root.join("state.sqlite-wal");
    assert!(wal.exists(), "fixture must retain live WAL state");
    let before = std::fs::read(&wal)?;

    let error = SemanticVectorStore::open_passive_snapshot(&root, &contract)
        .expect_err("a live WAL cannot be read through an immutable passive snapshot");

    assert_eq!(
        semantic_vector_failure_kind(&error),
        Some(SemanticVectorFailureKind::PassiveSnapshotUnavailable)
    );
    assert_eq!(std::fs::read(&wal)?, before);
    drop(store);
    Ok(())
}

#[test]
fn flat_search_is_unique_deterministic_bounded_and_exact() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let contract = test_contract();
    let mut store =
        SemanticVectorStore::open(&source_backed_semantic_vector_path(temp.path()), &contract)?;
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let first_chunk = test_chunk_at(first, 2, "first", 0, 2);
    let first_hash = first_chunk.source_text_hash.clone();
    store.publish_chunk_replacements(
        &[
            (first_chunk, test_embedding(&contract, 1.0, 0.0)),
            (
                test_chunk_at(first, 2, "first", 1, 2),
                test_embedding(&contract, 0.8, 0.6),
            ),
            (
                test_chunk(second, 1, "second"),
                test_embedding(&contract, 1.0, 0.0),
            ),
        ],
        &[],
    )?;

    let search = exact_search(&store, &test_embedding(&contract, 1.0, 0.0), 2)?;
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
        &test_embedding(&contract, 1.0, 0.0),
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
fn thirty_two_query_vectors_touch_each_flat_chunk_once() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let contract = test_contract();
    let mut store =
        SemanticVectorStore::open(&source_backed_semantic_vector_path(temp.path()), &contract)?;
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    store.publish_chunk_replacements(
        &[
            (
                test_chunk_at(first, 1, "first", 0, 2),
                test_embedding(&contract, 1.0, 0.0),
            ),
            (
                test_chunk_at(first, 1, "first", 1, 2),
                test_embedding(&contract, 0.8, 0.6),
            ),
            (
                test_chunk(second, 2, "second"),
                test_embedding(&contract, 0.0, 1.0),
            ),
        ],
        &[],
    )?;
    let queries = (0..32)
        .map(|index| {
            if index % 2 == 0 {
                test_embedding(&contract, 1.0, 0.0)
            } else {
                test_embedding(&contract, 0.0, 1.0)
            }
        })
        .collect::<Vec<_>>();

    let search = exact_search_multi(&store, &queries, 2)?;

    assert_eq!(search.stats.query_vectors, 32);
    assert_eq!(search.stats.vector_passes, 1);
    assert_eq!(search.stats.chunks_scanned, 3);
    assert_eq!(search.stats.events_scored, 2);
    assert_eq!(search.stats.dot_products, 96);
    assert_eq!(
        search.stats.vector_bytes_read,
        3 * contract.dimensions() * std::mem::size_of::<f32>()
    );
    assert_eq!(
        search
            .hits
            .iter()
            .find(|hit| hit.event_id == first)
            .expect("first event")
            .query_ordinal,
        0
    );
    assert_eq!(
        search
            .hits
            .iter()
            .find(|hit| hit.event_id == second)
            .expect("second event")
            .query_ordinal,
        1
    );
    Ok(())
}

#[test]
fn flat_rewrite_truncation_delete_and_restart_do_not_resurrect_chunks() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = source_backed_semantic_vector_path(temp.path());
    let contract = test_contract();
    let rewritten = Uuid::new_v4();
    let deleted = Uuid::new_v4();
    let rewritten_chunk = test_chunk(rewritten, 4, "rewrite-new");
    let rewritten_hash = rewritten_chunk.source_text_hash.clone();
    {
        let mut store = SemanticVectorStore::open(&root, &contract)?;
        store.publish_chunk_replacements(
            &[
                (
                    test_chunk_at(rewritten, 4, "rewrite-old", 0, 2),
                    test_embedding(&contract, 1.0, 0.0),
                ),
                (
                    test_chunk_at(rewritten, 4, "rewrite-old", 1, 2),
                    test_embedding(&contract, 1.0, 0.0),
                ),
                (
                    test_chunk(deleted, 5, "delete-me"),
                    test_embedding(&contract, 0.0, 1.0),
                ),
            ],
            &[],
        )?;
        store.publish_chunk_replacements(
            &[(rewritten_chunk, test_embedding(&contract, 0.0, 1.0))],
            &[],
        )?;
        assert_eq!(active_counts(&store)?, (2, 2));
        let rewritten_hit = exact_search(&store, &test_embedding(&contract, 1.0, 0.0), 2)?
            .hits
            .into_iter()
            .find(|hit| hit.event_id == rewritten)
            .expect("rewritten event");
        assert_eq!(rewritten_hit.similarity, 0.0);
        assert_eq!(rewritten_hit.source_text_hash, rewritten_hash);
        assert_eq!(store.delete_events(&[deleted])?, 1);
    }

    let store = SemanticVectorStore::open(&root, &contract)?;
    assert_eq!(active_counts(&store)?, (1, 1));
    let hits = exact_search(&store, &test_embedding(&contract, 1.0, 0.0), 10)?.hits;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].event_id, rewritten);
    assert_eq!(hits[0].similarity, 0.0);
    assert_eq!(hits[0].source_text_hash, rewritten_hash);
    Ok(())
}

#[test]
fn future_control_schema_prevents_writable_flat_recovery_mutation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = source_backed_semantic_vector_path(temp.path());
    let contract = test_contract();
    let mut store = SemanticVectorStore::open(&root, &contract)?;
    store.publish_chunk_replacements(
        &[(
            test_chunk(Uuid::new_v4(), 1, "future-schema"),
            test_embedding(&contract, 1.0, 0.0),
        )],
        &[],
    )?;
    drop(store);

    let recovery_artifact = root
        .join("flat_segments")
        .join(".flat-tmp-future-control-schema");
    fs::write(&recovery_artifact, b"must survive rejected writable open")?;
    let control = rusqlite::Connection::open(root.join("state.sqlite"))?;
    control.pragma_update(None, "user_version", SEMANTIC_VECTOR_SCHEMA_VERSION + 1)?;
    drop(control);

    let error = SemanticVectorStore::open(&root, &contract)
        .err()
        .expect("a future control schema must reject writable open");
    assert_eq!(
        semantic_vector_failure_kind(&error),
        Some(SemanticVectorFailureKind::NewerSchema)
    );
    assert_eq!(
        fs::read(recovery_artifact)?,
        b"must survive rejected writable open"
    );
    Ok(())
}
