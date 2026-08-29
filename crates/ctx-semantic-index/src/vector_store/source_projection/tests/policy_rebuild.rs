use super::*;

#[test]
fn high_odd_dimension_external_projection_preserves_full_ordinary_batches() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let contract = external_contract(
        "http://127.0.0.1:43122/v1/embeddings",
        "space-high-odd",
        4_095,
    )?;
    let page_limit = contract
        .external_space()
        .ok_or_else(|| anyhow!("external fixture lost its declared space"))?
        .max_inputs_per_request();
    assert_eq!(page_limit, 64);
    assert_eq!(source_event_page_limit(&contract), page_limit);
    assert_eq!(
        source_event_page_limit(semantic_model_contract()),
        MAX_SOURCE_EVENT_PAGE_ITEMS
    );
    let record_count = page_limit + 1;
    let index = fixture.publish(
        "external-high-dimension-pages",
        &[(0, bodies("high-dimension", record_count))],
    )?;
    let core_generation_id = index.generation_id().to_owned();
    let mut store = SemanticVectorStore::open(&fixture.semantic_path, &contract)?;
    let mut embedder = DimensionEmbedder::new(&contract);

    let outcome = reconcile_all(
        &mut store,
        &index,
        &mut CoreBuilder::default(),
        &mut embedder,
    )?;

    assert_eq!(outcome.records_decoded, record_count);
    assert_eq!(outcome.records_embedded, record_count);
    assert_eq!(embedder.batch_sizes, vec![page_limit, 1]);
    assert!(embedder
        .batch_sizes
        .iter()
        .all(|batch| *batch <= page_limit));
    assert_eq!(index.generation_id(), core_generation_id);
    assert_eq!(
        VerifiedIndex::open_pinned(
            fixture
                .data_root
                .join("index-external-high-dimension-pages"),
        )?
        .generation_id(),
        core_generation_id
    );
    Ok(())
}

#[test]
fn single_max_length_external_record_bounds_batches_at_max_dimensions() -> Result<()> {
    const EXTERNAL_SCALAR_LIMIT: usize = 262_144;

    for (dimensions, port) in [(4_095, 43_125), (4_096, 43_126)] {
        let fixture = Fixture::new(1)?;
        let contract = external_contract(
            &format!("http://127.0.0.1:{port}/v1/embeddings"),
            &format!("space-max-record-{dimensions}"),
            dimensions,
        )?;
        let batch_limit = source_event_page_limit(&contract);
        assert_eq!(batch_limit, 64);
        let index = fixture.publish(
            &format!("external-max-record-{dimensions}"),
            &[(
                0,
                vec!["x".repeat(ctx_history_index::SEMANTIC_SOURCE_MAX_CHARS)],
            )],
        )?;
        let mut store = SemanticVectorStore::open(&fixture.semantic_path, &contract)?;
        let mut embedder = DimensionEmbedder::new(&contract);

        let outcome = reconcile_all(
            &mut store,
            &index,
            &mut CoreBuilder::default(),
            &mut embedder,
        )?;

        assert!(outcome.ready);
        assert_eq!(outcome.records_decoded, 1);
        assert_eq!(outcome.records_embedded, 1);
        assert!(embedder.chunks > batch_limit);
        assert!(embedder.batch_sizes.len() > 1);
        assert!(embedder.batch_sizes.iter().all(|batch| {
            *batch <= batch_limit
                && batch
                    .checked_mul(dimensions)
                    .is_some_and(|scalars| scalars <= EXTERNAL_SCALAR_LIMIT)
        }));
        let snapshot = projection_snapshot(&store)?;
        assert_eq!(snapshot.events.len(), 1);
        assert_eq!(snapshot.events[0].0, fixture.event_id(0, 1)?);
        assert_eq!(snapshot.events[0].3 as usize, embedder.chunks);
        assert_eq!(snapshot.chunks.len(), embedder.chunks);
    }
    Ok(())
}

struct InterruptingDimensionEmbedder {
    inner: DimensionEmbedder,
    fail_on_call: usize,
    calls: usize,
    requested_batch_sizes: Vec<usize>,
}

