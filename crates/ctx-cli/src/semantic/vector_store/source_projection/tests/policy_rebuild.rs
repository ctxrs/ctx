use super::*;

#[test]
fn policy_model_or_chunk_revision_forces_full_rebuild() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let index = fixture.publish("revision", &[(0, bodies("first", 130))])?;
    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    reconcile_all(&mut store, &index, &mut builder, &mut embedder)?;
    let chunks_before = embedder.chunks;
    store.reset_flat_active_event_snapshot_count();

    let mut revised = SourceBackedSemanticGeneration::from_verified_index(&index)?;
    revised.semantic_policy_fingerprint = "f".repeat(64);
    builder.calls.clear();
    let rebuilt = reconcile_generation(&mut store, &index, &revised, &mut builder, &mut embedder)?;
    assert_eq!(rebuilt.records_read, 130);
    assert_eq!(rebuilt.records_embedded, 130);
    assert_eq!(builder.calls.len(), 130);
    assert!(embedder.chunks > chunks_before);
    assert_eq!(
        store.flat_active_event_snapshot_count(),
        0,
        "policy replacement must remain source-local"
    );
    Ok(())
}

#[test]
fn policy_rebuild_persists_linear_source_traversal_across_restart() -> Result<()> {
    let fixture = Fixture::new(8)?;
    let specs = (0..8)
        .map(|source| (source, bodies(&format!("source-{source}"), 1)))
        .collect::<Vec<_>>();
    let index = fixture.publish("linear-rebuild", &specs)?;
    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
    reconcile_all(
        &mut store,
        &index,
        &mut CoreBuilder::default(),
        &mut MarkerEmbedder::default(),
    )?;

    let mut revised = SourceBackedSemanticGeneration::from_verified_index(&index)?;
    revised.semantic_policy_fingerprint = "e".repeat(64);
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
    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
    store.reset_flat_active_event_snapshot_count();
    builder.fail_after = None;
    builder.calls.clear();
    let resumed = reconcile_generation(&mut store, &index, &revised, &mut builder, &mut embedder)?;
    assert_eq!(resumed.records_read, 5);
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
    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    reconcile_all(&mut store, &initial, &mut builder, &mut embedder)?;

    drop(store);
    let control = rusqlite::Connection::open(fixture.semantic_path.join("state.sqlite"))?;
    control.pragma_update(None, "user_version", 2)?;
    drop(control);
    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
    builder.calls.clear();
    let first_drain = store.reconcile_source_backed_index(&target, &mut builder, &mut embedder)?;
    assert_eq!(first_drain.deleted_chunks, MAX_SOURCE_EVENT_PAGE_ITEMS);
    assert!(first_drain.work_remaining);

    drop(store);
    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
    store.reset_flat_active_event_snapshot_count();
    let rebuilt = reconcile_all(&mut store, &target, &mut builder, &mut embedder)?;
    assert_eq!(rebuilt.records_read, 3);
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
