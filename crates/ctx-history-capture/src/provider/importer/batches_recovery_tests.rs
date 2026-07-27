#[test]
fn unsafe_four_batch_group_continues_to_safe_fifth_without_prefetch() {
    let temp = tempdir().expect("tempdir");
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let mut batches = VecDeque::from(
        (0_u64..5)
            .map(|ordinal| test_batch("source-revision-safe-fifth", ordinal, &[ordinal]))
            .collect::<Vec<_>>(),
    );
    let source = batches.front().expect("first batch").source().clone();
    let admission =
        CapturedSourceAdmission::conversation_without_cross_record_relationships(&source);
    let requests = Cell::new(0_usize);
    let revalidation_requests = RefCell::new(Vec::new());
    let mut projector = RetainingProjector {
        advance_at_ordinal: 4,
        seen_ordinals: Vec::new(),
        capture: None,
        reject_records: true,
    };

    let outcome = import_captured_batches(
        &mut store,
        &admission,
        NormalizedProviderImportOptions::default(),
        TEST_MACHINE_ID,
        observed_at(),
        None,
        &test_position(0),
        CapturedBatchCursorMode::Resume,
        NonZeroUsize::new(CAPTURE_BATCH_MAX_BATCHES_PER_GROUP).expect("group limit"),
        &mut projector,
        || {
            requests.set(requests.get().saturating_add(1));
            Ok(batches.pop_front())
        },
        || {
            revalidation_requests.borrow_mut().push(requests.get());
            Ok(true)
        },
    )
    .expect("unsafe group continues to the next safe boundary");

    assert_eq!(outcome.batches_imported, 5);
    assert!(!outcome.source_exhausted);
    assert!(outcome.cursor_safe);
    assert_eq!(outcome.summary.failed, 5);
    assert_eq!(
        requests.get(),
        5,
        "the producer must not even request batch six"
    );
    assert_eq!(&*revalidation_requests.borrow(), &[4, 5]);
    assert_eq!(projector.seen_ordinals, vec![0, 1, 2, 3, 4]);
    let stored = store
        .get_sync_cursor(None, TEST_MACHINE_ID, source.cursor_stream())
        .expect("read cursor")
        .expect("safe cursor");
    let certified = CertifiedProviderCursor::decode(&stored.cursor).expect("decode cursor");
    assert_eq!(certified.native_position(), &test_position(5));
    assert_eq!(certified.rejected_records(), 5);
}

#[test]
fn exhausted_unsafe_prefix_fails_closed_and_replays_idempotently() {
    let temp = tempdir().expect("tempdir");
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let batch = test_batch("source-revision-unsafe-eof", 0, &[0]).into_source_exhausted();
    let source = batch.source().clone();
    let admission =
        CapturedSourceAdmission::conversation_without_cross_record_relationships(&source);
    let mut first_batches = VecDeque::from([batch]);
    let capture = projected_pi_capture();
    let mut unsafe_projector = RetainingProjector {
        advance_at_ordinal: 9,
        seen_ordinals: Vec::new(),
        capture: Some(capture.clone()),
        reject_records: true,
    };

    let error = import_captured_batches(
        &mut store,
        &admission,
        NormalizedProviderImportOptions::default(),
        TEST_MACHINE_ID,
        observed_at(),
        None,
        &test_position(0),
        CapturedBatchCursorMode::Resume,
        NonZeroUsize::new(CAPTURE_BATCH_MAX_BATCHES_PER_GROUP).expect("group limit"),
        &mut unsafe_projector,
        || Ok(first_batches.pop_front()),
        || Ok(true),
    )
    .expect_err("EOF cannot report success while the parser-safe cursor lags");
    assert!(matches!(error, CaptureError::SystemInvariant(_)));

    assert!(store
        .get_sync_cursor(None, TEST_MACHINE_ID, source.cursor_stream())
        .expect("read unpublished cursor")
        .is_none());
    assert_eq!(store.list_sessions().expect("list sessions").len(), 1);

    let mut replay_batches =
        VecDeque::from([test_batch("source-revision-unsafe-eof", 0, &[0]).into_source_exhausted()]);
    let mut safe_projector = RetainingProjector {
        advance_at_ordinal: 0,
        seen_ordinals: Vec::new(),
        capture: Some(capture),
        reject_records: true,
    };
    let replay = import_captured_batches(
        &mut store,
        &admission,
        NormalizedProviderImportOptions::default(),
        TEST_MACHINE_ID,
        observed_at(),
        None,
        &test_position(0),
        CapturedBatchCursorMode::Resume,
        NonZeroUsize::new(CAPTURE_BATCH_MAX_BATCHES_PER_GROUP).expect("group limit"),
        &mut safe_projector,
        || Ok(replay_batches.pop_front()),
        || Ok(true),
    )
    .expect("committed prefix replays idempotently to a safe cursor");
    assert!(replay.cursor_safe);
    assert_eq!(replay.summary.failed, 1);
    let sessions = store.list_sessions().expect("list replayed sessions");
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        store
            .events_for_session(sessions[0].id)
            .expect("list replayed events")
            .len(),
        1
    );
    let published = store
        .get_sync_cursor(None, TEST_MACHINE_ID, source.cursor_stream())
        .expect("read safe replay cursor")
        .expect("safe replay cursor");
    let certified = CertifiedProviderCursor::decode(&published.cursor).expect("decode cursor");
    assert_eq!(certified.native_position(), &test_position(1));
    assert_eq!(certified.rejected_records(), 1);
}

