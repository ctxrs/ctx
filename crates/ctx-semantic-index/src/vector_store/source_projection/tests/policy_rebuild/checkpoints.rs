use super::*;

#[derive(Debug, Eq, PartialEq)]
enum ProjectionCheckpointFailure {
    Interrupted,
    Superseded,
    PreCommit,
}

impl std::fmt::Display for ProjectionCheckpointFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "projection checkpoint failed: {self:?}")
    }
}

impl std::error::Error for ProjectionCheckpointFailure {}

#[test]
fn checkpoint_inside_source_page_loop_preserves_typed_interruption_and_hides_staged_pages(
) -> Result<()> {
    let fixture = Fixture::new(1)?;
    let contract = external_contract(
        "http://127.0.0.1:43123/v1/embeddings",
        "space-page-checkpoint",
        4_095,
    )?;
    let page_limit = source_event_page_limit(&contract);
    assert_eq!(page_limit, 64);
    let record_count = page_limit + 1;
    let index = fixture.publish(
        "page-checkpoint",
        &[(0, bodies("page-checkpoint", record_count))],
    )?;
    let mut store = SemanticVectorStore::open(&fixture.semantic_path, &contract)?;
    let mut embedder = DimensionEmbedder::new(&contract);
    let mut checkpoint_calls = 0_usize;

    let error = store
        .reconcile_source_backed_index_with_checkpoint(
            &index,
            &mut CoreBuilder::default(),
            &mut embedder,
            &mut || {
                checkpoint_calls = checkpoint_calls.saturating_add(1);
                if checkpoint_calls == 2 {
                    return Err(ProjectionCheckpointFailure::Interrupted.into());
                }
                Ok(())
            },
        )
        .unwrap_err();

    assert_eq!(
        error.downcast_ref::<ProjectionCheckpointFailure>(),
        Some(&ProjectionCheckpointFailure::Interrupted)
    );
    assert_eq!(checkpoint_calls, 2);
    assert_eq!(embedder.batch_sizes, vec![page_limit, 1]);
    assert_eq!(active_events(&store)?, 0);
    assert!(store.source_acknowledgement()?.is_none());
    assert!(matches!(
        store.source_backed_generation_pin_exact(
            index.generation_id(),
            u64::try_from(record_count)?,
        )?,
        SourceBackedGenerationPin::NotReady
    ));
    Ok(())
}

#[test]
fn checkpoint_before_source_publication_rejects_supersession_without_stale_commit() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let contract = external_contract(
        "http://127.0.0.1:43124/v1/embeddings",
        "space-source-commit-checkpoint",
        4_095,
    )?;
    let page_limit = source_event_page_limit(&contract);
    let record_count = page_limit + 1;
    let index = fixture.publish(
        "source-commit-checkpoint",
        &[(0, bodies("source-commit-checkpoint", record_count))],
    )?;
    let mut store = SemanticVectorStore::open(&fixture.semantic_path, &contract)?;
    let mut checkpoint_calls = 0_usize;

    let error = store
        .reconcile_source_backed_index_with_checkpoint(
            &index,
            &mut CoreBuilder::default(),
            &mut DimensionEmbedder::new(&contract),
            &mut || {
                checkpoint_calls = checkpoint_calls.saturating_add(1);
                if checkpoint_calls == 3 {
                    return Err(ProjectionCheckpointFailure::Superseded.into());
                }
                Ok(())
            },
        )
        .unwrap_err();

    assert_eq!(
        error.downcast_ref::<ProjectionCheckpointFailure>(),
        Some(&ProjectionCheckpointFailure::Superseded)
    );
    assert_eq!(checkpoint_calls, 3);
    assert_eq!(active_events(&store)?, 0);
    assert!(store.source_acknowledgement()?.is_none());
    assert!(matches!(
        store.source_backed_generation_pin_exact(
            index.generation_id(),
            u64::try_from(record_count)?,
        )?,
        SourceBackedGenerationPin::NotReady
    ));
    Ok(())
}

#[test]
fn checkpoint_immediately_before_exact_acknowledgement_commit_rolls_back() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let contract = external_contract(
        "http://127.0.0.1:43128/v1/embeddings",
        "space-ack-commit-checkpoint",
        4_095,
    )?;
    let page_limit = source_event_page_limit(&contract);
    let record_count = page_limit + 1;
    let index = fixture.publish(
        "ack-commit-checkpoint",
        &[(0, bodies("ack-commit-checkpoint", record_count))],
    )?;
    let mut store = SemanticVectorStore::open(&fixture.semantic_path, &contract)?;
    let mut checkpoint_calls = 0_usize;

    let error = store
        .reconcile_source_backed_index_with_checkpoint(
            &index,
            &mut CoreBuilder::default(),
            &mut DimensionEmbedder::new(&contract),
            &mut || {
                checkpoint_calls = checkpoint_calls.saturating_add(1);
                if checkpoint_calls == 4 {
                    return Err(ProjectionCheckpointFailure::PreCommit.into());
                }
                Ok(())
            },
        )
        .unwrap_err();

    assert_eq!(
        error.downcast_ref::<ProjectionCheckpointFailure>(),
        Some(&ProjectionCheckpointFailure::PreCommit)
    );
    assert_eq!(checkpoint_calls, 4);
    assert!(store.source_acknowledgement()?.is_none());
    assert!(store.source_frontier()?.is_some());
    assert!(matches!(
        store.source_backed_generation_pin_exact(
            index.generation_id(),
            u64::try_from(record_count)?,
        )?,
        SourceBackedGenerationPin::NotReady
    ));
    drop(store);

    let reopened = SemanticVectorStore::open(&fixture.semantic_path, &contract)?;
    assert!(reopened.source_acknowledgement()?.is_none());
    assert!(reopened.source_frontier()?.is_some());
    Ok(())
}
