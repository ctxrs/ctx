use super::*;

#[test]
fn cumulative_message_indices_are_batch_boundary_and_prepublication_crash_independent() {
    let message = |id: &str| DeepAgentsMessage {
        role: EventRole::User,
        message_type: "human".to_owned(),
        message_class: None,
        message_id: Some(id.to_owned()),
        text: id.to_owned(),
    };
    let rows = vec![
        vec![message("A")],
        vec![message("A"), message("B")],
        vec![message("A"), message("B"), message("C")],
    ];

    let same_batch = cumulative_plan(&rows, &BTreeSet::new());
    let every_row_is_a_batch = cumulative_plan(&rows, &BTreeSet::from([1, 2]));

    // A crash before publication loses the transient ledger and replays from the certified
    // group frontier. Rebuilding from the same rows must therefore reproduce the same plan.
    let _discarded_precrash_prefix = cumulative_plan(&rows[..1], &BTreeSet::new());
    let replay_after_crash = cumulative_plan(&rows, &BTreeSet::from([1, 2]));

    let expected = (vec![(0, 0, 1), (1, 1, 2), (2, 2, 3)], 4);
    assert_eq!(same_batch, expected);
    assert_eq!(every_row_is_a_batch, expected);
    assert_eq!(replay_after_crash, expected);
}