#[test]
fn empty_sources_publish_and_replace_local_cursors() {
    let temp = tempdir().expect("tempdir");
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let first = test_batch("source-revision-empty-1", 0, &[0]);
    let first_source = first.source().clone();
    let first_admission =
        CapturedSourceAdmission::conversation_without_cross_record_relationships(&first_source);
    let mut projector = BatchEndRejectingProjector {
        seen_ordinals: Vec::new(),
    };
    let revalidations = Cell::new(0_usize);
    let fresh = import_captured_batches(
        &mut store,
        &first_admission,
        NormalizedProviderImportOptions::default(),
        TEST_MACHINE_ID,
        observed_at(),
        None,
        &test_position(0),
        CapturedBatchCursorMode::Resume,
        NonZeroUsize::new(1).expect("group limit"),
        &mut projector,
        || Ok(None),
        || {
            revalidations.set(revalidations.get().saturating_add(1));
            Ok(true)
        },
    )
    .expect("fresh empty source commits an initial cursor");
    assert!(fresh.source_exhausted);
    assert!(fresh.cursor_safe);
    assert_eq!(revalidations.get(), 1);
    let stored = store
        .get_sync_cursor(None, TEST_MACHINE_ID, first_source.cursor_stream())
        .expect("read fresh empty cursor")
        .expect("fresh empty cursor");
    let certified = CertifiedProviderCursor::decode(&stored.cursor).expect("decode cursor");
    assert_eq!(certified.source_revision(), "source-revision-empty-1");
    assert_eq!(certified.native_position(), &test_position(0));

    let unchanged = stored.clone();
    let noop = import_captured_batches(
        &mut store,
        &first_admission,
        NormalizedProviderImportOptions::default(),
        TEST_MACHINE_ID,
        observed_at(),
        Some(&stored),
        &test_position(0),
        CapturedBatchCursorMode::Resume,
        NonZeroUsize::new(1).expect("group limit"),
        &mut projector,
        || Ok(None),
        || Ok(true),
    )
    .expect("empty no-op preserves the prior cursor");
    assert!(noop.cursor_safe);
    let after_noop = store
        .get_sync_cursor(None, TEST_MACHINE_ID, first_source.cursor_stream())
        .expect("read no-op cursor")
        .expect("no-op cursor");
    assert_eq!(after_noop, unchanged);

    let replacement = test_batch("source-revision-empty-2", 0, &[0]);
    let replacement_source = replacement.source().clone();
    let replacement_admission =
        CapturedSourceAdmission::conversation_without_cross_record_relationships(
            &replacement_source,
        );
    let reset = import_captured_batches(
        &mut store,
        &replacement_admission,
        NormalizedProviderImportOptions::default(),
        TEST_MACHINE_ID,
        observed_at(),
        Some(&after_noop),
        &test_position(0),
        CapturedBatchCursorMode::ResetChangedSource,
        NonZeroUsize::new(1).expect("group limit"),
        &mut projector,
        || Ok(None),
        || Ok(true),
    )
    .expect("empty replacement commits its exact initial cursor");
    assert!(reset.cursor_safe);
    let replaced = store
        .get_sync_cursor(None, TEST_MACHINE_ID, replacement_source.cursor_stream())
        .expect("read replacement cursor")
        .expect("replacement cursor");
    let certified = CertifiedProviderCursor::decode(&replaced.cursor).expect("decode cursor");
    assert_eq!(certified.source_revision(), "source-revision-empty-2");
    assert_eq!(certified.native_position(), &test_position(0));
}

