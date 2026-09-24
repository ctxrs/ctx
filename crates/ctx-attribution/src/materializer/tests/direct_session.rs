use super::*;

use crate::materializer::CoreGenerationStart;

fn force_aggregate_split(
    session: &mut super::super::lifecycle::CoreMaterializationSession<'_>,
    page: &CoreEventDeltaPage,
) -> TestResult {
    assert!(page.deltas.len() > 1);
    let mut preparer =
        protocol(crate::core_materialization::CoreProjectionPreparer::with_parallelism(1))?;
    let mut singleton = page.clone();
    singleton.deltas.truncate(1);
    singleton.terminal = false;
    let prepared = protocol(preparer.prepare_event_delta_page(singleton))?;
    let limit = crate::core_materialization::MAX_CORE_PREPARED_UNIT_BYTES
        + prepared.prepared_output_encoded_len();
    preparer.set_output_limit_for_test(limit);
    let error = preparer
        .prepare_event_delta_page(page.clone())
        .expect_err("whole page must cross the aggregate limit");
    assert_eq!(error.class, crate::protocol::ErrorClass::Bounds);
    session.set_prepared_output_limit_for_test(limit);
    Ok(())
}

#[cfg(unix)]
#[test]
fn aggregate_split_preserves_cross_event_fact_and_following_page() -> TestResult {
    use crate::graph::segment_graph::SegmentGraph;
    use crate::query::{BlameFactFamily, BlameGraph, ResourceSelector};

    let directory = tempfile::tempdir()?;
    let repository = directory.path().join("repository");
    std::fs::create_dir_all(repository.join("src"))?;
    super::native_shell::git(&repository, &["init", "-q"])?;
    super::native_shell::git(&repository, &["config", "user.name", "ctx fixture"])?;
    super::native_shell::git(
        &repository,
        &["config", "user.email", "fixture@example.invalid"],
    )?;
    std::fs::write(repository.join("src/witness.rs"), "// witness\n")?;
    super::native_shell::git(&repository, &["add", "."])?;
    super::native_shell::git(&repository, &["commit", "-qm", "fixture"])?;
    let oid = super::native_shell::git(&repository, &["rev-parse", "HEAD"])?;
    let template = super::optional_root::record(&repository, &oid)?;
    let mut records = (0..3)
        .map(|index| {
            let mut record = indexed_record(&template, index)?;
            record.native_event_id = Some(TypedKey::utf8(format!("commit-event-{index}"))?);
            record.content.activity.as_mut().unwrap().provider_call_id =
                Some(TypedKey::utf8(format!("commit-call-{index}"))?);
            record.validate_contract()?;
            Ok(record)
        })
        .collect::<TestResult<Vec<_>>>()?;
    records[0].content.activity.as_mut().unwrap().result = None;
    records[1].content.activity.as_mut().unwrap().invocation = None;
    for record in &mut records[..2] {
        record.content.activity.as_mut().unwrap().provider_call_id =
            Some(TypedKey::utf8("paired-call")?);
        record.validate_contract()?;
    }
    let mut source = source_state(&template, 0x65);
    source.event_count = records.len() as u64;
    let generation = head(0x65, std::slice::from_ref(&source))?;
    let root = directory.path().join("graph");
    let mut materializer = SegmentMaterializer::open(&root)?;
    let mut session = match materializer.start_core_generation(generation.clone())? {
        CoreGenerationStart::Started(session) => session,
        CoreGenerationStart::Current(_) => panic!("fresh generation"),
    };
    let reconciliations = session.reconcile_source_page(protocol(CoreSourceDeltaPage::new(
        "0".repeat(64),
        generation.core_generation_id.clone(),
        0,
        true,
        vec![CoreSourceDelta::Present(source.clone())],
    ))?)?;
    let page = |index, terminal, deltas| CoreEventDeltaPage {
        materialization_id: "0".repeat(64),
        core_generation_id: generation.core_generation_id.clone(),
        reconciliation: reconciliations[0].clone(),
        page_index: index,
        terminal,
        deltas,
    };
    let first = page(
        0,
        false,
        records[..2]
            .iter()
            .cloned()
            .map(CoreEventDelta::Added)
            .collect(),
    );
    force_aggregate_split(&mut session, &first)?;
    session.ingest_event_pages(vec![
        first,
        page(1, true, vec![CoreEventDelta::Added(records[2].clone())]),
    ])?;
    let receipt = session.activate()?;
    assert_eq!(receipt.event_count, 3);
    materializer
        .active
        .as_mut()
        .unwrap()
        .completed
        .coverage
        .bounded_omission_events = 1;
    let partial = materializer.projection_status(&StatusRequest {
        requested_core_generation_id: Some(generation.core_generation_id.clone()),
    })?;
    assert_eq!(partial.currentness, CoreProjectionCurrentness::Current);
    assert_eq!(
        partial.materialized_coverage,
        crate::protocol::MaterializedCoverage::Partial
    );
    assert_eq!(partial.coverage.bounded_omission_events, 1);
    assert!(partial.availability.commit_blame);
    drop(materializer);

    let pinned = crate::graph::segment::FlatStore::new(&root)
        .open_active(SegmentGraph::flat_open_policy())?;
    let graph = SegmentGraph::from_pinned(pinned, None);
    let files = (&graph).resolve(
        &ResourceSelector {
            kind: ResourceKind::File,
            value: "src/witness.rs".to_owned(),
            repository: None,
        },
        10,
    )?;
    assert_eq!(files.len(), 1);
    let commits = (&graph).resolve_commits(std::slice::from_ref(&oid), None, 10)?;
    assert_eq!(commits.len(), 1);
    let facts = (&graph).blame_facts_page(&commits[0].1.id, BlameFactFamily::Commit, None, 10)?;
    let cited_events = facts
        .items
        .iter()
        .flat_map(|(fact, _)| fact.citations.iter())
        .map(|citation| citation.0.event_id)
        .collect::<Vec<_>>();
    for record in &records[1..] {
        assert!(
            cited_events.contains(&record.event_id),
            "missing citation from {}",
            record.event_id
        );
    }
    drop(graph);

    let mut reopened = SegmentMaterializer::open(&root)?;
    let mut next_source = source;
    next_source.core_record_accumulator = hex::encode([0x66; 32]);
    let next = head(0x66, std::slice::from_ref(&next_source))?;
    let mut query = match reopened.start_core_generation(next.clone())? {
        CoreGenerationStart::Started(session) => session,
        CoreGenerationStart::Current(_) => panic!("next generation"),
    };
    let next_reconciliations = query.reconcile_source_page(protocol(CoreSourceDeltaPage::new(
        "0".repeat(64),
        next.core_generation_id,
        0,
        true,
        vec![CoreSourceDelta::Present(next_source)],
    ))?)?;
    let (states, terminal) = query.event_states(&next_reconciliations[0], None)?;
    assert!(terminal);
    assert_eq!(states.len(), 3);
    assert_eq!(
        states
            .iter()
            .map(|state| state.event_id)
            .collect::<Vec<_>>(),
        records
            .iter()
            .map(|record| record.event_id)
            .collect::<Vec<_>>()
    );
    Ok(())
}

