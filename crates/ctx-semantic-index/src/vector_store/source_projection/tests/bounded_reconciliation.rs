use super::*;

#[test]
fn empty_full_rebuild_transition_continues_to_a_sequenced_boundary() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let index = fixture.publish("bounded-empty-full-rebuild", &[])?;
    let mut store = open_store(&fixture.semantic_path)?;
    store.record_flat_model_contract_reset()?;
    let mut sequences = Vec::new();

    let outcome = store
        .reconcile_source_backed_index_one_durable_boundary_with_checkpoint_and_progress(
            &index,
            &mut CoreBuilder::default(),
            &mut MarkerEmbedder::default(),
            &mut || Ok(()),
            &mut |sequence| {
                sequences.push(sequence);
                Ok(())
            },
        )?;

    assert!(outcome.ready());
    assert!(!outcome.work_remaining());
    assert_eq!(outcome.semantic_progress_sequence(), Some(1));
    assert_eq!(sequences, vec![1]);
    Ok(())
}

#[test]
fn one_durable_boundary_per_call_resumes_a_multi_page_source() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let record_count = source_event_page_limit(semantic_model_contract()) + 1;
    let index = fixture.publish(
        "bounded-multi-page",
        &[(0, bodies("bounded", record_count))],
    )?;
    let mut store = open_store(&fixture.semantic_path)?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    let mut sequences = Vec::new();

    let first = store
        .reconcile_source_backed_index_one_durable_boundary_with_checkpoint_and_progress(
            &index,
            &mut builder,
            &mut embedder,
            &mut || Ok(()),
            &mut |sequence| {
                sequences.push(sequence);
                Ok(())
            },
        )?;
    assert!(!first.ready());
    assert!(first.work_remaining());
    assert_eq!(first.semantic_progress_sequence(), Some(1));
    assert_eq!(
        first.records_decoded(),
        source_event_page_limit(semantic_model_contract())
    );
    assert_eq!(
        builder.calls.len(),
        source_event_page_limit(semantic_model_contract())
    );
    assert_eq!(sequences, vec![1]);

    let second = store
        .reconcile_source_backed_index_one_durable_boundary_with_checkpoint_and_progress(
            &index,
            &mut builder,
            &mut embedder,
            &mut || Ok(()),
            &mut |sequence| {
                sequences.push(sequence);
                Ok(())
            },
        )?;
    assert!(!second.ready());
    assert!(second.work_remaining());
    assert_eq!(second.semantic_progress_sequence(), Some(2));
    assert_eq!(second.records_decoded(), 1);
    assert_eq!(builder.calls.len(), record_count);
    assert_eq!(sequences, vec![1, 2]);

    let third = store
        .reconcile_source_backed_index_one_durable_boundary_with_checkpoint_and_progress(
            &index,
            &mut builder,
            &mut embedder,
            &mut || Ok(()),
            &mut |sequence| {
                sequences.push(sequence);
                Ok(())
            },
        )?;
    assert!(!third.ready());
    assert!(third.work_remaining());
    assert_eq!(third.semantic_progress_sequence(), Some(3));
    assert_eq!(third.records_decoded(), 0);
    assert_eq!(builder.calls.len(), record_count);
    assert_eq!(sequences, vec![1, 2, 3]);

    let fourth = store
        .reconcile_source_backed_index_one_durable_boundary_with_checkpoint_and_progress(
            &index,
            &mut builder,
            &mut embedder,
            &mut || Ok(()),
            &mut |sequence| {
                sequences.push(sequence);
                Ok(())
            },
        )?;
    assert!(fourth.ready());
    assert!(!fourth.work_remaining());
    assert_eq!(fourth.semantic_progress_sequence(), Some(4));
    assert_eq!(builder.calls.len(), record_count);
    assert_eq!(sequences, vec![1, 2, 3, 4]);
    Ok(())
}

#[test]
fn ordinary_reconciliation_still_drains_a_multi_page_source() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let record_count = source_event_page_limit(semantic_model_contract()) + 1;
    let index = fixture.publish(
        "ordinary-multi-page",
        &[(0, bodies("ordinary", record_count))],
    )?;
    let mut store = open_store(&fixture.semantic_path)?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();

    let outcome = store.reconcile_source_backed_index(&index, &mut builder, &mut embedder)?;

    assert!(outcome.ready());
    assert!(!outcome.work_remaining());
    assert_eq!(outcome.semantic_progress_sequence(), Some(4));
    assert_eq!(outcome.records_decoded(), record_count);
    assert_eq!(builder.calls.len(), record_count);
    Ok(())
}
