use super::*;

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