impl InterruptingDimensionEmbedder {
    fn new(contract: &SemanticModelContract, fail_on_call: usize) -> Self {
        Self {
            inner: DimensionEmbedder::new(contract),
            fail_on_call,
            calls: 0,
            requested_batch_sizes: Vec::new(),
        }
    }
}

impl SemanticBatchEmbedder for InterruptingDimensionEmbedder {
    fn embed_chunks(&mut self, chunks: &[SemanticChunkDocument]) -> Result<Vec<Vec<f32>>> {
        self.calls = self.calls.saturating_add(1);
        self.requested_batch_sizes.push(chunks.len());
        if self.calls == self.fail_on_call {
            return Err(anyhow!("forced external embedding interruption"));
        }
        self.inner.embed_chunks(chunks)
    }
}

#[test]
fn max_length_external_records_resume_atomically_without_skips() -> Result<()> {
    const DIMENSIONS: usize = 4_096;
    const EXTERNAL_SCALAR_LIMIT: usize = 262_144;
    const RECORD_COUNT: usize = 3;

    let fixture = Fixture::new(1)?;
    let contract = external_contract(
        "http://127.0.0.1:43127/v1/embeddings",
        "space-max-record-restart",
        DIMENSIONS,
    )?;
    let batch_limit = source_event_page_limit(&contract);
    let index = fixture.publish(
        "external-max-record-restart",
        &[(
            0,
            vec!["x".repeat(ctx_history_index::SEMANTIC_SOURCE_MAX_CHARS); RECORD_COUNT],
        )],
    )?;
    let expected_event_ids = (1..=RECORD_COUNT)
        .map(|sequence| fixture.event_id(0, u64::try_from(sequence)?))
        .collect::<Result<HashSet<_>>>()?;
    let mut store = SemanticVectorStore::open(&fixture.semantic_path, &contract)?;
    let mut builder = CoreBuilder::default();
    // One oversized record completes in two bounded batches. The fourth call
    // interrupts the second record after its first batch has returned.
    let mut interrupted = InterruptingDimensionEmbedder::new(&contract, 4);
    let error = store
        .reconcile_source_backed_index(&index, &mut builder, &mut interrupted)
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("forced external embedding interruption"));
    assert_eq!(interrupted.requested_batch_sizes.len(), 4);
    assert!(interrupted.requested_batch_sizes.iter().all(|batch| {
        *batch <= batch_limit
            && batch
                .checked_mul(DIMENSIONS)
                .is_some_and(|scalars| scalars <= EXTERNAL_SCALAR_LIMIT)
    }));
    let frontier = store
        .source_frontier()?
        .ok_or_else(|| anyhow!("interrupted external rebuild lost its frontier"))?;
    assert_eq!(frontier.processed_source_documents, 1);
    assert!(!frontier.source_scan_complete);
    let committed_event_id = frontier
        .after_identity
        .as_deref()
        .map(StableEntityId::decode_canonical)
        .transpose()?
        .ok_or_else(|| anyhow!("interrupted external rebuild lost committed progress"))?
        .as_uuid();
    assert!(expected_event_ids.contains(&committed_event_id));
    assert!(store.source_acknowledgement()?.is_none());

    drop(store);
    let mut store = SemanticVectorStore::open(&fixture.semantic_path, &contract)?;
    builder.calls.clear();
    let mut resumed_embedder = DimensionEmbedder::new(&contract);
    let resumed = reconcile_all(&mut store, &index, &mut builder, &mut resumed_embedder)?;

    assert!(resumed.ready);
    assert_eq!(resumed.records_embedded, RECORD_COUNT - 1);
    assert!(builder
        .calls
        .iter()
        .all(|event_id| *event_id != committed_event_id));
    assert!(resumed_embedder.batch_sizes.iter().all(|batch| {
        *batch <= batch_limit
            && batch
                .checked_mul(DIMENSIONS)
                .is_some_and(|scalars| scalars <= EXTERNAL_SCALAR_LIMIT)
    }));
    let snapshot = projection_snapshot(&store)?;
    assert_eq!(snapshot.events.len(), RECORD_COUNT);
    assert_eq!(
        snapshot
            .events
            .iter()
            .map(|event| event.0)
            .collect::<HashSet<_>>(),
        expected_event_ids
    );
    assert!(snapshot
        .events
        .iter()
        .all(|event| event.3 as usize > batch_limit));
    assert_eq!(
        snapshot.chunks.len(),
        snapshot
            .events
            .iter()
            .map(|event| event.3 as usize)
            .sum::<usize>()
    );
    assert!(store.source_acknowledgement()?.is_some());
    Ok(())
}

