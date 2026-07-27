use super::*;

#[test]
fn production_import_streams_message_array_larger_than_the_file_limit() {
    let temp = tempdir().unwrap();
    let task_dir = temp.path().join("large-task");
    fs::create_dir(&task_dir).unwrap();
    fs::write(
        task_dir.join("task_metadata.json"),
        br#"{"taskId":"large-task","createdAt":"2026-07-18T11:00:00Z"}"#,
    )
    .unwrap();
    let message_path = task_dir.join("api_conversation_history.json");
    let message_count = CAPTURE_BATCH_MAX_RECORDS * 2 + 1;
    let padding_bytes = MAX_PROVIDER_JSONL_LINE_BYTES / (message_count - 1) + 1;
    write_large_message_array(&message_path, message_count, padding_bytes);
    assert!(fs::metadata(&message_path).unwrap().len() > MAX_PROVIDER_JSONL_LINE_BYTES as u64);

    let spec = task_json_provider(CaptureProvider::Cline);
    let context = test_context(&task_dir);
    let root_history_paths = task_json_root_history_candidate_paths(&task_dir, spec);
    let observation = TaskJsonTaskObservation::read(&task_dir, &root_history_paths, spec).unwrap();
    let path_identity = provider_path_identity(&observation.canonical_task_dir).unwrap();
    let source = SourceObservation::new(
        spec.provider,
        spec.source_format,
        format!(
            "{}-task-json-directory:{path_identity}",
            spec.provider.as_str()
        ),
        observation.source_revision(spec),
        provider_source_cursor_stream_for_path(spec.provider, spec.source_format, &path_identity),
        TASK_JSON_CAPTURE_REVISION,
        TASK_JSON_POLICY_REVISION,
        None,
    )
    .unwrap();
    let stream = captured_batch_cursor_stream(&source);
    let (session, state_failures) =
        task_json_session_state(&task_dir, &observation, &context, spec).unwrap();
    let mut legacy_checkpoint = TaskJsonCapturedBatchProjector::fresh_checkpoint(
        &session,
        &state_failures,
        context.imported_at,
    )
    .unwrap();
    legacy_checkpoint.terminal_seen = true;
    let legacy_cursor = CertifiedProviderCursor::new(
        source.source_revision(),
        2,
        source.policy_revision(),
        task_json_native_position(TaskJsonStreamPosition::done(0)).unwrap(),
        BoundedParserCheckpoint::from_serializable(&legacy_checkpoint).unwrap(),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    store
        .upsert_sync_cursor(&SyncCursor {
            id: crate::stable_capture_uuid("task-json-v2-done-cursor", "provider-sync-cursor"),
            team_id: None,
            device_id: context.machine_id.clone(),
            stream: stream.clone(),
            cursor: legacy_cursor.encode().unwrap(),
            last_synced_at: Some(context.imported_at),
            timestamps: EntityTimestamps {
                created_at: context.imported_at,
                updated_at: context.imported_at,
            },
        })
        .unwrap();
    let summary = import_task_json_history_batched(
        &task_dir,
        &mut store,
        context.clone(),
        NormalizedProviderImportOptions::default(),
        spec,
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_events, message_count);
    assert_eq!(store.list_sessions().unwrap().len(), 1);
    assert_eq!(
        store
            .search_event_hits("uniquetailsentinel", 10)
            .unwrap()
            .len(),
        1
    );
    let published = store
        .get_sync_cursor(None, &context.machine_id, &stream)
        .unwrap()
        .unwrap();
    assert_eq!(
        CertifiedProviderCursor::decode(&published.cursor)
            .unwrap()
            .parser_revision(),
        TASK_JSON_CAPTURE_REVISION
    );
}

#[test]
fn root_history_scan_streams_array_larger_than_the_file_limit() {
    let temp = tempdir().unwrap();
    let task_dir = temp.path().join("large-root-history-task");
    let state_dir = temp.path().join("state");
    fs::create_dir(&task_dir).unwrap();
    fs::create_dir(&state_dir).unwrap();
    let history_path = state_dir.join("taskHistory.json");
    let item_count = CAPTURE_BATCH_MAX_RECORDS * 2 + 1;
    let padding_bytes = MAX_PROVIDER_JSONL_LINE_BYTES / (item_count - 1) + 1;
    write_large_root_history_array(
        &history_path,
        item_count,
        padding_bytes,
        "large-root-history-task",
    );
    assert!(fs::metadata(&history_path).unwrap().len() > MAX_PROVIDER_JSONL_LINE_BYTES as u64);
    let spec = task_json_provider(CaptureProvider::Cline);
    let observation = TaskJsonTaskObservation::read(&task_dir, &[history_path], spec).unwrap();

    let fragment = task_json_root_history_fragment(&observation, "large-root-history-task")
        .unwrap()
        .unwrap();

    assert_eq!(fragment.id.as_deref(), Some("large-root-history-task"));
    assert_eq!(
        fragment.fallback_event.unwrap()["content"],
        "large root history fallback sentinel"
    );
}

#[test]
fn producer_caps_records_and_resumes_at_the_exact_array_offset() {
    let temp = tempdir().unwrap();
    let task_dir = temp.path().join("task");
    fs::create_dir(&task_dir).unwrap();
    write_messages(&task_dir.join("api_conversation_history.json"), 65, 0);
    let spec = task_json_provider(CaptureProvider::Cline);
    let observation = TaskJsonTaskObservation::read(&task_dir, &[], spec).unwrap();
    let source = test_source(&observation, spec);
    let record_kind = ProviderRecordKind::new(TASK_JSON_RECORD_KIND).unwrap();
    let mut producer = TaskJsonBatchProducer::new(
        source.clone(),
        record_kind.clone(),
        test_message_sources(&observation, spec),
        TaskJsonStreamPosition::initial(),
    )
    .unwrap();

    let first = producer.next_batch().unwrap().unwrap();
    assert_eq!(first.records().len(), CAPTURE_BATCH_MAX_RECORDS);
    assert!(first.retained_payload_bytes() <= CAPTURE_BATCH_MAX_PAYLOAD_BYTES);
    let resume = task_json_decode_position(first.range_end()).unwrap();
    assert_eq!(resume.phase, TaskJsonMessagePhase::Api as u8);
    assert_eq!(resume.native_index, CAPTURE_BATCH_MAX_RECORDS as u64);
    assert_eq!(resume.ordinal, CAPTURE_BATCH_MAX_RECORDS as u64);
    assert!(resume.offset > 0);
    assert!(!first.source_exhausted());

    let mut resumed = TaskJsonBatchProducer::new(
        source,
        record_kind,
        test_message_sources(&observation, spec),
        resume,
    )
    .unwrap();
    let second = resumed.next_batch().unwrap().unwrap();
    assert_eq!(second.records().len(), 2);
    let (_, class, native_index, _) =
        task_json_decode_locator(second.records()[0].locator()).unwrap();
    assert_eq!(class, TaskJsonRecordClass::Event);
    assert_eq!(native_index, 64);
    let CapturedRecordPayload::NativeBytes(bytes) = second.records()[0].payload() else {
        panic!("expected native task JSON bytes");
    };
    assert_eq!(
        serde_json::from_slice::<Value>(bytes).unwrap()["id"],
        "message-64"
    );
    let done = task_json_decode_position(second.range_end()).unwrap();
    assert_eq!(done, TaskJsonStreamPosition::done(66));
    assert!(second.source_exhausted());
    assert!(resumed.next_batch().unwrap().is_none());
}

#[test]
fn producer_revalidates_the_message_file_before_streaming() {
    let temp = tempdir().unwrap();
    let task_dir = temp.path().join("task");
    fs::create_dir(&task_dir).unwrap();
    let message_path = task_dir.join("api_conversation_history.json");
    write_messages(&message_path, 1, 0);
    let spec = task_json_provider(CaptureProvider::Cline);
    let observation = TaskJsonTaskObservation::read(&task_dir, &[], spec).unwrap();
    let mut producer = TaskJsonBatchProducer::new(
        test_source(&observation, spec),
        ProviderRecordKind::new(TASK_JSON_RECORD_KIND).unwrap(),
        test_message_sources(&observation, spec),
        TaskJsonStreamPosition::initial(),
    )
    .unwrap();
    fs::write(&message_path, b"[]").unwrap();

    assert!(matches!(
        producer.next_batch(),
        Err(CaptureError::SourceChangedDuringCapture)
    ));
}

#[test]
fn checkpoint_rehydrates_provider_content_for_exact_fallback_resume() {
    const TASK_SENTINEL: &str = "task-content-must-not-enter-checkpoint";
    const HISTORY_SENTINEL: &str = "history-fallback-must-not-enter-checkpoint";
    const INDEX_SENTINEL: &str = "index-content-must-not-enter-checkpoint";

    let temp = tempdir().unwrap();
    let task_dir = temp.path().join("roo-compact-checkpoint");
    fs::create_dir(&task_dir).unwrap();
    fs::write(
        task_dir.join("task_metadata.json"),
        serde_json::to_vec(&json!({
            "taskId": "roo-compact-checkpoint",
            "createdAt": "2026-07-18T11:00:00Z",
            "providerContent": TASK_SENTINEL,
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        task_dir.join("history_item.json"),
        serde_json::to_vec(&json!({
            "id": "roo-compact-checkpoint",
            "task": HISTORY_SENTINEL,
            "ts": 1_784_372_400_000_i64,
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        task_dir.join("_index.json"),
        serde_json::to_vec(&json!({
            "id": "roo-compact-checkpoint",
            "providerContent": INDEX_SENTINEL,
        }))
        .unwrap(),
    )
    .unwrap();
    let mut malformed_messages = Vec::from(&b"["[..]);
    for index in 0..CAPTURE_BATCH_MAX_RECORDS {
        if index != 0 {
            malformed_messages.push(b',');
        }
        malformed_messages.extend_from_slice(b"\"\xff\"");
    }
    malformed_messages.push(b']');
    fs::write(
        task_dir.join("api_conversation_history.json"),
        malformed_messages,
    )
    .unwrap();

    let spec = task_json_provider(CaptureProvider::RooCode);
    let observation = TaskJsonTaskObservation::read(&task_dir, &[], spec).unwrap();
    let source = test_source(&observation, spec);
    assert_eq!(source.capture_revision(), TASK_JSON_CAPTURE_REVISION);
    let mut producer = TaskJsonBatchProducer::new(
        source,
        ProviderRecordKind::new(TASK_JSON_RECORD_KIND).unwrap(),
        test_message_sources(&observation, spec),
        TaskJsonStreamPosition::initial(),
    )
    .unwrap();
    let first = producer.next_batch().unwrap().unwrap();
    let terminal = producer.next_batch().unwrap().unwrap();
    assert_eq!(first.records().len(), CAPTURE_BATCH_MAX_RECORDS);
    assert_eq!(terminal.records().len(), 1);
    assert!(!first.source_exhausted());
    assert!(terminal.source_exhausted());
    assert!(producer.next_batch().unwrap().is_none());

    let context = test_context(&task_dir);
    let (session, state_failures) =
        task_json_session_state(&task_dir, &observation, &context, spec).unwrap();
    let mut uninterrupted = TaskJsonCapturedBatchProjector::fresh(
        spec,
        context.clone(),
        task_dir.display().to_string(),
        session.clone(),
        state_failures.clone(),
    )
    .unwrap();
    let mut uninterrupted_first = CollectingProjectionOutput::default();
    for record in first.records() {
        uninterrupted
            .project_record(record, &mut uninterrupted_first)
            .unwrap();
    }
    let mut expected_terminal = CollectingProjectionOutput::default();
    uninterrupted
        .project_record(&terminal.records()[0], &mut expected_terminal)
        .unwrap();

    let mut interrupted = TaskJsonCapturedBatchProjector::fresh(
        spec,
        context.clone(),
        task_dir.display().to_string(),
        session,
        state_failures,
    )
    .unwrap();
    let mut interrupted_first = CollectingProjectionOutput::default();
    for record in first.records() {
        interrupted
            .project_record(record, &mut interrupted_first)
            .unwrap();
    }
    let initial_cursor = interrupted
        .initial_cursor_candidate(first.source(), first.range_before())
        .unwrap();
    let initial_checkpoint: TaskJsonParserCheckpoint =
        initial_cursor.parser_checkpoint().deserialize().unwrap();
    assert_eq!(initial_checkpoint.next_record_ordinal, 0);
    assert_eq!(initial_checkpoint.rejected_records, 0);
    assert!(!initial_checkpoint.state_failures_reported);

    let CapturedBatchCursorFinish::Advance(cursor) = interrupted.finish_cursor(&first).unwrap()
    else {
        panic!("task JSON cursor must advance at every batch boundary");
    };
    assert_eq!(cursor.parser_revision(), TASK_JSON_CAPTURE_REVISION);
    let checkpoint_bytes = cursor.parser_checkpoint().as_bytes();
    assert!(checkpoint_bytes.len() < 1024);
    let checkpoint_text = String::from_utf8_lossy(checkpoint_bytes);
    for sentinel in [TASK_SENTINEL, HISTORY_SENTINEL, INDEX_SENTINEL] {
        assert!(!checkpoint_text.contains(sentinel));
    }
    let checkpoint: TaskJsonParserCheckpoint = cursor.parser_checkpoint().deserialize().unwrap();
    assert_eq!(
        checkpoint.next_record_ordinal,
        CAPTURE_BATCH_MAX_RECORDS as u64
    );
    assert_eq!(checkpoint.accepted_events, 0);
    assert_eq!(
        checkpoint.rejected_records,
        CAPTURE_BATCH_MAX_RECORDS as u64
    );
    assert_eq!(checkpoint.session.task_id, "roo-compact-checkpoint");

    let mut resumed_context = context;
    resumed_context.imported_at = "2026-07-19T12:00:00Z".parse().unwrap();
    let (observed_session, observed_state_failures) =
        task_json_session_state(&task_dir, &observation, &resumed_context, spec).unwrap();
    let mut resumed = TaskJsonCapturedBatchProjector::resume(
        spec,
        resumed_context,
        task_dir.display().to_string(),
        observed_session,
        observed_state_failures,
        &cursor,
    )
    .unwrap();
    let mut actual_terminal = CollectingProjectionOutput::default();
    resumed
        .project_record(&terminal.records()[0], &mut actual_terminal)
        .unwrap();

    assert_eq!(actual_terminal.rejections, expected_terminal.rejections);
    assert_eq!(actual_terminal.normalizations.len(), 1);
    assert_eq!(expected_terminal.normalizations.len(), 1);
    assert_eq!(
        actual_terminal.normalizations[0].summary,
        expected_terminal.normalizations[0].summary
    );
    assert_eq!(
        actual_terminal.normalizations[0].captures,
        expected_terminal.normalizations[0].captures
    );
    assert_eq!(
        actual_terminal.normalizations[0].files_touched,
        expected_terminal.normalizations[0].files_touched
    );
    assert_eq!(
        actual_terminal.normalizations[0].captures[0]
            .1
            .event
            .as_ref()
            .unwrap()
            .payload["text"],
        HISTORY_SENTINEL
    );
}

#[test]
fn producer_caps_retained_payload_bytes_without_a_source_collection() {
    let temp = tempdir().unwrap();
    let task_dir = temp.path().join("task");
    fs::create_dir(&task_dir).unwrap();
    write_messages(
        &task_dir.join("api_conversation_history.json"),
        9,
        1024 * 1024,
    );
    let spec = task_json_provider(CaptureProvider::Cline);
    let observation = TaskJsonTaskObservation::read(&task_dir, &[], spec).unwrap();
    let mut producer = TaskJsonBatchProducer::new(
        test_source(&observation, spec),
        ProviderRecordKind::new(TASK_JSON_RECORD_KIND).unwrap(),
        test_message_sources(&observation, spec),
        TaskJsonStreamPosition::initial(),
    )
    .unwrap();
    let mut batches = 0;
    let mut records = 0;
    while let Some(batch) = producer.next_batch().unwrap() {
        batches += 1;
        records += batch.records().len();
        assert!(batch.records().len() <= CAPTURE_BATCH_MAX_RECORDS);
        assert!(batch.retained_payload_bytes() <= CAPTURE_BATCH_MAX_PAYLOAD_BYTES);
    }
    assert!(batches >= 2);
    assert_eq!(records, 10);
}

#[test]
fn producer_admits_a_singleton_between_batch_and_record_limits() {
    let temp = tempdir().unwrap();
    let task_dir = temp.path().join("task");
    fs::create_dir(&task_dir).unwrap();
    write_messages(
        &task_dir.join("api_conversation_history.json"),
        1,
        CAPTURE_BATCH_MAX_PAYLOAD_BYTES + 1,
    );
    let spec = task_json_provider(CaptureProvider::Cline);
    let observation = TaskJsonTaskObservation::read(&task_dir, &[], spec).unwrap();
    let mut producer = TaskJsonBatchProducer::new(
        test_source(&observation, spec),
        ProviderRecordKind::new(TASK_JSON_RECORD_KIND).unwrap(),
        test_message_sources(&observation, spec),
        TaskJsonStreamPosition::initial(),
    )
    .unwrap();

    let batch = producer.next_batch().unwrap().unwrap();
    assert_eq!(batch.records().len(), 1);
    assert!(batch.retained_payload_bytes() > CAPTURE_BATCH_MAX_PAYLOAD_BYTES);
    assert!(batch.retained_payload_bytes() <= CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES);
    let (_, class, _, _) = task_json_decode_locator(batch.records()[0].locator()).unwrap();
    assert_eq!(class, TaskJsonRecordClass::Event);
}

#[test]
fn producer_admits_a_valid_item_at_the_exact_record_limit() {
    let temp = tempdir().unwrap();
    let task_dir = temp.path().join("task");
    fs::create_dir(&task_dir).unwrap();
    write_exact_limit_message_array(&task_dir.join("api_conversation_history.json"), b"]");
    let spec = task_json_provider(CaptureProvider::Cline);
    let observation = TaskJsonTaskObservation::read(&task_dir, &[], spec).unwrap();
    let mut producer = TaskJsonBatchProducer::new(
        test_source(&observation, spec),
        ProviderRecordKind::new(TASK_JSON_RECORD_KIND).unwrap(),
        test_message_sources(&observation, spec),
        TaskJsonStreamPosition::initial(),
    )
    .unwrap();

    let batch = producer.next_batch().unwrap().unwrap();
    assert_eq!(batch.records().len(), 1);
    assert_eq!(
        batch.retained_payload_bytes(),
        CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES
    );
    let (_, class, _, _) = task_json_decode_locator(batch.records()[0].locator()).unwrap();
    assert_eq!(class, TaskJsonRecordClass::Event);
    assert!(!batch.source_exhausted());
    assert!(producer.next_batch().unwrap().unwrap().source_exhausted());
    assert!(producer.next_batch().unwrap().is_none());
}

#[test]
fn producer_rejects_exact_limit_items_with_invalid_array_separators_once() {
    let temp = tempdir().unwrap();
    let cases: [(&str, &[u8], &str); 2] = [
        (
            "trailing-comma",
            b",]",
            "task message JSON array has a trailing comma",
        ),
        (
            "invalid-separator",
            b"!]",
            "task message JSON array has an invalid item separator",
        ),
    ];
    for (name, array_suffix, expected_reason) in cases {
        let task_dir = temp.path().join(name);
        fs::create_dir(&task_dir).unwrap();
        write_exact_limit_message_array(
            &task_dir.join("api_conversation_history.json"),
            array_suffix,
        );
        let spec = task_json_provider(CaptureProvider::Cline);
        let observation = TaskJsonTaskObservation::read(&task_dir, &[], spec).unwrap();
        let mut producer = TaskJsonBatchProducer::new(
            test_source(&observation, spec),
            ProviderRecordKind::new(TASK_JSON_RECORD_KIND).unwrap(),
            test_message_sources(&observation, spec),
            TaskJsonStreamPosition::initial(),
        )
        .unwrap();

        let batch = producer.next_batch().unwrap().unwrap();
        assert_eq!(batch.records().len(), 2);
        let (_, class, _, _) = task_json_decode_locator(batch.records()[0].locator()).unwrap();
        assert_eq!(class, TaskJsonRecordClass::FileError);
        let CapturedRecordPayload::NativeBytes(reason) = batch.records()[0].payload() else {
            panic!("expected deterministic task JSON file rejection");
        };
        assert_eq!(String::from_utf8_lossy(reason), expected_reason);
        assert!(batch.source_exhausted());
        assert!(producer.next_batch().unwrap().is_none());
    }
}

#[test]
fn producer_rejects_a_native_item_larger_than_the_record_limit() {
    let temp = tempdir().unwrap();
    let task_dir = temp.path().join("task");
    fs::create_dir(&task_dir).unwrap();
    write_messages(
        &task_dir.join("api_conversation_history.json"),
        1,
        CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES + 1,
    );
    let spec = task_json_provider(CaptureProvider::Cline);
    let observation = TaskJsonTaskObservation::read(&task_dir, &[], spec).unwrap();
    let mut producer = TaskJsonBatchProducer::new(
        test_source(&observation, spec),
        ProviderRecordKind::new(TASK_JSON_RECORD_KIND).unwrap(),
        test_message_sources(&observation, spec),
        TaskJsonStreamPosition::initial(),
    )
    .unwrap();

    let batch = producer.next_batch().unwrap().unwrap();
    assert!(batch.retained_payload_bytes() <= CAPTURE_BATCH_MAX_PAYLOAD_BYTES);
    let (_, class, _, _) = task_json_decode_locator(batch.records()[0].locator()).unwrap();
    assert_eq!(class, TaskJsonRecordClass::FileError);
}
