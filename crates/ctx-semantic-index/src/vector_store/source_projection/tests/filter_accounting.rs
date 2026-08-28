use super::*;

#[test]
fn mixed_controls_and_empty_content_publish_exact_filter_accounting() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let index = fixture.publish(
        "mixed-content-filters",
        &[(
            0,
            vec![
                "ordinary semantic question".to_owned(),
                "<environment_context>private control</environment_context>".to_owned(),
                "Warning: The maximum number of unified exec processes was reached".to_owned(),
                EMPTY_DOCUMENT_TOKEN.to_owned(),
            ],
        )],
    )?;
    let mut store = SemanticVectorStore::open(&fixture.semantic_path, semantic_model_contract())?;
    let outcome = reconcile_all(
        &mut store,
        &index,
        &mut CoreBuilder::default(),
        &mut MarkerEmbedder::default(),
    )?;

    assert_eq!(index.semantic_eligible_event_count()?, 4);
    assert_eq!(outcome.records_embedded, 1);
    assert_eq!(outcome.records_filtered, 3);
    let acknowledgement = store
        .source_acknowledgement()?
        .ok_or_else(|| anyhow!("mixed filter projection was not acknowledged"))?;
    assert_eq!(acknowledgement.semantic_documents, 4);
    assert_eq!(acknowledgement.projected_documents, 1);
    assert_eq!(acknowledgement.filtered_documents, 3);
    assert_eq!(
        acknowledgement
            .projected_documents
            .checked_add(acknowledgement.filtered_documents),
        Some(acknowledgement.semantic_documents)
    );
    let receipt = store
        .flat
        .source_states()
        .map_err(anyhow::Error::new)?
        .into_iter()
        .next()
        .and_then(|state| state.receipt)
        .ok_or_else(|| anyhow!("mixed filter source receipt is missing"))?;
    assert_eq!(receipt.semantic_eligible_documents, 4);
    assert_eq!(receipt.owned_event_count, 1);
    assert_eq!(receipt.filtered_event_count, 3);
    assert!(matches!(
        store.source_backed_generation_pin_exact(index.generation_id(), 4)?,
        SourceBackedGenerationPin::Ready(_)
    ));

    let flat_generation = store
        .flat_pin_generation()?
        .ok_or_else(|| anyhow!("mixed filter Flat generation is missing"))?
        .generation();
    let unchanged = fixture.publish(
        "mixed-content-filters-noop",
        &[(
            0,
            vec![
                "ordinary semantic question".to_owned(),
                "<environment_context>private control</environment_context>".to_owned(),
                "Warning: The maximum number of unified exec processes was reached".to_owned(),
                EMPTY_DOCUMENT_TOKEN.to_owned(),
            ],
        )],
    )?;
    let no_op = reconcile_all(
        &mut store,
        &unchanged,
        &mut CoreBuilder::default(),
        &mut MarkerEmbedder::default(),
    )?;
    assert_eq!(no_op.records_decoded, 0);
    assert_eq!(
        store
            .flat_pin_generation()?
            .ok_or_else(|| anyhow!("no-op lost the Flat generation"))?
            .generation(),
        flat_generation
    );
    let acknowledgement = store
        .source_acknowledgement()?
        .ok_or_else(|| anyhow!("no-op acknowledgement is missing"))?;
    assert_eq!(acknowledgement.semantic_documents, 4);
    assert_eq!(acknowledgement.projected_documents, 1);
    assert_eq!(acknowledgement.filtered_documents, 3);
    Ok(())
}

#[test]
fn all_filtered_generation_is_ready_empty_with_exact_accounting() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let index = fixture.publish(
        "all-filtered",
        &[(
            0,
            vec![
                "<environment_context>control</environment_context>".to_owned(),
                "<turn_aborted>control</turn_aborted>".to_owned(),
                EMPTY_DOCUMENT_TOKEN.to_owned(),
            ],
        )],
    )?;
    let mut store = SemanticVectorStore::open(&fixture.semantic_path, semantic_model_contract())?;
    let outcome = reconcile_all(
        &mut store,
        &index,
        &mut CoreBuilder::default(),
        &mut MarkerEmbedder::default(),
    )?;

    assert_eq!(outcome.records_embedded, 0);
    assert_eq!(outcome.records_filtered, 3);
    assert_eq!(active_events(&store)?, 0);
    let acknowledgement = store
        .source_acknowledgement()?
        .ok_or_else(|| anyhow!("all-filtered projection was not acknowledged"))?;
    assert_eq!(acknowledgement.semantic_documents, 3);
    assert_eq!(acknowledgement.projected_documents, 0);
    assert_eq!(acknowledgement.filtered_documents, 3);
    assert!(matches!(
        store.source_backed_generation_pin_exact(index.generation_id(), 3)?,
        SourceBackedGenerationPin::ReadyEmpty
    ));
    Ok(())
}