#[test]
fn same_dimension_external_space_change_resets_and_reembeds_unchanged_core() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let index = fixture.publish("external-space-reset", &[(0, bodies("space", 3))])?;
    let core_generation_id = index.generation_id().to_owned();
    let endpoint = "http://127.0.0.1:43123/v1/embeddings";
    let first_contract = external_contract(endpoint, "space-a", 6)?;
    let second_contract = external_contract(endpoint, "space-b", 6)?;
    let moved_contract =
        external_contract("http://127.0.0.1:43123/other/embeddings", "space-a", 6)?;
    assert_eq!(first_contract.dimensions(), second_contract.dimensions());
    assert_ne!(first_contract, second_contract);
    let first_policy = semantic_generation_policy(&first_contract);
    let second_policy = semantic_generation_policy(&second_contract);
    assert_eq!(first_policy.embedding.model, first_contract.model_id());
    assert_eq!(first_policy.embedding.model_revision, "space-a");
    assert_eq!(first_policy.embedding.dimensions, 6);
    assert_eq!(second_policy.embedding.model, second_contract.model_id());
    assert_eq!(second_policy.embedding.model_revision, "space-b");
    assert_eq!(second_policy.embedding.dimensions, 6);
    assert_ne!(
        semantic_generation_policy_hash(&first_contract)?,
        semantic_generation_policy_hash(&second_contract)?
    );
    assert_ne!(
        source_backed_semantic_contract_fingerprint(&first_contract)?,
        source_backed_semantic_contract_fingerprint(&second_contract)?
    );
    assert_ne!(
        crate::vector_store_schema::flat_model_contract(&first_contract)
            .map_err(anyhow::Error::new)?,
        crate::vector_store_schema::flat_model_contract(&second_contract)
            .map_err(anyhow::Error::new)?
    );
    assert_eq!(
        semantic_generation_policy_hash(&first_contract)?,
        semantic_generation_policy_hash(&moved_contract)?,
        "executor location must not change vector compatibility"
    );
    assert_eq!(
        source_backed_semantic_contract_fingerprint(&first_contract)?,
        source_backed_semantic_contract_fingerprint(&moved_contract)?,
        "executor location must not change source projection identity"
    );
    assert_eq!(
        crate::vector_store_schema::flat_model_contract(&first_contract)
            .map_err(anyhow::Error::new)?,
        crate::vector_store_schema::flat_model_contract(&moved_contract)
            .map_err(anyhow::Error::new)?,
        "executor location must not reset compatible Flat vectors"
    );

    let mut store = SemanticVectorStore::open(&fixture.semantic_path, &first_contract)?;
    let mut first_embedder = DimensionEmbedder::new(&first_contract);
    let initial = reconcile_all(
        &mut store,
        &index,
        &mut CoreBuilder::default(),
        &mut first_embedder,
    )?;
    assert_eq!(initial.records_embedded, 3);
    assert_eq!(initial.records_reused, 0);
    assert_eq!(first_embedder.chunks, 3);
    let first_acknowledgement = store
        .source_acknowledgement()?
        .ok_or_else(|| anyhow!("missing first external-space acknowledgement"))?;
    drop(store);

    let mut store = SemanticVectorStore::open(&fixture.semantic_path, &second_contract)?;
    assert!(store.conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM semantic_maintenance_state WHERE key = ?1
         )",
        [FULL_REBUILD_STATE],
        |row| row.get::<_, bool>(0),
    )?);
    assert!(store.source_acknowledgement()?.is_none());
    let mut builder = CoreBuilder::default();
    let mut second_embedder = DimensionEmbedder::new(&second_contract);
    let rebuilt = reconcile_all(&mut store, &index, &mut builder, &mut second_embedder)?;
    assert_eq!(rebuilt.records_embedded, 3);
    assert_eq!(rebuilt.records_reused, 0);
    assert_eq!(builder.calls.len(), 3);
    assert_eq!(second_embedder.chunks, 3);
    let second_acknowledgement = store
        .source_acknowledgement()?
        .ok_or_else(|| anyhow!("missing second external-space acknowledgement"))?;
    assert_ne!(
        first_acknowledgement.semantic_policy_fingerprint,
        second_acknowledgement.semantic_policy_fingerprint
    );
    assert_ne!(
        first_acknowledgement.contract_fingerprint,
        second_acknowledgement.contract_fingerprint
    );
    assert_eq!(index.generation_id(), core_generation_id);
    assert_eq!(
        VerifiedIndex::open_pinned(fixture.data_root.join("index-external-space-reset"))?
            .generation_id(),
        core_generation_id
    );
    Ok(())
}

