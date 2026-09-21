use super::*;

use crate::materializer::CoreGenerationStart;

#[test]
fn direct_session_publication_failure_preserves_active_and_drop_cleans_candidate() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("graph");

    let mut materializer = SegmentMaterializer::open(&root)?;

    let first_head = head(0x21, &[])?;
    let first_receipt = match materializer.start_core_generation(first_head.clone())? {
        CoreGenerationStart::Current(_) => {
            return Err(io::Error::other("fresh generation unexpectedly current").into());
        }
        CoreGenerationStart::Started(session) => session.activate()?,
    };
    let first_manifest = crate::graph::segment::SegmentStore::new(&root)
        .load_active()?
        .ok_or_else(|| io::Error::other("first publication has no active manifest"))?;

    let hook = super::super::publication::install_publication_transaction_failure_test_hook(
        &root,
        super::super::publication::PublicationTransactionTestFault::BeforeManifestActivation,
    )?;
    let second = match materializer.start_core_generation(head(0x22, &[])?)? {
        CoreGenerationStart::Current(_) => {
            return Err(io::Error::other("successor generation unexpectedly current").into());
        }
        CoreGenerationStart::Started(session) => session.activate(),
    };
    assert!(second.is_err());
    drop(hook);

    let still_active = crate::graph::segment::SegmentStore::new(&root)
        .load_active()?
        .ok_or_else(|| io::Error::other("failed publication removed the active manifest"))?;
    assert_eq!(still_active.generation_id, first_manifest.generation_id);
    assert_eq!(still_active.core_receipt, first_receipt);
    drop(materializer);

    let mut reopened = SegmentMaterializer::open(&root)?;
    let status = reopened.projection_status(&StatusRequest {
        requested_core_generation_id: Some(first_head.core_generation_id),
    })?;
    assert_eq!(status.currentness, CoreProjectionCurrentness::Current);
    assert_eq!(status.receipt, Some(first_receipt));

    let session = match reopened.start_core_generation(head(0x23, &[])?)? {
        CoreGenerationStart::Current(_) => {
            return Err(io::Error::other("second successor unexpectedly current").into());
        }
        CoreGenerationStart::Started(session) => session,
    };
    drop(session);
    let cleanup_probe = match reopened.start_core_generation(head(0x24, &[])?)? {
        CoreGenerationStart::Current(_) => {
            return Err(io::Error::other("aborted successor unexpectedly current").into());
        }
        CoreGenerationStart::Started(session) => session,
    };
    drop(cleanup_probe);
    Ok(())
}