#[test]
fn multi_page_batch_publishes_and_reopens_every_event_in_order() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("graph");
    let template = one_record()?;
    let records = [indexed_record(&template, 0)?, indexed_record(&template, 1)?];
    let mut source = source_state(&template, 0x61);
    source.event_count = 2;
    let generation = head(0x61, std::slice::from_ref(&source))?;
    let mut materializer = SegmentMaterializer::open(&root)?;
    let mut session = match materializer.start_core_generation(generation.clone())? {
        CoreGenerationStart::Started(session) => session,
        CoreGenerationStart::Current(_) => panic!("fresh generation"),
    };
    let reconciliations = session.reconcile_source_page(protocol(CoreSourceDeltaPage::new(
        "0".repeat(64),
        generation.core_generation_id.clone(),
        0,
        true,
        vec![CoreSourceDelta::Present(source.clone())],
    ))?)?;
    let pages = records
        .iter()
        .enumerate()
        .map(|(index, record)| CoreEventDeltaPage {
            materialization_id: "0".repeat(64),
            core_generation_id: generation.core_generation_id.clone(),
            reconciliation: reconciliations[0].clone(),
            page_index: index as u32,
            terminal: index == 1,
            deltas: vec![CoreEventDelta::Added(record.clone())],
        })
        .collect();
    session.ingest_event_pages(pages)?;
    let receipt = session.activate()?;
    assert_eq!(receipt.event_count, 2);
    assert_eq!(receipt.source_count, 1);
    drop(materializer);

    let mut reopened = SegmentMaterializer::open(&root)?;
    let status = reopened.projection_status(&StatusRequest {
        requested_core_generation_id: Some(generation.core_generation_id.clone()),
    })?;
    assert_eq!(status.currentness, CoreProjectionCurrentness::Current);
    assert_eq!(status.receipt, Some(receipt));
    let mut next_source = source;
    next_source.core_record_accumulator = hex::encode([0x62; 32]);
    let next_generation = head(0x62, std::slice::from_ref(&next_source))?;
    let mut query = match reopened.start_core_generation(next_generation.clone())? {
        CoreGenerationStart::Started(session) => session,
        CoreGenerationStart::Current(_) => panic!("successor generation"),
    };
    let next_reconciliations = query.reconcile_source_page(protocol(CoreSourceDeltaPage::new(
        "0".repeat(64),
        next_generation.core_generation_id,
        0,
        true,
        vec![CoreSourceDelta::Present(next_source)],
    ))?)?;
    let (states, terminal) = query.event_states(&next_reconciliations[0], None)?;
    assert!(terminal);
    assert_eq!(states.len(), 2);
    assert_eq!(states[0].event_id, records[0].event_id);
    assert_eq!(states[1].event_id, records[1].event_id);
    Ok(())
}