#[test]
fn odd_dimension_external_change_resets_and_rebuilds_without_affecting_core() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let index = fixture.publish("external-dimension-reset", &[(0, bodies("dimension", 2))])?;
    let core_generation_id = index.generation_id().to_owned();
    let endpoint = "http://127.0.0.1:43124/v1/embeddings";
    let initial_contract = external_contract(endpoint, "space-even", 6)?;
    let odd_contract = external_contract(endpoint, "space-odd", 7)?;

    let mut store = SemanticVectorStore::open(&fixture.semantic_path, &initial_contract)?;
    reconcile_all(
        &mut store,
        &index,
        &mut CoreBuilder::default(),
        &mut DimensionEmbedder::new(&initial_contract),
    )?;
    drop(store);

    let mut store = SemanticVectorStore::open(&fixture.semantic_path, &odd_contract)?;
    assert!(store.conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM semantic_maintenance_state WHERE key = ?1
         )",
        [FULL_REBUILD_STATE],
        |row| row.get::<_, bool>(0),
    )?);
    let mut builder = CoreBuilder::default();
    let mut embedder = DimensionEmbedder::new(&odd_contract);
    let rebuilt = reconcile_all(&mut store, &index, &mut builder, &mut embedder)?;
    assert_eq!(rebuilt.records_embedded, 2);
    assert_eq!(rebuilt.records_reused, 0);
    assert_eq!(builder.calls.len(), 2);
    assert_eq!(embedder.chunks, 2);
    let pinned = store
        .flat_pin_generation()?
        .ok_or_else(|| anyhow!("odd-dimension rebuild lost its Flat generation"))?;
    assert_eq!(pinned.model_contract().dimensions, 7);
    assert_eq!(pinned.stats().active_events, 2);
    assert_eq!(index.generation_id(), core_generation_id);
    assert_eq!(
        VerifiedIndex::open_pinned(fixture.data_root.join("index-external-dimension-reset"))?
            .generation_id(),
        core_generation_id
    );
    Ok(())
}

#[test]
fn flat_contract_reset_survives_both_control_handoff_crash_windows() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let index = fixture.publish("flat-contract-reset", &[(0, bodies("first", 3))])?;
    let contract = semantic_model_contract();
    let mut store = SemanticVectorStore::open(&fixture.semantic_path, contract)?;
    reconcile_all(
        &mut store,
        &index,
        &mut CoreBuilder::default(),
        &mut MarkerEmbedder::default(),
    )?;
    assert!(store.source_acknowledgement()?.is_some());
    drop(store);

    let mut changed_flat =
        crate::vector_store_schema::flat_model_contract(contract).map_err(anyhow::Error::new)?;
    changed_flat.model_revision.push_str("-test-only");
    let changed = crate::vector_store::flat_segments::FlatSegmentStore::open(
        &fixture.semantic_path,
        changed_flat,
    )
    .map_err(anyhow::Error::new)?;
    assert!(changed.model_contract_reset_pending()?);
    drop(changed); // Crash after Flat publication and before the control handoff.

    assert!(SemanticVectorStore::open_read_only(&fixture.semantic_path, contract)?.is_none());
    let store = SemanticVectorStore::open(&fixture.semantic_path, contract)?;
    assert!(store.source_acknowledgement()?.is_none());
    assert!(store.source_frontier()?.is_none());
    assert!(!store.flat.model_contract_reset_pending()?);
    drop(store);

    fs::write(
        fixture
            .semantic_path
            .join(crate::vector_store::flat_segments::MODEL_CONTRACT_RESET_PENDING_FILE),
        b"pending\n",
    )?; // Crash after the control commit and before marker acknowledgement.
    let mut store = SemanticVectorStore::open(&fixture.semantic_path, contract)?;
    assert!(store.source_acknowledgement()?.is_none());
    assert!(!store.flat.model_contract_reset_pending()?);

    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    let rebuilt = reconcile_all(&mut store, &index, &mut builder, &mut embedder)?;
    assert_eq!(rebuilt.records_embedded, 3);
    assert!(store.source_acknowledgement()?.is_some());
    assert!(matches!(
        store.source_backed_generation_pin_exact(index.generation_id(), 3)?,
        SourceBackedGenerationPin::Ready(_)
    ));
    Ok(())
}