#[test]
fn pinned_reader_does_not_block_cursor_or_next_group_admission() {
    let temp = tempdir().expect("tempdir");
    let store_path = temp.path().join("work.sqlite");
    let mut store =
        Store::open_with_busy_timeout(&store_path, Duration::from_millis(10)).expect("open store");
    let reader = Connection::open(&store_path).expect("open reader");
    reader.execute_batch("BEGIN").expect("begin read");
    let _: i64 = reader
        .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .expect("pin read snapshot");

    let mut batches = VecDeque::from(
        (0..=CAPTURE_BATCH_MAX_BATCHES_PER_GROUP as u64)
            .map(|ordinal| {
                let batch = test_batch("source-revision-pinned-group", ordinal, &[ordinal]);
                if ordinal == CAPTURE_BATCH_MAX_BATCHES_PER_GROUP as u64 {
                    batch.into_source_exhausted()
                } else {
                    batch
                }
            })
            .collect::<Vec<_>>(),
    );
    let source = batches.front().expect("first batch").source().clone();
    let admission =
        CapturedSourceAdmission::conversation_without_cross_record_relationships(&source);
    let admitted = Cell::new(0_usize);
    let revalidations = Cell::new(0_usize);
    let mut projector = BatchEndRejectingProjector {
        seen_ordinals: Vec::new(),
    };

    let summary = drain_captured_batches(
        &mut store,
        &admission,
        NormalizedProviderImportOptions::default(),
        TEST_MACHINE_ID,
        observed_at(),
        None,
        &test_position(0),
        CapturedBatchCursorMode::Resume,
        source.cursor_stream(),
        &mut projector,
        || {
            let batch = batches.pop_front();
            if batch.is_some() {
                admitted.set(admitted.get().saturating_add(1));
            }
            Ok(batch)
        },
        || {
            revalidations.set(revalidations.get().saturating_add(1));
            Ok(true)
        },
    )
    .expect("pinned readers do not block durable maintenance handoff or cursor publication");

    assert_eq!(summary.failed, 5);
    assert_eq!(admitted.get(), 5);
    assert!(batches.is_empty());
    assert_eq!(revalidations.get(), 2);
    let stored = store
        .get_sync_cursor(
            None,
            TEST_MACHINE_ID,
            &captured_batch_cursor_stream(&source),
        )
        .expect("read cursor")
        .expect("published cursor");
    let certified = CertifiedProviderCursor::decode(&stored.cursor).expect("decode cursor");
    assert_eq!(certified.native_position(), &test_position(5));
    assert_eq!(certified.rejected_records(), 5);

    reader.execute_batch("ROLLBACK").expect("release reader");
}