#[test]
fn direct_session_post_activation_sync_failure_preserves_disk_winner_segments() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("graph");

    let record = one_record()?;
    let source = source_state(&record, 0x54);
    let winner_head = head(0x54, std::slice::from_ref(&source))?;
    let mut materializer = SegmentMaterializer::open(&root)?;

    let mut session = match materializer.start_core_generation(winner_head.clone())? {
        CoreGenerationStart::Current(_) => {
            return Err(io::Error::other("fresh generation unexpectedly current").into());
        }
        CoreGenerationStart::Started(session) => session,
    };
    let reconciliations = session.reconcile_source_page(protocol(CoreSourceDeltaPage::new(
        "0".repeat(64),
        winner_head.core_generation_id.clone(),
        0,
        true,
        vec![CoreSourceDelta::Present(source.clone())],
    ))?)?;
    assert_eq!(reconciliations.len(), 1);
    session.ingest_event_pages(vec![CoreEventDeltaPage {
        materialization_id: "0".repeat(64),
        core_generation_id: winner_head.core_generation_id.clone(),
        reconciliation: reconciliations[0].clone(),
        page_index: 0,
        terminal: true,
        deltas: vec![CoreEventDelta::Added(record.clone())],
    }])?;

    let store = crate::graph::segment::SegmentStore::new(&root);
    store.fail_next_post_activation_sync_for_test()?;
    assert!(matches!(
        session.activate(),
        Err(SegmentMaterializerError::Store(
            crate::graph::segment::SegmentStoreError::ActivationDurabilityUncertain { .. }
        ))
    ));

    let winner = store
        .load_active()?
        .ok_or_else(|| io::Error::other("post-activation winner has no active manifest"))?;
    assert_eq!(
        winner.core_receipt.core_generation_id,
        winner_head.core_generation_id
    );
    for reference in winner.segments.iter().filter(|reference| {
        reference.role == super::super::model::MATERIALIZER_SOURCE_ROLE
            || reference.role == crate::graph::segment::EVENT_STATE_INDEX_ROLE
    }) {
        assert!(root.join(&reference.file_name).is_file());
    }
    assert!(
        winner
            .segments
            .iter()
            .any(|reference| reference.role == super::super::model::MATERIALIZER_SOURCE_ROLE)
    );
    assert!(
        winner
            .segments
            .iter()
            .any(|reference| reference.role == crate::graph::segment::EVENT_STATE_INDEX_ROLE)
    );

    drop(materializer);
    let mut reopened = SegmentMaterializer::open(&root)?;
    let status = reopened.projection_status(&StatusRequest {
        requested_core_generation_id: Some(winner_head.core_generation_id.clone()),
    })?;
    assert_eq!(status.currentness, CoreProjectionCurrentness::Current);
    assert_eq!(
        status
            .receipt
            .as_ref()
            .map(|receipt| &receipt.core_generation_id),
        Some(&winner_head.core_generation_id)
    );

    let mut query_source = source;
    query_source.core_record_accumulator = hex::encode([0x55; 32]);
    let query_head = head(0x55, std::slice::from_ref(&query_source))?;
    let mut query = match reopened.start_core_generation(query_head.clone())? {
        CoreGenerationStart::Current(_) => {
            return Err(io::Error::other("query generation unexpectedly current").into());
        }
        CoreGenerationStart::Started(session) => session,
    };
    let query_reconciliations =
        query.reconcile_source_page(protocol(CoreSourceDeltaPage::new(
            "0".repeat(64),
            query_head.core_generation_id,
            0,
            true,
            vec![CoreSourceDelta::Present(query_source)],
        ))?)?;
    let (events, terminal) = query.event_states(&query_reconciliations[0], None)?;
    assert!(terminal);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_id, record.event_id);
    drop(query);
    Ok(())
}

#[test]
fn cancellation_at_final_publication_fence_preserves_active_and_retry() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("graph");
    let mut materializer = SegmentMaterializer::open(&root)?;
    let first = match materializer.start_core_generation(head(0x81, &[])?)? {
        CoreGenerationStart::Started(session) => session.activate()?,
        CoreGenerationStart::Current(_) => panic!("fresh fixture"),
    };
    let store = crate::graph::segment::SegmentStore::new(&root);
    let original = store.load_active()?.unwrap();
    let hook = super::super::publication::install_publication_transaction_failure_test_hook(
        &root,
        super::super::publication::PublicationTransactionTestFault::CancelBeforeManifestActivation,
    )?;
    let next = head(0x82, &[])?;
    let result = match materializer.start_core_generation(next.clone())? {
        CoreGenerationStart::Started(session) => {
            session.activate_cancellable(Some(&|| hook.cancellation_requested()))
        }
        CoreGenerationStart::Current(_) => panic!("new generation"),
    };
    assert!(matches!(result, Err(SegmentMaterializerError::Cancelled)));
    assert_eq!(store.load_active()?.unwrap(), original);
    assert!(std::fs::read_dir(&root)?.all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".candidate")
    }));
    assert_eq!(store.load_active()?.unwrap().core_receipt, first);
    drop(hook);
    match materializer.start_core_generation(next.clone())? {
        CoreGenerationStart::Started(session) => {
            assert_eq!(
                session.activate()?.core_generation_id,
                next.core_generation_id
            );
        }
        CoreGenerationStart::Current(_) => panic!("cancelled candidate must not have published"),
    }
    Ok(())
}