#[test]
fn descriptor_only_model_change_rebuilds_every_vector_from_unchanged_core() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let index = fixture.publish("revision", &[(0, bodies("first", 130))])?;
    let core_generation_id = index.generation_id().to_owned();
    let mut store = SemanticVectorStore::open(&fixture.semantic_path, semantic_model_contract())?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    reconcile_all(&mut store, &index, &mut builder, &mut embedder)?;
    let model_contract = semantic_model_contract();
    let baseline_generation =
        SourceBackedSemanticGeneration::from_verified_index(&index, model_contract)?;
    let baseline_contract = baseline_generation.contract_fingerprint.clone();
    store.reset_flat_active_event_snapshot_count();

    let descriptor = model_contract.descriptor();
    let revised_descriptor =
        descriptor.replacen("max_sequence_length=512", "max_sequence_length=513", 1);
    assert_ne!(descriptor, revised_descriptor);
    let revised = SourceBackedSemanticGeneration::from_verified_index_with_authority(
        &index,
        current_semantic_generation_policy(),
        revised_descriptor,
    )?;
    assert_ne!(revised.contract_fingerprint, baseline_contract);
    builder.calls.clear();
    let rebuilt = reconcile_generation(&mut store, &index, &revised, &mut builder, &mut embedder)?;
    assert_eq!(rebuilt.records_decoded, 130);
    assert_eq!(rebuilt.records_embedded, 130);
    assert_eq!(rebuilt.records_reused, 0);
    assert_eq!(builder.calls.len(), 130);
    assert_eq!(
        store
            .source_acknowledgement()?
            .expect("descriptor rebuild acknowledgement")
            .contract_fingerprint,
        revised.contract_fingerprint
    );
    assert_eq!(
        store.flat_active_event_snapshot_count(),
        0,
        "policy replacement must remain source-local"
    );
    assert_eq!(index.generation_id(), core_generation_id);
    assert_eq!(
        VerifiedIndex::open_pinned(fixture.data_root.join("index-revision"))?.generation_id(),
        core_generation_id,
        "a semantic-model-only rebuild must leave committed Core active"
    );

    builder.calls.clear();
    let embedded_chunks = embedder.chunks;
    let no_op = reconcile_generation(&mut store, &index, &revised, &mut builder, &mut embedder)?;
    assert_eq!(no_op.records_decoded, 0);
    assert_eq!(no_op.records_embedded, 0);
    assert!(builder.calls.is_empty());
    assert_eq!(embedder.chunks, embedded_chunks);
    Ok(())
}

