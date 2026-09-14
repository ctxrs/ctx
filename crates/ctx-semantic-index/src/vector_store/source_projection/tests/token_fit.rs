use super::*;

struct BudgetEmbedder {
    body_limit: usize,
    marker: MarkerEmbedder,
    batches: Vec<usize>,
}

impl SemanticBatchEmbedder for BudgetEmbedder {
    fn document_fits(&mut self, input: &str) -> Result<bool> {
        let body = input.split_once("\n\n").map_or(input, |(_, body)| body);
        Ok(body.chars().count() <= self.body_limit)
    }

    fn embed_chunks(&mut self, chunks: &[SemanticChunkDocument]) -> Result<Vec<Vec<f32>>> {
        self.batches.push(chunks.len());
        self.marker.embed_chunks(chunks)
    }
}

#[test]
fn token_fit_failure_does_not_publish_or_acknowledge_and_resumes_after_restart() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let index = fixture.publish("fit-failure", &[(0, vec!["世界".repeat(800)])])?;
    let core_generation = index.generation_id().to_owned();
    let mut store = open_store(&fixture.semantic_path)?;
    let baseline = store
        .flat
        .active_publication_token()
        .map_err(anyhow::Error::new)?;
    let mut embedder = BudgetEmbedder {
        body_limit: 0,
        marker: MarkerEmbedder::default(),
        batches: Vec::new(),
    };
    let mut sequences = Vec::new();
    let error = store
        .reconcile_source_backed_index_with_checkpoint_and_progress(
            &index,
            &mut CoreBuilder::default(),
            &mut embedder,
            &mut || Ok(()),
            &mut |sequence| {
                sequences.push(sequence);
                Ok(())
            },
        )
        .unwrap_err();
    assert_eq!(
        crate::semantic_vector_failure_kind(&error),
        Some(crate::SemanticVectorFailureKind::Unavailable)
    );
    assert!(sequences.is_empty());
    assert_eq!(embedder.marker.calls, 0);
    assert!(store.source_acknowledgement()?.is_none());
    assert_eq!(
        store
            .flat
            .active_publication_token()
            .map_err(anyhow::Error::new)?,
        baseline
    );
    let frontier = store.source_frontier()?.expect("retry frontier");
    assert_eq!(frontier.processed_source_documents, 0);
    assert!(frontier.after_identity.is_none());
    drop(store);
    let mut store = open_store(&fixture.semantic_path)?;
    embedder.body_limit = 700;
    let outcome = reconcile_all(
        &mut store,
        &index,
        &mut CoreBuilder::default(),
        &mut embedder,
    )?;
    assert!(outcome.ready());
    assert_eq!(outcome.records_embedded, 1);
    assert!(store.source_acknowledgement()?.is_some());
    assert_eq!(index.generation_id(), core_generation);
    Ok(())
}

#[test]
fn first_document_can_exceed_page_budget_once_and_complete_without_skipping() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let index = fixture.publish(
        "fit-first-document",
        &[(0, vec!["x".repeat(1800), "small".to_owned()])],
    )?;
    let mut store = open_store(&fixture.semantic_path)?;
    let mut embedder = BudgetEmbedder {
        body_limit: 4,
        marker: MarkerEmbedder::default(),
        batches: Vec::new(),
    };
    let outcome = reconcile_all(
        &mut store,
        &index,
        &mut CoreBuilder::default(),
        &mut embedder,
    )?;
    assert!(outcome.ready());
    assert_eq!(outcome.records_embedded, 2);
    assert_eq!(active_events(&store)?, 2);
    // A single admitted document may exceed512, but never the1024 document cap.
    // Embedding calls are independently split to the page bound.
    assert!(embedder.marker.chunks > 512 && embedder.marker.chunks <= 1028);
    assert!(embedder.batches.iter().all(|n| *n <= 512));
    assert!(store.source_acknowledgement()?.is_some());
    Ok(())
}