#[test]
fn unaccounted_source_drop_cannot_publish_a_receipt() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let index = fixture.publish(
        "unaccounted-drop",
        &[(
            0,
            vec![
                "ordinary semantic question".to_owned(),
                "<subagent_notification>control</subagent_notification>".to_owned(),
            ],
        )],
    )?;
    let mut store = SemanticVectorStore::open(&fixture.semantic_path, semantic_model_contract())?;
    store.flat.fail_after_source_frontier_commit_once();
    let error = store
        .reconcile_source_backed_index(
            &index,
            &mut CoreBuilder::default(),
            &mut MarkerEmbedder::default(),
        )
        .expect_err("fault must stop before the source receipt is published");
    assert!(error
        .to_string()
        .contains("injected failure after semantic source frontier commit"));
    let mut frontier = store
        .source_frontier()?
        .ok_or_else(|| anyhow!("faulted source lost its durable frontier"))?;
    assert_eq!(frontier.processed_source_semantic_documents, 2);
    assert_eq!(frontier.processed_source_filtered_documents, 1);
    frontier.processed_source_filtered_documents = 0;
    store.store_source_frontier(&frontier)?;

    let error = store
        .reconcile_source_backed_index(
            &index,
            &mut CoreBuilder::default(),
            &mut MarkerEmbedder::default(),
        )
        .expect_err("an unaccounted candidate must fail source finalization");
    assert!(error
        .to_string()
        .contains("source receipt does not match its staged Core aggregate"));
    assert!(store.source_acknowledgement()?.is_none());
    Ok(())
}

#[test]
fn corrupted_filter_acknowledgement_is_not_query_ready() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let index = fixture.publish(
        "corrupt-filter-ack",
        &[(
            0,
            vec![
                "ordinary semantic question".to_owned(),
                "<environment_context>control</environment_context>".to_owned(),
            ],
        )],
    )?;
    let mut store = SemanticVectorStore::open(&fixture.semantic_path, semantic_model_contract())?;
    reconcile_all(
        &mut store,
        &index,
        &mut CoreBuilder::default(),
        &mut MarkerEmbedder::default(),
    )?;
    let mut acknowledgement = store
        .source_acknowledgement()?
        .ok_or_else(|| anyhow!("projection acknowledgement is missing"))?;
    acknowledgement.filtered_documents = 0;
    store.conn.execute(
        "UPDATE semantic_maintenance_state SET value = ?1 WHERE key = ?2",
        rusqlite::params![
            serde_json::to_string(&acknowledgement)?,
            super::super::manifest::SOURCE_ACKNOWLEDGEMENT_STATE
        ],
    )?;

    assert!(matches!(
        store.source_backed_generation_pin_exact(index.generation_id(), 2)?,
        SourceBackedGenerationPin::NotReady
    ));
    Ok(())
}

#[test]
fn vector_reuse_preserves_intentional_filter_accounting() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let records = vec![
        (1, "same semantic body".to_owned()),
        (
            2,
            "<environment_context>same control body</environment_context>".to_owned(),
        ),
    ];
    let resequenced = vec![
        (91, "same semantic body".to_owned()),
        (
            92,
            "<environment_context>same control body</environment_context>".to_owned(),
        ),
    ];
    let initial = fixture.publish_with_event_sequences("reuse-filter-a", &[(0, records)])?;
    let target = fixture.publish_with_event_sequences("reuse-filter-b", &[(0, resequenced)])?;
    let mut store = SemanticVectorStore::open(&fixture.semantic_path, semantic_model_contract())?;
    reconcile_all(
        &mut store,
        &initial,
        &mut CoreBuilder::default(),
        &mut MarkerEmbedder::default(),
    )?;

    let outcome = reconcile_all(
        &mut store,
        &target,
        &mut CoreBuilder::default(),
        &mut MarkerEmbedder::default(),
    )?;
    assert_eq!(outcome.records_reused, 1);
    assert_eq!(outcome.records_embedded, 0);
    assert_eq!(outcome.records_filtered, 1);
    let acknowledgement = store
        .source_acknowledgement()?
        .ok_or_else(|| anyhow!("reuse projection acknowledgement is missing"))?;
    assert_eq!(acknowledgement.semantic_documents, 2);
    assert_eq!(acknowledgement.projected_documents, 1);
    assert_eq!(acknowledgement.filtered_documents, 1);
    assert!(matches!(
        store.source_backed_generation_pin_exact(target.generation_id(), 2)?,
        SourceBackedGenerationPin::Ready(_)
    ));
    Ok(())
}