#[test]
fn bounded_literal_fact_policy_upgrade_reembeds_without_touching_core() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let index = fixture.publish("bounded-facts-upgrade", &[(0, bodies("facts", 3))])?;
    let core_generation_id = index.generation_id().to_owned();
    let model_contract = semantic_model_contract();

    let mut legacy_policy = current_semantic_generation_policy();
    legacy_policy.core_content_filter =
        SemanticCoreContentFilter::PolicySelectedCompleteContentAndLiteralFactsV2;
    let current_policy = current_semantic_generation_policy();
    assert_eq!(
        current_policy.core_content_filter,
        SemanticCoreContentFilter::PolicySelectedCompleteContentAndBoundedLiteralFactsV3
    );
    assert_ne!(
        legacy_policy.canonical_sha256()?,
        current_policy.canonical_sha256()?
    );

    let legacy = SourceBackedSemanticGeneration::from_verified_index_with_policy(
        &index,
        legacy_policy,
        model_contract,
    )?;
    let current = SourceBackedSemanticGeneration::from_verified_index(&index, model_contract)?;
    assert_ne!(
        legacy.semantic_policy_fingerprint,
        current.semantic_policy_fingerprint
    );
    assert_ne!(legacy.contract_fingerprint, current.contract_fingerprint);

    let mut store = SemanticVectorStore::open(&fixture.semantic_path, model_contract)?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    let initial = reconcile_generation(&mut store, &index, &legacy, &mut builder, &mut embedder)?;
    assert_eq!(initial.records_embedded, 3);
    let calls_before_upgrade = embedder.calls;

    builder.calls.clear();
    let rebuilt = reconcile_generation(&mut store, &index, &current, &mut builder, &mut embedder)?;
    assert_eq!(rebuilt.records_embedded, 3);
    assert_eq!(rebuilt.records_reused, 0);
    assert_eq!(builder.calls.len(), 3);
    assert!(embedder.calls > calls_before_upgrade);
    assert_eq!(
        store
            .source_acknowledgement()?
            .ok_or_else(|| anyhow!("missing bounded-facts acknowledgement"))?
            .semantic_policy_fingerprint,
        current.semantic_policy_fingerprint
    );
    assert_eq!(index.generation_id(), core_generation_id);
    assert_eq!(
        VerifiedIndex::open_pinned(fixture.data_root.join("index-bounded-facts-upgrade"))?
            .generation_id(),
        core_generation_id,
        "semantic policy replacement must leave committed Core and lexical state active"
    );
    Ok(())
}