#[test]
fn chunking_revision_rebuilds_vectors_with_core_generation_unchanged() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let index = fixture.publish("fit-policy", &[(0, bodies("stable", 2))])?;
    let core_generation = index.generation_id().to_owned();
    let mut store = open_store(&fixture.semantic_path)?;
    let mut old_policy = current_semantic_generation_policy();
    assert_eq!(old_policy.chunking_revision, 2);
    old_policy.chunking_revision = 1;
    let old = SourceBackedSemanticGeneration::from_verified_index_with_policy(
        &index,
        old_policy,
        semantic_model_contract(),
    )?;
    reconcile_generation(
        &mut store,
        &index,
        &old,
        &mut CoreBuilder::default(),
        &mut MarkerEmbedder::default(),
    )?;
    let old_fingerprint = store
        .source_acknowledgement()?
        .expect("old receipt")
        .semantic_policy_fingerprint;
    let rebuilt = reconcile_all(
        &mut store,
        &index,
        &mut CoreBuilder::default(),
        &mut MarkerEmbedder::default(),
    )?;
    assert!(rebuilt.ready());
    assert_eq!(rebuilt.records_embedded, 2);
    assert_eq!(rebuilt.records_reused, 0);
    assert_ne!(
        store
            .source_acknowledgement()?
            .expect("fitted receipt")
            .semantic_policy_fingerprint,
        old_fingerprint
    );
    assert_eq!(index.generation_id(), core_generation);
    Ok(())
}

#[test]
fn legacy_http_receipt_cannot_certify_fitted_builtin_spans() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let index = fixture.publish("fit-legacy-route", &[(0, vec!["世界".repeat(800)])])?;
    let legacy = legacy_fixed_http_semantic_model_contract("http://127.0.0.1:43123")?;
    let legacy_policy = semantic_generation_policy(&legacy);
    assert_eq!(legacy_policy.chunking_revision, 1);
    assert_eq!(
        legacy_policy.canonical_sha256()?,
        "e8d31418a1da20200d75580348b8b2e7ee4f97c58f34a46900fc6d87daa83ccf"
    );
    let mut store = SemanticVectorStore::open(&fixture.semantic_path, &legacy)?;
    reconcile_all(
        &mut store,
        &index,
        &mut CoreBuilder::default(),
        &mut MarkerEmbedder::default(),
    )?;
    let legacy_fingerprint = store
        .source_acknowledgement()?
        .expect("HTTP receipt")
        .semantic_policy_fingerprint;
    drop(store);
    let mut store = open_store(&fixture.semantic_path)?;
    let mut embedder = BudgetEmbedder {
        body_limit: 700,
        marker: MarkerEmbedder::default(),
        batches: Vec::new(),
    };
    let outcome = reconcile_all(
        &mut store,
        &index,
        &mut CoreBuilder::default(),
        &mut embedder,
    )?;
    assert_eq!(outcome.records_reused, 0);
    assert_eq!(outcome.records_embedded, 1);
    assert!(
        embedder.marker.chunks > 2,
        "the old acknowledged receipt must not bypass fitting"
    );
    assert_ne!(
        store
            .source_acknowledgement()?
            .expect("built-in receipt")
            .semantic_policy_fingerprint,
        legacy_fingerprint
    );
    Ok(())
}

#[test]
fn sequence_only_core_change_updates_authority_without_embedding() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let initial = fixture.publish_with_event_sequences(
        "sequence-a",
        &[(0, vec![(1, "same semantic body".to_owned())])],
    )?;
    let target = fixture.publish_with_event_sequences(
        "sequence-b",
        &[(0, vec![(91, "same semantic body".to_owned())])],
    )?;
    let mut store = SemanticVectorStore::open(&fixture.semantic_path, semantic_model_contract())?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    reconcile_all(&mut store, &initial, &mut builder, &mut embedder)?;
    let embedded_before = embedder.chunks;
    let fits_before = embedder.fit_calls;

    let outcome = reconcile_all(&mut store, &target, &mut builder, &mut embedder)?;
    assert_eq!(outcome.records_decoded, 1);
    assert_eq!(outcome.records_reused, 1);
    assert_eq!(outcome.records_embedded, 0);
    assert_eq!(outcome.vectors_touched, 0);
    assert_eq!(outcome.vector_bytes_touched, 0);
    assert!(outcome.metadata_records_touched < 32);
    assert_eq!(embedder.chunks, embedded_before);
    assert_eq!(
        embedder.fit_calls, fits_before,
        "sequence-only reuse must not require a model"
    );
    assert!(matches!(
        store.source_backed_generation_pin_exact(initial.generation_id(), 1)?,
        SourceBackedGenerationPin::NotReady
    ));
    let pin = match store.source_backed_generation_pin_exact(target.generation_id(), 1)? {
        SourceBackedGenerationPin::Ready(pin) => pin,
        SourceBackedGenerationPin::NotReady | SourceBackedGenerationPin::ReadyEmpty => {
            return Err(anyhow!("sequence-only target did not produce an exact pin"));
        }
    };
    let chunk = pin
        .scan_segments()
        .iter()
        .flat_map(|segment| segment.chunks())
        .next()
        .ok_or_else(|| anyhow!("sequence-only target lost its vector"))?;
    assert_eq!(chunk.seq, 91);
    Ok(())
}
