#[test]
fn existing_session_event_requires_an_event_and_rolls_back_parent_projection() {
    let temp = tempdir().expect("tempdir");
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let batch = test_batch("source-revision-eventless-existing-session", 0, &[0, 1]);
    let parent_capture = projected_pi_capture();
    let mut eventless = parent_capture.clone();
    eventless.event = None;
    let mut projector = ExistingSessionEventProjector {
        projections: VecDeque::from([(false, parent_capture), (true, eventless)]),
    };

    let error = import_captured_batch(
        &mut store,
        &CapturedSourceAdmission::conversation_without_cross_record_relationships(batch.source()),
        &batch,
        NormalizedProviderImportOptions::default(),
        TEST_MACHINE_ID,
        observed_at(),
        None,
        &test_position(0),
        CapturedBatchCursorMode::Resume,
        &mut projector,
        || Ok(true),
    )
    .expect_err("eventless existing-session projection must fail closed");

    assert!(matches!(error, CaptureError::SystemInvariant(_)));
    let archive = store.export_archive().expect("export archive");
    assert!(archive.capture_sources.is_empty());
    assert!(archive.sessions.is_empty());
    assert!(archive.events.is_empty());
    assert!(store
        .get_sync_cursor(
            None,
            TEST_MACHINE_ID,
            &captured_batch_cursor_stream(batch.source()),
        )
        .expect("read cursor")
        .is_none());
}

#[test]
fn existing_session_event_must_match_the_admitted_source_scope() {
    let temp = tempdir().expect("tempdir");
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let batch = test_batch("source-revision-existing-session-scope", 0, &[0, 1]);
    let parent_capture = projected_pi_capture();
    let mut wrong_machine_event = parent_capture.clone();
    wrong_machine_event.source.machine_id = "wrong-machine".to_owned();
    wrong_machine_event
        .event
        .as_mut()
        .expect("fixture event")
        .provider_event_index = 2;
    let mut projector = ExistingSessionEventProjector {
        projections: VecDeque::from([(false, parent_capture), (true, wrong_machine_event)]),
    };

    let error = import_captured_batch(
        &mut store,
        &CapturedSourceAdmission::conversation_without_cross_record_relationships(batch.source()),
        &batch,
        NormalizedProviderImportOptions::default(),
        TEST_MACHINE_ID,
        observed_at(),
        None,
        &test_position(0),
        CapturedBatchCursorMode::Resume,
        &mut projector,
        || Ok(true),
    )
    .expect_err("cross-source existing-session event must fail closed");

    assert!(matches!(error, CaptureError::SystemInvariant(_)));
    let archive = store.export_archive().expect("export archive");
    assert!(archive.capture_sources.is_empty());
    assert!(archive.sessions.is_empty());
    assert!(archive.events.is_empty());
}

#[test]
fn repeated_session_captures_merge_out_of_order_temporal_bounds() {
    let temp = tempdir().expect("tempdir");
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let import_batch =
        test_batch("source-revision-temporal-merge", 0, &[0, 1]).into_source_exhausted();
    let mut later = projected_pi_capture();
    later.session.provider_session_id = "out-of-order-session".to_owned();
    later.session.metadata["temporal_marker"] = json!("first-envelope");
    let baseline = later.session.started_at;
    later.session.started_at = baseline + chrono::Duration::hours(1);
    later.session.ended_at = Some(baseline + chrono::Duration::hours(2));
    let mut earlier = later.clone();
    earlier.session.metadata["temporal_marker"] = json!("later-envelope");
    earlier.session.started_at = baseline - chrono::Duration::hours(1);
    earlier.session.ended_at = Some(baseline + chrono::Duration::hours(3));

    let admission = CapturedSourceAdmission::conversation_without_cross_record_relationships(
        import_batch.source(),
    );
    let mut projector = QueuedCaptureProjector {
        captures: VecDeque::from([later, earlier]),
    };
    let mut batches = VecDeque::from([import_batch]);
    let imported = import_captured_batches(
        &mut store,
        &admission,
        NormalizedProviderImportOptions::default(),
        TEST_MACHINE_ID,
        observed_at(),
        None,
        &test_position(0),
        CapturedBatchCursorMode::Resume,
        NonZeroUsize::new(4).expect("nonzero group limit"),
        &mut projector,
        || Ok(batches.pop_front()),
        || Ok(true),
    )
    .expect("import out-of-order session captures");

    assert_eq!(imported.summary.imported_sessions, 1);
    let session = store
        .session_by_external_session(CaptureProvider::Pi, "out-of-order-session")
        .expect("read session")
        .expect("session");
    assert_eq!(session.started_at, baseline - chrono::Duration::hours(1));
    assert_eq!(
        session.ended_at,
        Some(baseline + chrono::Duration::hours(3))
    );
    assert_eq!(
        session.sync.metadata["metadata"]["temporal_marker"],
        "first-envelope"
    );
}