#[test]
fn exact_builtin_legacy_descriptor_migrates_without_reembedding_across_restart() -> Result<()> {
    let fixture = Fixture::new(2)?;
    let index = fixture.publish(
        "legacy-descriptor-migration",
        &[(0, bodies("first", 1)), (1, bodies("second", 1))],
    )?;
    let model_contract = semantic_model_contract();
    let legacy_descriptor = model_contract
        .legacy_builtin_descriptor_alias()
        .ok_or_else(|| anyhow!("exact built-in contract lost its legacy descriptor alias"))?;
    assert_eq!(
        format!("sha256:{:x}", Sha256::digest(legacy_descriptor.as_bytes())),
        "sha256:c812eb325bc5e90e7278b2b8da3933206340c5b5a46fd678be40016e06a89fc3"
    );
    let legacy = SourceBackedSemanticGeneration::from_verified_index_with_authority(
        &index,
        current_semantic_generation_policy(),
        legacy_descriptor.to_owned(),
    )?;
    let current = SourceBackedSemanticGeneration::from_verified_index(&index, model_contract)?;
    assert_ne!(legacy.contract_fingerprint, current.contract_fingerprint);
    assert_eq!(legacy.trusted_legacy_contract_fingerprint, None);
    assert_eq!(
        current.trusted_legacy_contract_fingerprint.as_deref(),
        Some(legacy.contract_fingerprint.as_str())
    );

    let mut store = SemanticVectorStore::open(&fixture.semantic_path, model_contract)?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    let initial = reconcile_generation(&mut store, &index, &legacy, &mut builder, &mut embedder)?;
    assert_eq!(initial.records_embedded, 2);
    let legacy_chunks = projection_snapshot(&store)?.chunks;
    let legacy_receipts = store
        .flat
        .source_states()
        .map_err(anyhow::Error::new)?
        .into_iter()
        .map(|state| {
            state
                .receipt
                .ok_or_else(|| anyhow!("missing legacy receipt"))
        })
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(legacy_receipts.len(), 2);
    assert!(legacy_receipts.iter().all(|receipt| {
        receipt.contract_fingerprint == legacy.contract_fingerprint
            && receipt.semantic_policy_fingerprint == legacy.semantic_policy_fingerprint
    }));
    assert_eq!(
        store
            .source_acknowledgement()?
            .ok_or_else(|| anyhow!("missing legacy acknowledgement"))?
            .contract_fingerprint,
        legacy.contract_fingerprint
    );

    let mut malformed = legacy_receipts[0].clone();
    malformed.contract_fingerprint.push('0');
    assert!(!source_receipt_allows_vector_reuse(&malformed, &current));
    malformed.contract_fingerprint = legacy.contract_fingerprint.clone();
    malformed.semantic_policy_fingerprint.push('0');
    assert!(!source_receipt_allows_vector_reuse(&malformed, &current));

    builder.calls.clear();
    builder.fail_after = Some(1);
    let embedding_calls = embedder.calls;
    let embedded_chunks = embedder.chunks;
    let error = store
        .reconcile_source_backed_generation(&index, &current, &mut builder, &mut embedder)
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("forced Core projection interruption"));
    assert_eq!(embedder.calls, embedding_calls);
    assert_eq!(embedder.chunks, embedded_chunks);
    let interrupted_fingerprints = store
        .flat
        .source_states()
        .map_err(anyhow::Error::new)?
        .into_iter()
        .map(|state| {
            state
                .receipt
                .map(|receipt| receipt.contract_fingerprint)
                .ok_or_else(|| anyhow!("missing interrupted migration receipt"))
        })
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(
        interrupted_fingerprints
            .iter()
            .filter(|fingerprint| *fingerprint == &current.contract_fingerprint)
            .count(),
        1
    );
    assert_eq!(
        interrupted_fingerprints
            .iter()
            .filter(|fingerprint| *fingerprint == &legacy.contract_fingerprint)
            .count(),
        1
    );
    let frontier = store
        .source_frontier()?
        .ok_or_else(|| anyhow!("interrupted migration lost its frontier"))?;
    assert_eq!(frontier.contract_fingerprint, current.contract_fingerprint);
    assert!(frontier.vector_reuse_allowed);

    drop(store);
    let mut store = SemanticVectorStore::open(&fixture.semantic_path, model_contract)?;
    builder.fail_after = None;
    builder.calls.clear();
    let resumed = reconcile_generation(&mut store, &index, &current, &mut builder, &mut embedder)?;
    assert_eq!(resumed.records_embedded, 0);
    assert_eq!(resumed.records_reused, 1);
    assert_eq!(embedder.calls, embedding_calls);
    assert_eq!(embedder.chunks, embedded_chunks);
    assert_eq!(projection_snapshot(&store)?.chunks, legacy_chunks);

    let acknowledgement = store
        .source_acknowledgement()?
        .ok_or_else(|| anyhow!("missing migrated acknowledgement"))?;
    assert_eq!(
        acknowledgement.contract_fingerprint,
        current.contract_fingerprint
    );
    assert_eq!(
        acknowledgement.semantic_policy_fingerprint,
        current.semantic_policy_fingerprint
    );
    assert_eq!(
        acknowledgement.consumer_build_id,
        super::super::manifest::source_consumer_build_id(
            &current.contract_fingerprint,
            index.generation_id(),
        )
    );
    let migrated_receipts = store
        .flat
        .source_states()
        .map_err(anyhow::Error::new)?
        .into_iter()
        .map(|state| {
            state
                .receipt
                .ok_or_else(|| anyhow!("missing migrated receipt"))
        })
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(migrated_receipts.len(), 2);
    assert!(migrated_receipts.iter().all(|receipt| {
        receipt.contract_fingerprint == current.contract_fingerprint
            && receipt.semantic_policy_fingerprint == current.semantic_policy_fingerprint
    }));

    builder.calls.clear();
    let no_op = reconcile_all(&mut store, &index, &mut builder, &mut embedder)?;
    assert_eq!(no_op.records_decoded, 0);
    assert_eq!(no_op.records_embedded, 0);
    assert_eq!(no_op.metadata_records_touched, 0);
    assert!(builder.calls.is_empty());
    assert_eq!(embedder.calls, embedding_calls);
    assert_eq!(embedder.chunks, embedded_chunks);
    Ok(())
}

