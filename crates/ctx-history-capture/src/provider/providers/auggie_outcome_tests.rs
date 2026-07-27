#[test]
fn completed_message_metadata_does_not_invent_a_tool_result() {
    let entry = json!({"completed": true, "source": "agent"});
    let exchange = json!({"request_id": "request-1"});
    let event = auggie_event(AuggieEventInput {
        provider_session_id: "session-1",
        provider_event_index: 0,
        chat_index: 0,
        role: EventRole::Assistant,
        label: "response",
        occurred_at: "2026-07-21T00:00:00Z".parse().unwrap(),
        text: "created commit 0123456789abcdef0123456789abcdef01234567".to_owned(),
        entry: &entry,
        exchange: &exchange,
        raw_source_path: "/tmp/auggie/session.json",
    });

    assert_eq!(event.event_type, EventType::Message);
    assert_eq!(event.payload["result_outcome"], Value::Null);
    assert_eq!(event.payload["result_evidence"], Value::Null);
}

#[test]
fn structural_rejection_replay_preserves_failed_count_without_scaffolding() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("oversized.json");
    let oversized_bytes = u64::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES)
        .unwrap()
        .checked_add(1)
        .unwrap();
    let file = File::create(&path).unwrap();
    file.set_len(oversized_bytes).unwrap();
    file.sync_all().unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let context = ProviderAdapterContext {
        machine_id: "auggie-structural-replay-machine".to_owned(),
        source_path: Some(path.clone()),
        source_root: Some(temp.path().to_path_buf()),
        imported_at: "2026-07-17T12:01:00Z".parse().unwrap(),
    };
    let import_options = NormalizedProviderImportOptions::default();

    let first =
        import_auggie_session_file_batched(path.clone(), &mut store, &context, &import_options)
            .unwrap();

    assert_eq!(first.failed, 1, "{:?}", first.failures);
    assert_eq!(first.failures.len(), 1);
    assert!(first.failures[0].error.contains("exceeds"));
    assert_eq!(first.imported_sessions, 0);
    assert_eq!(first.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());

    let replay = import_auggie_session_file_batched(
        path,
        &mut store,
        &ProviderAdapterContext {
            imported_at: "2026-07-17T12:02:00Z".parse().unwrap(),
            ..context
        },
        &import_options,
    )
    .unwrap();

    assert_eq!(replay.failed, first.failed);
    assert!(replay.failures.is_empty());
    assert_eq!(replay.imported_sessions, 0);
    assert_eq!(replay.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());
}