#[test]
fn unresolved_relationship_placeholder_is_eventless_and_source_scoped() {
    let temp = tempdir().expect("tempdir");
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let batch = test_batch("source-revision-1", 0, &[0]).into_source_exhausted();
    let mut child = projected_pi_capture();
    child.session.provider_session_id = "dangling-child".to_owned();
    child.session.parent_provider_session_id = Some("dangling-parent".to_owned());
    child.session.root_provider_session_id = Some("dangling-parent".to_owned());
    let mut projector = QueuedCaptureProjector {
        captures: VecDeque::from([child]),
    };
    let source = batch.source().clone();
    let admission =
        CapturedSourceAdmission::conversation_without_cross_record_relationships(&source);
    let mut batches = VecDeque::from([batch]);

    import_captured_batches(
        &mut store,
        &admission,
        NormalizedProviderImportOptions::default(),
        TEST_MACHINE_ID,
        observed_at(),
        None,
        &test_position(0),
        CapturedBatchCursorMode::Resume,
        NonZeroUsize::new(4).expect("nonzero group limit"),
        &mut projector,
        || Ok(batches.pop_front()),
        || Ok(true),
    )
    .expect("import unresolved child relationship");

    let parent = store
        .session_by_external_session(CaptureProvider::Pi, "dangling-parent")
        .expect("read parent placeholder")
        .expect("parent placeholder");
    let child = store
        .session_by_external_session(CaptureProvider::Pi, "dangling-child")
        .expect("read child")
        .expect("child session");
    assert_eq!(parent.agent_type, AgentType::Unknown);
    assert_eq!(parent.status, SessionStatus::Imported);
    assert_eq!(parent.sync.fidelity, Fidelity::Partial);
    assert_eq!(
        parent
            .sync
            .metadata
            .get("relationship_placeholder")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert_eq!(parent.parent_session_id, None);
    assert_eq!(parent.root_session_id, None);
    assert_eq!(parent.capture_source_id, None);
    assert_eq!(child.parent_session_id, Some(parent.id));
    assert_eq!(child.root_session_id, Some(parent.id));
    let archive = store.export_archive().expect("export archive");
    assert_eq!(archive.events.len(), 1);
    let source_identity = parent
        .sync
        .metadata
        .get("source_identity")
        .and_then(serde_json::Value::as_str)
        .expect("placeholder source identity");
    let edge_id = provider_import_edge_uuid(
        CaptureProvider::Pi,
        "dangling-child",
        Some(source_identity),
        child.id,
        "parent_child",
    );
    assert!(store
        .session_edge_exists(edge_id)
        .expect("read parent-child edge"));
}

#[test]
fn projection_output_rejects_source_sized_normalization_vectors() {
    let temp = tempdir().expect("tempdir");
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let batch = test_batch("source-revision-1", 0, &[0]);
    let mut projector = MultiCaptureProjector {
        capture: projected_pi_capture(),
        cursor_position: batch.range_end().clone(),
    };

    let error = import_captured_batch(
        &mut store,
        &CapturedSourceAdmission::conversation_without_cross_record_relationships(batch.source()),
        &batch,
        NormalizedProviderImportOptions::default(),
        TEST_MACHINE_ID,
        observed_at(),
        None,
        &test_position(0),
        CapturedBatchCursorMode::Resume,
        &mut projector,
        || Ok(true),
    )
    .expect_err("projectors must stream normalized captures one unit at a time");

    assert!(matches!(error, CaptureError::SystemInvariant(_)));
    assert!(store
        .get_sync_cursor(
            None,
            TEST_MACHINE_ID,
            &captured_batch_cursor_stream(batch.source()),
        )
        .expect("read cursor")
        .is_none());
}

#[test]
fn accepted_events_persist_exact_source_record_coordinates() {
    let temp = tempdir().expect("tempdir");
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let batch = test_batch("source-revision-local-coordinates", 7, &[7]);
    let first = projected_pi_capture();
    let mut second = first.clone();
    let second_event = second.event.as_mut().expect("second event");
    second_event.provider_event_index = 2;
    second_event.provider_event_hash = Some("captured-batch-message-2".to_owned());
    second_event.idempotency_key = Some("captured-batch-message-2".to_owned());
    second_event.metadata["entry_id"] = json!("captured-batch-message-2");
    second_event.metadata["provider_event_identity_index"] = json!(2);
    second_event.metadata["legacy_provider_event_index"] = json!(2);
    second_event.payload["text"] = json!("second");
    let mut projector = StreamingMultiEventProjector {
        captures: vec![first, second],
    };

    import_captured_batch(
        &mut store,
        &CapturedSourceAdmission::conversation_without_cross_record_relationships(batch.source()),
        &batch,
        NormalizedProviderImportOptions::default(),
        TEST_MACHINE_ID,
        observed_at(),
        None,
        &test_position(7),
        CapturedBatchCursorMode::Resume,
        &mut projector,
        || Ok(true),
    )
    .expect("import multi-event record");

    let mut events = store.export_archive().expect("export archive").events;
    events.sort_by_key(|event| event.seq);
    assert_eq!(events.len(), 2);
    for (subrecord, event) in events.iter().enumerate() {
        assert!(event
            .sync
            .metadata
            .get(crate::complete_content::COMPLETE_CONTENT_LOCATOR_METADATA_KEY)
            .is_none());
        assert_eq!(event.sync.metadata["source_record_ordinal"], 7);
        assert_eq!(
            event.sync.metadata["source_record_subrecord_index"],
            u64::try_from(subrecord).expect("subrecord")
        );
        let provider_metadata = &event.sync.metadata["metadata"];
        assert!(provider_metadata.get("source_record_ordinal").is_none());
        assert!(provider_metadata
            .get("source_record_subrecord_index")
            .is_none());
    }
}

#[test]
fn explicit_file_touch_declaration_suppresses_automatic_duplicates() {
    let temp = tempdir().expect("tempdir");
    let mut automatic_store =
        Store::open(temp.path().join("automatic.sqlite")).expect("open automatic store");
    let mut explicit_store =
        Store::open(temp.path().join("explicit.sqlite")).expect("open explicit store");
    let batch = test_batch("source-revision-explicit-touches", 0, &[0]);
    let admission =
        CapturedSourceAdmission::conversation_without_cross_record_relationships(batch.source());
    let mut capture = projected_pi_capture();
    let event = capture.event.as_mut().expect("fixture event");
    event.event_type = EventType::FileTouched;
    event.payload = json!({
        "files": [
            {"path": "/workspace/one.rs"},
            {"path": "/workspace/two.rs"}
        ]
    });
    let (touches, outcome) = provider_file_touches_from_event(
        capture.provider,
        &capture.session.provider_session_id,
        &capture.source.source_format,
        capture.source.raw_source_path.as_deref(),
        capture.source.source_root.as_deref(),
        event,
        1,
    )
    .into_parts();
    assert!(!outcome.limit_exceeded());
    assert_eq!(touches.len(), 2);
    let mut automatic = CaptureProjector {
        capture: capture.clone(),
        cursor_position: batch.range_end().clone(),
    };
    let automatic_outcome = import_captured_batch(
        &mut automatic_store,
        &admission,
        &batch,
        NormalizedProviderImportOptions::default(),
        TEST_MACHINE_ID,
        observed_at(),
        None,
        &test_position(0),
        CapturedBatchCursorMode::Resume,
        &mut automatic,
        || Ok(true),
    )
    .expect("automatic touches");

    let mut explicit = ExplicitTouchProjector { capture, touches };
    let explicit_outcome = import_captured_batch(
        &mut explicit_store,
        &admission,
        &batch,
        NormalizedProviderImportOptions::default(),
        TEST_MACHINE_ID,
        observed_at(),
        None,
        &test_position(0),
        CapturedBatchCursorMode::Resume,
        &mut explicit,
        || Ok(true),
    )
    .expect("explicit touches");

    assert_eq!(
        explicit_outcome.summary.accepted_content_records,
        automatic_outcome.summary.accepted_content_records,
        "explicit touch units must replace, not duplicate, automatic inference"
    );
}

#[test]
fn invalid_normalized_event_is_a_deterministic_cursor_advancing_rejection() {
    let temp = tempdir().expect("tempdir");
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let batch = test_batch("source-revision-1", 0, &[0]);
    let mut capture = projected_pi_capture();
    let event = capture.event.as_mut().expect("Pi event");
    event.event_type = EventType::CommandOutput;
    event.payload["duration_ms"] = json!(-1);
    let mut projector = CaptureProjector {
        capture,
        cursor_position: batch.range_end().clone(),
    };

    let outcome = import_captured_batch(
        &mut store,
        &CapturedSourceAdmission::conversation_without_cross_record_relationships(batch.source()),
        &batch,
        NormalizedProviderImportOptions::default(),
        TEST_MACHINE_ID,
        observed_at(),
        None,
        &test_position(0),
        CapturedBatchCursorMode::Resume,
        &mut projector,
        || Ok(true),
    )
    .expect("deterministic validation rejection advances cursor");

    assert_eq!(outcome.summary.failed, 1);
    assert_eq!(outcome.summary.failures.len(), 1);
    assert!(outcome.summary.failures[0]
        .error
        .contains("duration_ms must be nonnegative"));
    assert!(store
        .get_sync_cursor(
            None,
            TEST_MACHINE_ID,
            &captured_batch_cursor_stream(batch.source()),
        )
        .expect("read cursor")
        .is_some());
}