#[test]
fn policy_rebuild_persists_linear_source_traversal_across_restart() -> Result<()> {
    let fixture = Fixture::new(8)?;
    let specs = (0..8)
        .map(|source| (source, bodies(&format!("source-{source}"), 1)))
        .collect::<Vec<_>>();
    let index = fixture.publish("linear-rebuild", &specs)?;
    let mut store = SemanticVectorStore::open(&fixture.semantic_path, semantic_model_contract())?;
    reconcile_all(
        &mut store,
        &index,
        &mut CoreBuilder::default(),
        &mut MarkerEmbedder::default(),
    )?;

    let mut revised_policy = current_semantic_generation_policy();
    revised_policy.chunking_revision = revised_policy
        .chunking_revision
        .checked_add(1)
        .ok_or_else(|| anyhow!("semantic chunking revision overflow"))?;
    let revised = SourceBackedSemanticGeneration::from_verified_index_with_policy(
        &index,
        revised_policy,
        semantic_model_contract(),
    )?;
    let mut builder = CoreBuilder {
        fail_after: Some(3),
        ..CoreBuilder::default()
    };
    let mut embedder = MarkerEmbedder::default();

    let error = store
        .reconcile_source_backed_generation(&index, &revised, &mut builder, &mut embedder)
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("forced Core projection interruption"));
    assert_eq!(builder.calls.len(), 4);
    let completed_before_fault = builder.calls[..3].iter().copied().collect::<HashSet<_>>();

    drop(store);
    let mut store = SemanticVectorStore::open(&fixture.semantic_path, semantic_model_contract())?;
    store.reset_flat_active_event_snapshot_count();
    builder.fail_after = None;
    builder.calls.clear();
    let resumed = reconcile_generation(&mut store, &index, &revised, &mut builder, &mut embedder)?;
    assert_eq!(resumed.records_decoded, 5);
    assert_eq!(builder.calls.len(), 5);
    assert!(builder
        .calls
        .iter()
        .all(|event_id| !completed_before_fault.contains(event_id)));
    assert_eq!(store.flat.source_catalog_load_count(), 0);
    assert_eq!(store.flat.source_catalog_records_replayed(), 0);
    assert_eq!(store.flat.source_publication_count(), 5);
    Ok(())
}

#[test]
fn control_reset_retires_unowned_flat_vectors_before_rebuild() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let record_count = MAX_SOURCE_EVENT_PAGE_ITEMS + 4;
    let initial = fixture.publish("reset-a", &[(0, bodies("retained", record_count))])?;
    let target = fixture.publish("reset-b", &[(0, bodies("retained", 3))])?;
    let removed_event = fixture.event_id(0, u64::try_from(record_count - 1)?)?;
    let mut store = SemanticVectorStore::open(&fixture.semantic_path, semantic_model_contract())?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    reconcile_all(&mut store, &initial, &mut builder, &mut embedder)?;

    drop(store);
    let control = rusqlite::Connection::open(fixture.semantic_path.join("state.sqlite"))?;
    control.pragma_update(None, "user_version", 5)?;
    drop(control);
    let mut store = SemanticVectorStore::open(&fixture.semantic_path, semantic_model_contract())?;
    builder.calls.clear();
    let first_drain = store.reconcile_source_backed_index(&target, &mut builder, &mut embedder)?;
    assert_eq!(first_drain.deleted_chunks, MAX_SOURCE_EVENT_PAGE_ITEMS);
    assert!(first_drain.work_remaining);

    drop(store);
    let mut store = SemanticVectorStore::open(&fixture.semantic_path, semantic_model_contract())?;
    store.reset_flat_active_event_snapshot_count();
    let rebuilt = reconcile_all(&mut store, &target, &mut builder, &mut embedder)?;
    assert_eq!(rebuilt.records_decoded, 3);
    assert_eq!(rebuilt.records_embedded, 3);
    assert_eq!(
        first_drain.deleted_chunks + rebuilt.deleted_chunks,
        record_count
    );
    assert_eq!(
        store.flat_active_event_snapshot_count(),
        1,
        "the reset drain materializes one global view; replacement remains source-local"
    );
    assert_eq!(
        store.flat.active_generation_load_count(),
        0,
        "cold rebuild and source completion must not pin the corpus"
    );
    assert_eq!(active_events(&store)?, 3);
    let final_pin = store
        .flat_pin_generation()?
        .ok_or_else(|| anyhow!("rebuilt projection lost its flat generation"))?;
    assert_eq!(
        final_pin.stats().segment_count,
        2,
        "rebuild retains one source vector segment and one catalog snapshot"
    );
    assert!(final_pin
        .active_events()
        .iter()
        .all(|event| event.event_id != removed_event));
    Ok(())
}