#[test]
fn ordinary_full_event_page_retains_all_events() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("graph");
    let template = one_record()?;
    let records = (0..crate::protocol::MAX_CORE_EVENT_DELTA_PAGE_ITEMS as u32)
        .map(|index| indexed_record(&template, index))
        .collect::<TestResult<Vec<_>>>()?;
    let mut source = source_state(&template, 0x67);
    source.event_count = records.len() as u64;
    let generation = head(0x67, std::slice::from_ref(&source))?;
    let mut materializer = SegmentMaterializer::open(&root)?;
    let mut session = match materializer.start_core_generation(generation.clone())? {
        CoreGenerationStart::Started(session) => session,
        CoreGenerationStart::Current(_) => panic!("fresh generation"),
    };
    let reconciliations = session.reconcile_source_page(protocol(CoreSourceDeltaPage::new(
        "0".repeat(64),
        generation.core_generation_id.clone(),
        0,
        true,
        vec![CoreSourceDelta::Present(source)],
    ))?)?;
    session.ingest_event_pages(vec![CoreEventDeltaPage {
        materialization_id: "0".repeat(64),
        core_generation_id: generation.core_generation_id,
        reconciliation: reconciliations[0].clone(),
        page_index: 0,
        terminal: true,
        deltas: records.iter().cloned().map(CoreEventDelta::Added).collect(),
    }])?;
    assert_eq!(session.activate()?.event_count, records.len() as u64);
    Ok(())
}

#[test]
fn later_invalid_page_aborts_earlier_staged_page() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("graph");
    let template = one_record()?;
    let records = [
        indexed_record(&template, 0)?,
        indexed_record(&template, 1)?,
        indexed_record(&template, 2)?,
    ];
    let mut source = source_state(&template, 0x63);
    source.event_count = 3;
    let generation = head(0x63, std::slice::from_ref(&source))?;
    let mut materializer = SegmentMaterializer::open(&root)?;
    let mut session = match materializer.start_core_generation(generation.clone())? {
        CoreGenerationStart::Started(session) => session,
        CoreGenerationStart::Current(_) => panic!("fresh generation"),
    };
    let reconciliations = session.reconcile_source_page(protocol(CoreSourceDeltaPage::new(
        "0".repeat(64),
        generation.core_generation_id.clone(),
        0,
        true,
        vec![CoreSourceDelta::Present(source)],
    ))?)?;
    let pages = [
        vec![
            CoreEventDelta::Added(records[0].clone()),
            CoreEventDelta::Added(records[1].clone()),
        ],
        vec![CoreEventDelta::Added(records[2].clone())],
    ]
    .into_iter()
    .map(|deltas| CoreEventDeltaPage {
        materialization_id: "0".repeat(64),
        core_generation_id: generation.core_generation_id.clone(),
        reconciliation: reconciliations[0].clone(),
        page_index: 0, // The second page cannot replay the first index.
        terminal: false,
        deltas,
    })
    .collect::<Vec<_>>();
    force_aggregate_split(&mut session, &pages[0])?;
    assert!(session.ingest_event_pages(pages).is_err());
    assert!(session.activate().is_err());
    drop(materializer);
    assert!(
        crate::graph::segment::SegmentStore::new(&root)
            .load_active()?
            .is_none()
    );
    Ok(())
}

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