#[test]
fn cumulative_message_indices_survive_a_published_batch_crash_boundary() {
    let source_conn = Connection::open_in_memory().unwrap();
    create_tables(&source_conn);
    insert_large_cumulative_writes(&source_conn);

    let directory = crate::test_support_paths::tempdir().unwrap();
    let mut store = Store::open(directory.path().join("store.sqlite")).unwrap();
    let context = context(Some(directory.path().join("sessions.db")));
    let source = test_source("published-crash-boundary");
    let stream = captured_batch_cursor_stream(&source);
    let initial = initial_deepagents_position().unwrap();
    let admission = CapturedSourceAdmission::conversation_for_context(&source, &context).unwrap();

    let committed_store = Store::open_read_only(store.path()).unwrap();
    let fetcher = Rc::new(RefCell::new(
        DeepAgentsRowFetcher::new(&source_conn, context.clone(), Some(committed_store)).unwrap(),
    ));
    let producer_fetcher = Rc::clone(&fetcher);
    let mut producer =
        SqliteLogicalRowBatchProducer::new(source.clone(), initial.clone(), move |position| {
            producer_fetcher.borrow_mut().fetch(position)
        });
    let mut projector = DeepAgentsCapturedBatchProjector {
        context: context.clone(),
        raw_source_path: context
            .source_path
            .as_ref()
            .map(|path| path.display().to_string()),
        user_version: 0,
        schema_fingerprint: "schema:test".to_owned(),
        source_revision: source.source_revision().to_owned(),
        committed_store: Some(Store::open_read_only(store.path()).unwrap()),
    };
    let first = import_captured_batches(
        &mut store,
        &admission,
        NormalizedProviderImportOptions::default(),
        &context.machine_id,
        context.imported_at,
        None,
        &initial,
        CapturedBatchCursorMode::Resume,
        NonZeroUsize::new(1).unwrap(),
        &mut projector,
        || {
            with_sqlite_read_snapshot(&source_conn, || {
                producer.next_batch().map_err(deepagents_sqlite_batch_error)
            })
        },
        || Ok(true),
    )
    .unwrap();
    assert_eq!(first.batches_imported, 1);
    assert!(!first.source_exhausted);
    assert_eq!(first.summary.imported_events, 1);
    drop(producer);
    drop(fetcher);

    let published_cursor = store
        .get_sync_cursor(None, &context.machine_id, &stream)
        .unwrap()
        .unwrap();
    let certified = CertifiedProviderCursor::decode_if_certified(&published_cursor.cursor)
        .unwrap()
        .unwrap();
    let resume = certified.native_position().clone();

    // Simulate process death after the first raw batch and its cursor publication. The new
    // fetcher has no transient row ledger; it must combine the certified event-index frontier
    // with the already committed stable message identity.
    let committed_store = Store::open_read_only(store.path()).unwrap();
    let fetcher = Rc::new(RefCell::new(
        DeepAgentsRowFetcher::new(&source_conn, context.clone(), Some(committed_store)).unwrap(),
    ));
    let producer_fetcher = Rc::clone(&fetcher);
    let mut producer =
        SqliteLogicalRowBatchProducer::new(source.clone(), resume, move |position| {
            producer_fetcher.borrow_mut().fetch(position)
        });
    let mut projector = DeepAgentsCapturedBatchProjector {
        context: context.clone(),
        raw_source_path: context
            .source_path
            .as_ref()
            .map(|path| path.display().to_string()),
        user_version: 0,
        schema_fingerprint: "schema:test".to_owned(),
        source_revision: source.source_revision().to_owned(),
        committed_store: Some(Store::open_read_only(store.path()).unwrap()),
    };
    let batch_requests = Cell::new(0_usize);
    let resumed = import_captured_batches(
        &mut store,
        &admission,
        NormalizedProviderImportOptions::default(),
        &context.machine_id,
        context.imported_at,
        Some(&published_cursor),
        &initial,
        CapturedBatchCursorMode::Resume,
        NonZeroUsize::new(CAPTURE_BATCH_MAX_BATCHES_PER_GROUP).unwrap(),
        &mut projector,
        || {
            if batch_requests.get() > 0 {
                fetcher.borrow_mut().reset_for_batch_request();
            }
            batch_requests.set(batch_requests.get() + 1);
            with_sqlite_read_snapshot(&source_conn, || {
                producer.next_batch().map_err(deepagents_sqlite_batch_error)
            })
        },
        || Ok(true),
    )
    .unwrap();
    assert!(resumed.source_exhausted);
    assert_eq!(resumed.summary.imported_events, 2);

    let session = store
        .session_by_external_session(CaptureProvider::DeepAgents, "thread-a")
        .unwrap()
        .unwrap();
    let events = store.events_for_session(session.id).unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(
        events
            .iter()
            .map(|event| event.payload["provider_event_index"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
}

#[test]
fn cumulative_message_indices_converge_after_store_prefix_commit_without_cursor() {
    let source_conn = Connection::open_in_memory().unwrap();
    create_tables(&source_conn);
    insert_large_cumulative_writes(&source_conn);
    let directory = crate::test_support_paths::tempdir().unwrap();
    let mut store = Store::open(directory.path().join("store.sqlite")).unwrap();
    let context = context(Some(directory.path().join("sessions.db")));
    let source = test_source("partial-store-prefix");
    let stream = captured_batch_cursor_stream(&source);
    let initial = initial_deepagents_position().unwrap();
    let baseline_batches = produce_all(
        &source_conn,
        source.clone(),
        initial.clone(),
        context.clone(),
    );
    let expected_end = baseline_batches.last().unwrap().range_end().clone();
    let mut baseline_projector = DeepAgentsCapturedBatchProjector {
        context: context.clone(),
        raw_source_path: context
            .source_path
            .as_ref()
            .map(|path| path.display().to_string()),
        user_version: 0,
        schema_fingerprint: "schema:test".to_owned(),
        source_revision: source.source_revision().to_owned(),
        committed_store: None,
    };
    let mut baseline_output = CollectingProjectionOutput::default();
    for batch in &baseline_batches {
        for record in batch.records() {
            baseline_projector
                .project_record(record, &mut baseline_output)
                .unwrap();
        }
    }
    let committed_prefix = baseline_output
        .normalizations
        .into_iter()
        .flat_map(|normalization| normalization.captures)
        .filter(|(_, capture)| {
            capture
                .event
                .as_ref()
                .is_some_and(|event| event.provider_event_index <= 2)
        })
        .collect::<Vec<_>>();
    assert_eq!(committed_prefix.len(), 2);
    let prefix_summary = crate::import_normalized_provider_captures(
        &mut store,
        ProviderNormalizationResult {
            captures: committed_prefix,
            ..ProviderNormalizationResult::default()
        },
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(prefix_summary.imported_events, 2);
    assert!(store
        .get_sync_cursor(None, &context.machine_id, &stream)
        .unwrap()
        .is_none());

    // This is the crash shape: normalized transactions for A/B are visible, but the durable
    // captured-batch cursor still names the initial group frontier. Replay must skip A/B by
    // stable identity, emit C at legacy index 3, and publish the exact baseline end cursor.
    let committed_store = Store::open_read_only(store.path()).unwrap();
    let fetcher = Rc::new(RefCell::new(
        DeepAgentsRowFetcher::new(&source_conn, context.clone(), Some(committed_store)).unwrap(),
    ));
    let producer_fetcher = Rc::clone(&fetcher);
    let mut producer =
        SqliteLogicalRowBatchProducer::new(source.clone(), initial.clone(), move |position| {
            producer_fetcher.borrow_mut().fetch(position)
        });
    let mut projector = DeepAgentsCapturedBatchProjector {
        context: context.clone(),
        raw_source_path: context
            .source_path
            .as_ref()
            .map(|path| path.display().to_string()),
        user_version: 0,
        schema_fingerprint: "schema:test".to_owned(),
        source_revision: source.source_revision().to_owned(),
        committed_store: Some(Store::open_read_only(store.path()).unwrap()),
    };
    let batch_requests = Cell::new(0_usize);
    let admission = CapturedSourceAdmission::conversation_for_context(&source, &context).unwrap();
    let replay = import_captured_batches(
        &mut store,
        &admission,
        NormalizedProviderImportOptions::default(),
        &context.machine_id,
        context.imported_at,
        None,
        &initial,
        CapturedBatchCursorMode::Resume,
        NonZeroUsize::new(CAPTURE_BATCH_MAX_BATCHES_PER_GROUP).unwrap(),
        &mut projector,
        || {
            if batch_requests.get() > 0 {
                fetcher.borrow_mut().reset_for_batch_request();
            }
            batch_requests.set(batch_requests.get() + 1);
            with_sqlite_read_snapshot(&source_conn, || {
                producer.next_batch().map_err(deepagents_sqlite_batch_error)
            })
        },
        || Ok(true),
    )
    .unwrap();
    assert!(replay.source_exhausted);
    assert_eq!(replay.summary.imported_events, 1);
    let published = store
        .get_sync_cursor(None, &context.machine_id, &stream)
        .unwrap()
        .unwrap();
    let published = CertifiedProviderCursor::decode_if_certified(&published.cursor)
        .unwrap()
        .unwrap();
    assert_eq!(published.native_position(), &expected_end);
    let session = store
        .session_by_external_session(CaptureProvider::DeepAgents, "thread-a")
        .unwrap()
        .unwrap();
    assert_eq!(
        store
            .events_for_session(session.id)
            .unwrap()
            .iter()
            .map(|event| event.payload["provider_event_index"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
}
