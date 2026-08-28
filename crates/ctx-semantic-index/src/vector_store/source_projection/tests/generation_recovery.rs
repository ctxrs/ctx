#[test]
fn two_lost_candidate_publications_rebuild_from_flat_authority() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let initial = fixture.publish("two-loss-a", &[(0, bodies("initial", 3))])?;
    let middle = fixture.publish("two-loss-b", &[(0, bodies("middle", 3))])?;
    let target = fixture.publish("two-loss-c", &[(0, bodies("target", 4))])?;
    let mut store = SemanticVectorStore::open(&fixture.semantic_path, semantic_model_contract())?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    reconcile_all(&mut store, &initial, &mut builder, &mut embedder)?;
    reconcile_all(&mut store, &middle, &mut builder, &mut embedder)?;
    reconcile_all(&mut store, &target, &mut builder, &mut embedder)?;
    let expected = projection_snapshot(&store)?;
    let newest = store.flat.rollback_active_manifest()?;
    let preceding = store.flat.rollback_active_manifest()?;
    assert!(newest.generation > preceding.generation);
    drop(store);

    builder.calls.clear();
    let mut restarted =
        SemanticVectorStore::open(&fixture.semantic_path, semantic_model_contract())?;
    reconcile_all(&mut restarted, &target, &mut builder, &mut embedder)?;
    assert!(
        !builder.calls.is_empty(),
        "lost candidates must trigger a rebuild"
    );
    assert_eq!(projection_snapshot(&restarted)?, expected);
    Ok(())
}

#[test]
fn same_generation_wrong_hash_fails_closed() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let index = fixture.publish("wrong-hash", &[(0, bodies("hash", 2))])?;
    let mut store = SemanticVectorStore::open(&fixture.semantic_path, semantic_model_contract())?;
    store.flat.fail_after_source_frontier_commit_once();
    let error = store
        .reconcile_source_backed_index(
            &index,
            &mut CoreBuilder::default(),
            &mut MarkerEmbedder::default(),
        )
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("injected failure after semantic source frontier commit"));
    let mut frontier = store
        .source_frontier()?
        .ok_or_else(|| anyhow!("staged page lost its frontier"))?;
    frontier.flat_publication.generation_hash = Some("0".repeat(64));
    store.store_source_frontier(&frontier)?;
    drop(store);

    let mut restarted =
        SemanticVectorStore::open(&fixture.semantic_path, semantic_model_contract())?;
    let error = restarted
        .reconcile_source_backed_index(
            &index,
            &mut CoreBuilder::default(),
            &mut MarkerEmbedder::default(),
        )
        .unwrap_err();
    assert!(error.to_string().contains("different manifest hash"));
    Ok(())
}

#[test]
fn disagreeing_retained_candidate_fails_closed() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let index = fixture.publish("candidate-hash", &[(0, bodies("candidate", 2))])?;
    let mut store = SemanticVectorStore::open(&fixture.semantic_path, semantic_model_contract())?;
    store.flat.fail_after_source_finalization_once();
    let error = store
        .reconcile_source_backed_index(
            &index,
            &mut CoreBuilder::default(),
            &mut MarkerEmbedder::default(),
        )
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("injected failure after semantic source finalization"));
    store
        .flat
        .corrupt_retained_source_candidate_hash()
        .map_err(anyhow::Error::new)?;
    drop(store);

    let mut restarted =
        SemanticVectorStore::open(&fixture.semantic_path, semantic_model_contract())?;
    let error = restarted
        .reconcile_source_backed_index(
            &index,
            &mut CoreBuilder::default(),
            &mut MarkerEmbedder::default(),
        )
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("retained Flat source candidate disagrees"));
    Ok(())
}

#[test]
fn full_compaction_preserves_receipts_and_readiness_exactly() -> Result<()> {
    let fixture = Fixture::new(2)?;
    let index = fixture.publish(
        "full-compaction",
        &[
            (
                0,
                vec![
                    "first projected".to_owned(),
                    "<turn_aborted>first filtered</turn_aborted>".to_owned(),
                    "first projected again".to_owned(),
                ],
            ),
            (
                1,
                vec![
                    "second projected".to_owned(),
                    EMPTY_DOCUMENT_TOKEN.to_owned(),
                ],
            ),
        ],
    )?;
    let mut store = SemanticVectorStore::open(&fixture.semantic_path, semantic_model_contract())?;
    reconcile_all(
        &mut store,
        &index,
        &mut CoreBuilder::default(),
        &mut MarkerEmbedder::default(),
    )?;
    let acknowledgement = store
        .source_acknowledgement()?
        .ok_or_else(|| anyhow!("pre-compaction acknowledgement is missing"))?;
    assert_eq!(acknowledgement.semantic_documents, 5);
    assert_eq!(acknowledgement.projected_documents, 3);
    assert_eq!(acknowledgement.filtered_documents, 2);
    let before = projection_snapshot(&store)?;
    let compacted = store.flat.compact().map_err(anyhow::Error::new)?;
    assert!(compacted.published);
    assert_eq!(projection_snapshot(&store)?, before);
    assert_eq!(
        store
            .source_acknowledgement()?
            .ok_or_else(|| anyhow!("post-compaction acknowledgement is missing"))?,
        acknowledgement
    );
    assert!(matches!(
        store.source_backed_generation_pin_exact(index.generation_id(), 5)?,
        SourceBackedGenerationPin::Ready(_)
    ));
    Ok(())
}

#[test]
fn core_advance_mid_catch_up_never_pins_mixed_generation() -> Result<()> {
    let fixture = Fixture::new(2)?;
    let initial = fixture.publish(
        "advance-a",
        &[(0, bodies("stable", 2)), (1, vec!["version a".to_owned()])],
    )?;
    let middle = fixture.publish(
        "advance-b",
        &[(0, bodies("stable", 2)), (1, vec!["version b".to_owned()])],
    )?;
    let newest = fixture.publish(
        "advance-c",
        &[
            (0, bodies("stable-new", 2)),
            (1, vec!["version a".to_owned()]),
        ],
    )?;
    let mut store = SemanticVectorStore::open(&fixture.semantic_path, semantic_model_contract())?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    reconcile_all(&mut store, &initial, &mut builder, &mut embedder)?;

    builder.calls.clear();
    builder.fail_after = Some(0);
    let error = store
        .reconcile_source_backed_index(&middle, &mut builder, &mut embedder)
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("forced Core projection interruption"));
    assert!(matches!(
        store.source_backed_generation_pin_exact(initial.generation_id(), 3)?,
        SourceBackedGenerationPin::NotReady
    ));
    assert!(matches!(
        store.source_backed_generation_pin_exact(middle.generation_id(), 3)?,
        SourceBackedGenerationPin::NotReady
    ));

    builder.fail_after = None;
    builder.calls.clear();
    let completed = reconcile_all(&mut store, &newest, &mut builder, &mut embedder)?;
    assert_eq!(completed.records_decoded, 2);
    assert_eq!(builder.calls.len(), 2);
    assert!(matches!(
        store.source_backed_generation_pin_exact(newest.generation_id(), 3)?,
        SourceBackedGenerationPin::Ready(_)
    ));
    assert!(matches!(
        store.source_backed_generation_pin_exact(middle.generation_id(), 3)?,
        SourceBackedGenerationPin::NotReady
    ));
    Ok(())
}