#[test]
fn source_change_rolls_back_cursor_publication() {
    let temp = tempdir().expect("tempdir");
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let batch = test_batch("source-revision-1", 0, &[0]);
    let mut projector = RejectingProjector {
        seen_ordinals: Vec::new(),
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
        || Ok(false),
    )
    .expect_err("changed source cannot publish");

    assert!(matches!(error, CaptureError::SourceChangedDuringCapture));
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
fn stale_initial_compare_and_set_keeps_the_winning_cursor() {
    let temp = tempdir().expect("tempdir");
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let batch = test_batch("source-revision-1", 0, &[0]);
    let winning = cursor_for_batch(&batch, test_position(99));
    store
        .upsert_sync_cursor(&winning)
        .expect("seed competing cursor");
    let mut projector = RejectingProjector {
        seen_ordinals: Vec::new(),
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
    .expect_err("conflict is a typed error");

    assert!(matches!(error, CaptureError::ProviderCursorConflict));
    let stored = store
        .get_sync_cursor(
            None,
            TEST_MACHINE_ID,
            &captured_batch_cursor_stream(batch.source()),
        )
        .expect("read cursor")
        .expect("winning cursor remains");
    assert_eq!(stored.cursor, winning.cursor);
}

#[test]
fn projector_cannot_publish_a_cursor_before_or_after_the_batch_end() {
    let temp = tempdir().expect("tempdir");
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let batch = test_batch("source-revision-1", 0, &[0]);
    let mut projector = RejectingProjector {
        seen_ordinals: Vec::new(),
        cursor_position: test_position(2),
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
    .expect_err("cursor must equal exact batch end");

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
fn resume_rejects_a_changed_source_revision_even_at_the_expected_position() {
    let temp = tempdir().expect("tempdir");
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let prior_batch = test_batch("source-revision-1", 0, &[0]);
    let prior = cursor_for_batch(&prior_batch, test_position(1));
    store.upsert_sync_cursor(&prior).expect("seed prior cursor");
    let changed_batch = test_batch("source-revision-2", 1, &[1]);
    let mut projector = RejectingProjector {
        seen_ordinals: Vec::new(),
        cursor_position: changed_batch.range_end().clone(),
    };

    let error = import_captured_batch(
        &mut store,
        &CapturedSourceAdmission::conversation_without_cross_record_relationships(
            changed_batch.source(),
        ),
        &changed_batch,
        NormalizedProviderImportOptions::default(),
        TEST_MACHINE_ID,
        observed_at(),
        Some(&prior),
        &test_position(0),
        CapturedBatchCursorMode::Resume,
        &mut projector,
        || Ok(true),
    )
    .expect_err("rewritten source cannot use resume mode");

    assert!(matches!(error, CaptureError::SystemInvariant(_)));
    assert!(projector.seen_ordinals.is_empty());
}

#[test]
fn append_resume_accepts_a_changed_revision_at_the_certified_boundary() {
    let temp = tempdir().expect("tempdir");
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let current_source = SourceObservation::new(
        CaptureProvider::Pi,
        "pi-jsonl-v1",
        "fixture://captured-batch/projector",
        "source-revision-2",
        "provider:pi:pi-jsonl-v1:source:test",
        1,
        1,
        None,
    )
    .expect("current source");
    let initial = initial_jsonl_position().expect("initial JSONL position");
    let prior_cursor = CertifiedProviderCursor::new(
        "source-revision-1",
        current_source.capture_revision(),
        current_source.policy_revision(),
        initial.clone(),
        BoundedParserCheckpoint::from_serializable(&()).expect("valid fixture checkpoint"),
    )
    .expect("prior certified cursor");
    let prior = certified_provider_sync_cursor(
        CaptureProvider::Pi,
        TEST_MACHINE_ID,
        captured_batch_cursor_stream(&current_source),
        &prior_cursor,
        observed_at(),
    )
    .expect("prior Store cursor");
    store.upsert_sync_cursor(&prior).expect("seed prior cursor");
    let bytes = b"{}\n".to_vec();
    let mut producer = JsonlBatchProducer::new(
        Cursor::new(bytes.clone()),
        current_source.clone(),
        b"/tmp/captured-batch.jsonl".to_vec(),
        ProviderRecordKind::new("fixture").expect("record kind"),
        bytes.len() as u64,
        0,
        0,
        false,
    )
    .expect("JSONL producer");
    let appended_batch = producer
        .next_batch()
        .expect("capture appended batch")
        .expect("appended batch");
    let verified_append =
        verify_jsonl_append_boundary(&mut Cursor::new(bytes), &initial, &current_source, 3)
            .expect("verify append boundary");
    let mut projector = RejectingProjector {
        seen_ordinals: Vec::new(),
        cursor_position: appended_batch.range_end().clone(),
    };

    let outcome = import_captured_batch(
        &mut store,
        &CapturedSourceAdmission::conversation_without_cross_record_relationships(
            appended_batch.source(),
        ),
        &appended_batch,
        NormalizedProviderImportOptions::default(),
        TEST_MACHINE_ID,
        observed_at(),
        Some(&prior),
        &initial,
        CapturedBatchCursorMode::ResumeAppend(verified_append),
        &mut projector,
        || Ok(true),
    )
    .expect("verified append resumes from the certified boundary");

    assert_eq!(outcome.batches_imported, 1);
    assert_eq!(projector.seen_ordinals, vec![0]);
}

#[test]
fn append_with_only_an_incomplete_tail_rebinds_the_safe_candidate() {
    let temp = tempdir().expect("tempdir");
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let current_source = SourceObservation::new(
        CaptureProvider::Pi,
        "pi-jsonl-v1",
        "fixture://captured-batch/incomplete-append",
        "source-revision-incomplete-2",
        "provider:pi:pi-jsonl-v1:source:incomplete",
        1,
        1,
        None,
    )
    .expect("current source");
    let initial = initial_jsonl_position().expect("initial JSONL position");
    let checkpoint = BoundedParserCheckpoint::from_serializable(&json!({
        "typed": "content-free"
    }))
    .expect("bounded checkpoint");
    let prior_cursor = CertifiedProviderCursor::new(
        "source-revision-incomplete-1",
        current_source.capture_revision(),
        current_source.policy_revision(),
        initial.clone(),
        checkpoint.clone(),
    )
    .expect("prior cursor")
    .with_rejected_records(2);
    let prior = certified_provider_sync_cursor(
        CaptureProvider::Pi,
        TEST_MACHINE_ID,
        captured_batch_cursor_stream(&current_source),
        &prior_cursor,
        observed_at(),
    )
    .expect("prior Store cursor");
    store.upsert_sync_cursor(&prior).expect("seed prior cursor");
    let incomplete = b"unterminated".to_vec();
    let verified_append = verify_jsonl_append_boundary(
        &mut Cursor::new(incomplete.clone()),
        &initial,
        &current_source,
        incomplete.len() as u64,
    )
    .expect("verify append boundary");
    let admission =
        CapturedSourceAdmission::conversation_without_cross_record_relationships(&current_source);
    let mut projector = BatchEndRejectingProjector {
        seen_ordinals: Vec::new(),
    };

    let outcome = import_captured_batches(
        &mut store,
        &admission,
        NormalizedProviderImportOptions::default(),
        TEST_MACHINE_ID,
        observed_at(),
        Some(&prior),
        &initial,
        CapturedBatchCursorMode::ResumeAppend(verified_append),
        NonZeroUsize::new(1).expect("group limit"),
        &mut projector,
        || Ok(None),
        || Ok(true),
    )
    .expect("incomplete append keeps the prior parser-safe boundary");

    assert_eq!(outcome.batches_imported, 0);
    assert!(outcome.source_exhausted);
    assert!(outcome.cursor_safe);
    assert_eq!(outcome.summary.failed, 2);
    let stored = store
        .get_sync_cursor(None, TEST_MACHINE_ID, current_source.cursor_stream())
        .expect("read rebound cursor")
        .expect("rebound cursor");
    assert_ne!(stored.cursor, prior.cursor);
    let certified = CertifiedProviderCursor::decode(&stored.cursor).expect("decode cursor");
    assert_eq!(
        certified.source_revision(),
        current_source.source_revision()
    );
    assert_eq!(certified.native_position(), &initial);
    assert_eq!(certified.parser_checkpoint(), &checkpoint);
    assert_eq!(certified.rejected_records(), 2);
}

#[test]
fn legacy_cursor_is_replaced_only_from_the_initial_native_position() {
    let temp = tempdir().expect("tempdir");
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let batch = test_batch("source-revision-1", 0, &[0]);
    let mut legacy = cursor_for_batch(&batch, test_position(99));
    legacy.cursor = "legacy-provider-event-id".to_owned();
    store
        .upsert_sync_cursor(&legacy)
        .expect("seed legacy cursor");
    let mut projector = RejectingProjector {
        seen_ordinals: Vec::new(),
        cursor_position: batch.range_end().clone(),
    };

    import_captured_batch(
        &mut store,
        &CapturedSourceAdmission::conversation_without_cross_record_relationships(batch.source()),
        &batch,
        NormalizedProviderImportOptions::default(),
        TEST_MACHINE_ID,
        observed_at(),
        Some(&legacy),
        &test_position(0),
        CapturedBatchCursorMode::ReplaceLegacyCursor,
        &mut projector,
        || Ok(true),
    )
    .expect("replace legacy cursor after idempotent replay");

    let stored = store
        .get_sync_cursor(
            None,
            TEST_MACHINE_ID,
            &captured_batch_cursor_stream(batch.source()),
        )
        .expect("read cursor")
        .expect("certified cursor exists");
    assert!(CertifiedProviderCursor::decode_if_certified(&stored.cursor)
        .expect("decode cursor")
        .is_some());
}

#[test]
fn direct_batch_path_resolves_cross_record_relationships_in_one_pass() {
    let temp = tempdir().expect("tempdir");
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let batch = test_batch("source-revision-1", 0, &[0]);
    let mut capture = projected_pi_capture();
    capture.session.parent_provider_session_id = Some("later-parent".to_owned());
    let mut projector = CaptureProjector {
        capture,
        cursor_position: batch.range_end().clone(),
    };

    import_captured_batch(
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
    .expect("import relationship-bearing source");

    let parent = store
        .session_by_external_session(CaptureProvider::Pi, "later-parent")
        .expect("read parent placeholder")
        .expect("parent placeholder");
    assert_eq!(
        parent
            .sync
            .metadata
            .get("relationship_placeholder")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert!(store
        .get_sync_cursor(
            None,
            TEST_MACHINE_ID,
            &captured_batch_cursor_stream(batch.source()),
        )
        .expect("read cursor")
        .is_some());
}

#[test]
fn projected_capture_must_match_the_admitted_source_scope() {
    let temp = tempdir().expect("tempdir");
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let batch = test_batch("source-revision-1", 0, &[0]);
    let mut capture = projected_pi_capture();
    capture.source.machine_id = "different-machine".to_owned();
    let mut projector = CaptureProjector {
        capture,
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
    .expect_err("cross-source projection must fail before Store publication");

    assert!(matches!(error, CaptureError::SystemInvariant(_)));
    assert!(store
        .export_archive()
        .expect("export archive")
        .events
        .is_empty());
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
fn warp_event_conflict_rolls_back_projection_and_keeps_cursor_unpublished() {
    let temp = tempdir().expect("tempdir");
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let batch = test_batch_for_source(
        CaptureProvider::Warp,
        crate::WARP_SQLITE_SOURCE_FORMAT,
        "provider:warp:warp-sqlite-v1:source:test",
        "source-revision-warp-conflict",
        0,
        &[0],
    );
    let mut existing = projected_warp_capture();
    existing
        .event
        .as_mut()
        .expect("Warp event")
        .provider_event_hash = Some("warp-existing-hash".to_owned());
    import_provider_capture_line(
        &mut store,
        &existing,
        &NormalizedProviderImportOptions::default(),
        1,
        &mut ProviderImportCaches::default(),
    )
    .expect("seed existing Warp event");
    let archive_before_conflict = store.export_archive().expect("export seeded archive");

    let mut conflicting = existing;
    conflicting.session.role_hint = Some("must-roll-back".to_owned());
    conflicting.source.metadata["must_roll_back"] = json!(true);
    let event = conflicting.event.as_mut().expect("Warp event");
    event.provider_event_hash = Some("warp-conflicting-hash".to_owned());
    event.payload["text"] = json!("conflicting Warp payload");
    let mut projector = CaptureProjector {
        capture: conflicting,
        cursor_position: batch.range_end().clone(),
    };
    let revalidated = Cell::new(false);

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
        || {
            revalidated.set(true);
            Ok(true)
        },
    )
    .expect_err("Warp event hash conflicts must fail closed");

    assert!(matches!(
        error,
        CaptureError::Store(StoreError::ProviderEventConflict { .. })
    ));
    assert!(!revalidated.get());
    assert_eq!(
        store
            .export_archive()
            .expect("export archive after conflict"),
        archive_before_conflict
    );
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
fn oversized_first_projected_unit_fails_before_store_mutation() {
    let temp = tempdir().expect("tempdir");
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let batch = test_batch("source-revision-oversized-projection", 0, &[0]);
    let archive_before_import = store.export_archive().expect("export empty archive");
    let mut capture = projected_pi_capture();
    let event = capture.event.as_mut().expect("Pi event");
    event.payload["transaction_padding"] = json!("x".repeat(IMPORT_TRANSACTION_BATCH_BYTES));
    event.provider_event_hash = Some("oversized-projected-unit".to_owned());
    let mut projector = CaptureProjector {
        capture,
        cursor_position: batch.range_end().clone(),
    };
    let revalidated = Cell::new(false);

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
        || {
            revalidated.set(true);
            Ok(true)
        },
    )
    .expect_err("oversized projected units must fail before Store writes");

    assert!(matches!(
        error,
        CaptureError::InvalidPayload(message) if message.contains("transaction limit")
    ));
    assert!(!revalidated.get());
    assert_eq!(
        store
            .export_archive()
            .expect("export archive after failure"),
        archive_before_import
    );
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
fn child_first_relationship_uses_placeholder_and_enriches_it_in_one_pass() {
    let temp = tempdir().expect("tempdir");
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let import_batch = test_batch("source-revision-1", 0, &[0, 1]).into_source_exhausted();
    let mut child = projected_pi_capture();
    child.session.provider_session_id = "one-pass-child".to_owned();
    child.session.parent_provider_session_id = Some("one-pass-parent".to_owned());
    child.session.root_provider_session_id = Some("one-pass-parent".to_owned());
    child.session.status = SessionStatus::Completed;
    let mut parent = projected_pi_capture();
    parent.session.provider_session_id = "one-pass-parent".to_owned();
    parent.event = None;
    let parent_started_at = observed_at() - chrono::Duration::hours(1);
    parent.session.started_at = parent_started_at;
    let source = import_batch.source().clone();
    let admission =
        CapturedSourceAdmission::conversation_without_cross_record_relationships(&source);
    let mut import_projector = QueuedCaptureProjector {
        captures: VecDeque::from([child, parent]),
    };
    let mut import_batches = VecDeque::from([import_batch]);
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
        &mut import_projector,
        || Ok(import_batches.pop_front()),
        || Ok(true),
    )
    .expect("import child-first relationships");

    assert!(imported.source_exhausted);
    assert_eq!(imported.summary.imported_sessions, 2);
    let parent_session = store
        .session_by_external_session(CaptureProvider::Pi, "one-pass-parent")
        .expect("read parent")
        .expect("parent session");
    let imported_child = store
        .session_by_external_session(CaptureProvider::Pi, "one-pass-child")
        .expect("read child")
        .expect("child session");
    assert_eq!(imported_child.parent_session_id, Some(parent_session.id));
    assert_eq!(imported_child.root_session_id, Some(parent_session.id));
    assert_eq!(imported_child.status, SessionStatus::Completed);
    assert_eq!(parent_session.started_at, parent_started_at);
    assert_eq!(
        store.export_archive().expect("export archive").events.len(),
        1
    );
    assert_ne!(
        parent_session
            .sync
            .metadata
            .get("relationship_placeholder")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert!(parent_session.capture_source_id.is_some());
    assert!(store
        .get_sync_cursor(
            None,
            TEST_MACHINE_ID,
            &captured_batch_cursor_stream(&source),
        )
        .expect("read cursor")
        .is_some());
}

#[test]
fn existing_session_events_preserve_parent_metadata_edges_touches_and_idempotency() {
    let temp = tempdir().expect("tempdir");
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let batch = test_batch("source-revision-existing-session-events", 0, &[0, 1, 2]);
    let mut parent_capture = projected_pi_capture();
    parent_capture.session.provider_session_id = "existing-event-child".to_owned();
    parent_capture.session.parent_provider_session_id = Some("existing-event-parent".to_owned());
    parent_capture.session.root_provider_session_id = Some("existing-event-parent".to_owned());
    parent_capture.session.metadata["preserved"] = json!("original-parent-metadata");
    let preserved_status = parent_capture.session.status;
    let mut child_event = parent_capture.clone();
    child_event.session.parent_provider_session_id = Some("must-not-create-parent".to_owned());
    child_event.session.root_provider_session_id = Some("must-not-create-root".to_owned());
    child_event.session.metadata["preserved"] = json!("must-not-replace");
    child_event.session.status = SessionStatus::Failed;
    let event = child_event.event.as_mut().expect("fixture event");
    event.provider_event_index = 2;
    event.provider_event_hash = Some("existing-session-file-event".to_owned());
    event.cursor = Some("existing-session-file-event".to_owned());
    event.idempotency_key =
        Some("provider-event:pi:existing-event-child:existing-session-file-event".to_owned());
    event.event_type = EventType::FileTouched;
    event.payload = json!({
        "entry_id": "existing-session-file-event",
        "files": [{"path": "/workspace/existing-session.rs"}]
    });
    event.metadata["entry_id"] = json!("existing-session-file-event");
    event.metadata["provider_event_identity_index"] = json!(2);
    event.metadata["legacy_provider_event_index"] = json!(2);
    let replay = child_event.clone();
    let mut projector = ExistingSessionEventProjector {
        projections: VecDeque::from([(false, parent_capture), (true, child_event), (true, replay)]),
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
    .expect("import event children for an existing source-scoped session");

    assert_eq!(outcome.summary.imported_sessions, 2);
    assert_eq!(outcome.summary.imported_edges, 1);
    assert_eq!(outcome.summary.imported_events, 2);
    assert_eq!(outcome.summary.skipped_events, 1);
    assert_eq!(outcome.summary.failed, 0);
    let archive = store.export_archive().expect("export archive");
    assert_eq!(archive.sessions.len(), 2);
    assert_eq!(archive.events.len(), 2);
    assert_eq!(archive.files_touched.len(), 1);
    let child = store
        .session_by_external_session(CaptureProvider::Pi, "existing-event-child")
        .expect("read child session")
        .expect("persisted child session");
    let parent = store
        .session_by_external_session(CaptureProvider::Pi, "existing-event-parent")
        .expect("read parent session")
        .expect("persisted parent session");
    assert_eq!(child.parent_session_id, Some(parent.id));
    assert_eq!(child.root_session_id, Some(parent.id));
    assert_eq!(child.status, preserved_status);
    assert_eq!(
        child.sync.metadata["metadata"]["preserved"],
        "original-parent-metadata"
    );
    assert!(store
        .session_by_external_session(CaptureProvider::Pi, "must-not-create-parent")
        .expect("check ignored parent metadata")
        .is_none());
    assert!(store
        .get_sync_cursor(
            None,
            TEST_MACHINE_ID,
            &captured_batch_cursor_stream(batch.source()),
        )
        .expect("read cursor")
        .is_some());
}

#[test]
fn existing_session_event_wrong_source_parent_is_a_deterministic_rejection() {
    let temp = tempdir().expect("tempdir");
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let mut wrong_source = projected_pi_capture();
    wrong_source.session.provider_session_id = "wrong-source-parent".to_owned();
    wrong_source.source.raw_source_path = Some("/tmp/wrong-source.jsonl".to_owned());
    wrong_source.source.source_root = Some("/tmp/wrong-source.jsonl".to_owned());
    import_provider_capture_line(
        &mut store,
        &wrong_source,
        &NormalizedProviderImportOptions::default(),
        1,
        &mut ProviderImportCaches::default(),
    )
    .expect("seed only a wrong-source parent");
    let batch = test_batch("source-revision-wrong-source-parent", 0, &[0]);
    let mut projected = wrong_source;
    projected.source.raw_source_path = Some("/tmp/captured-batch.jsonl".to_owned());
    projected.source.source_root = Some("/tmp/captured-batch.jsonl".to_owned());
    let event = projected.event.as_mut().expect("fixture event");
    event.provider_event_index = 2;
    event.provider_event_hash = Some("wrong-source-child-event".to_owned());
    let mut projector = ExistingSessionEventProjector {
        projections: VecDeque::from([(true, projected)]),
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
    .expect("wrong-source parent is rejected without falling back");

    assert_eq!(outcome.summary.failed, 1);
    assert!(outcome.summary.failures[0]
        .error
        .contains("not already persisted for its exact source"));
    let archive = store.export_archive().expect("export archive");
    assert_eq!(archive.sessions.len(), 1);
    assert_eq!(archive.events.len(), 1);
    let stored_cursor = store
        .get_sync_cursor(
            None,
            TEST_MACHINE_ID,
            &captured_batch_cursor_stream(batch.source()),
        )
        .expect("read cursor")
        .expect("deterministic rejection advances cursor");
    assert_eq!(
        CertifiedProviderCursor::decode(&stored_cursor.cursor)
            .expect("decode cursor")
            .rejected_records(),
        1
    );
}
