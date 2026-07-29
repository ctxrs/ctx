use super::*;

#[test]
fn source_backed_timeout_result_is_exactly_hydratable_on_cold_and_replay() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fa000-0000-7000-8000-000000000071";
    let exact_output = r#"{"message":"Wait timed out.","timed_out":true}"#;
    let timeout_result = serde_json::json!({
        "timestamp": "2026-07-19T03:38:53Z",
        "type": "response_item",
        "payload": {
            "type": "function_call_output",
            "call_id": "call-timeout-result",
            "output": exact_output,
            "internal_chat_message_metadata_passthrough": {
                "turn_id": "turn-timeout-result"
            }
        }
    })
    .to_string();
    let metadata_only = serde_json::json!({
        "type": "event_msg",
        "payload": {
            "type": "token_count",
            "info": {"total_token_usage": {"input_tokens": 42}}
        }
    })
    .to_string();
    let metadata_only_failure = serde_json::json!({
        "timestamp": "2026-07-19T03:38:54Z",
        "type": "response_item",
        "payload": {
            "type": "function_call_output",
            "call_id": "call-metadata-only",
            "status": "failed"
        }
    })
    .to_string();
    let encrypted_reasoning = serde_json::json!({
        "timestamp": "2026-07-19T03:38:55Z",
        "type": "response_item",
        "payload": {
            "type": "reasoning",
            "encrypted_content": "opaque-code-only-record",
            "summary": []
        }
    })
    .to_string();
    write_session(
        &sessions,
        native_session_id,
        &[
            timeout_result,
            metadata_only,
            metadata_only_failure,
            encrypted_reasoning,
        ],
    );

    let cold = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    assert_eq!(cold.commit.indexed_documents, 1);
    let cold_index = VerifiedIndex::open(&index).unwrap();
    let source = codex_source_key(native_session_id).unwrap();
    let cold_page = cold_index.source_event_page(&source, None, 64).unwrap();
    assert!(cold_page.terminal);
    assert_eq!(cold_page.items.len(), 1);
    assert_eq!(cold_page.items[0].event_sequence, 1);
    assert_eq!(cold_page.items[0].event_type, "tool_output");
    let cold_event_id = cold_page.items[0].event_id;
    let cold_text = hydrate_codex_locator(&sessions, &cold_page.items[0].locator)
        .unwrap()
        .decoded_display_text
        .expect("every published Codex document must have exact display text");
    assert_eq!(cold_text, exact_output);
    let cold_counts = cold_index.manifest().sources[0].counts();
    assert_eq!(cold_counts.complete_records, 5);
    assert_eq!(cold_counts.retained_records, 1);
    assert_eq!(cold_counts.ignored_records, 4);
    assert_eq!(cold_counts.indexed_documents, 1);

    let replay = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    assert_eq!(replay.counters.replayed_sources, 1);
    assert_eq!(replay.counters.staged_documents, 0);
    assert_eq!(replay.commit.generation_id, cold.commit.generation_id);
    let replayed_index = VerifiedIndex::open(&index).unwrap();
    let replayed_page = replayed_index.source_event_page(&source, None, 64).unwrap();
    assert!(replayed_page.terminal);
    assert_eq!(replayed_page.items.len(), 1);
    assert_eq!(replayed_page.items[0].event_id, cold_event_id);
    let replayed_text = hydrate_codex_locator(&sessions, &replayed_page.items[0].locator)
        .unwrap()
        .decoded_display_text
        .expect("replayed Codex document must remain exactly hydratable");
    assert_eq!(replayed_text, cold_text);
    assert_eq!(replayed_index.manifest().sources[0].counts(), cold_counts);
}
#[test]
fn source_backed_batch_opens_once_reads_by_offset_and_restores_caller_order() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fa000-0000-7000-8000-000000000081";
    write_session(
        &sessions,
        native_session_id,
        &[
            message("user", "batch first sentinel"),
            message("assistant", "batch second sentinel"),
            message("user", "batch third sentinel"),
        ],
    );
    ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    let source = codex_source_key(native_session_id).unwrap();
    let session_id = codex_session_identity(&source, native_session_id).unwrap();
    let events = VerifiedIndex::open(&index)
        .unwrap()
        .events_for_session(session_id.as_uuid())
        .unwrap();
    assert_eq!(events.len(), 3);
    let requests = [2_usize, 0, 1]
        .into_iter()
        .map(|index| {
            EventHydrationRequest::new(events[index].event_id, events[index].locator.clone())
                .unwrap()
        })
        .collect::<Vec<_>>();
    let resolver = CodexLocatorResolverV0::discover([&sessions]).unwrap();
    let individual = requests
        .iter()
        .map(|request| resolver.hydrate_event_request(request).unwrap())
        .collect::<Vec<_>>();

    CODEX_HYDRATION_SOURCE_OPEN_CALLS.with(|calls| calls.set(0));
    let batch = BatchHydrationRequest::new(requests.clone()).unwrap();
    let hydrated = resolver.hydrate_batch_request(&batch).unwrap();
    assert_eq!(
        CODEX_HYDRATION_SOURCE_OPEN_CALLS.with(|calls| calls.get()),
        1
    );
    assert_eq!(hydrated.records(), individual);
    assert_eq!(
        hydrated
            .records()
            .iter()
            .map(|record| record.event_id)
            .collect::<Vec<_>>(),
        requests
            .iter()
            .map(EventHydrationRequest::event_id)
            .collect::<Vec<_>>()
    );

    let mut expected_offsets = requests
        .iter()
        .map(|request| match request.locator().coordinate() {
            NativeRecordCoordinate::Jsonl { byte_offset, .. } => *byte_offset,
            _ => panic!("Codex event locator is not JSONL"),
        })
        .collect::<Vec<_>>();
    expected_offsets.sort_unstable();
    assert_eq!(
        CODEX_BATCH_READ_OFFSETS.with(|offsets| offsets.borrow().clone()),
        expected_offsets
    );
}

#[test]
fn source_backed_batch_rejects_duplicate_cross_source_invalid_ordinal_and_digest() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    fs::create_dir_all(&sessions).unwrap();
    let first_id = "019fa000-0000-7000-8000-000000000082";
    let second_id = "019fa000-0000-7000-8000-000000000083";
    write_session(
        &sessions,
        first_id,
        &[message("user", "batch validation first")],
    );
    write_session(
        &sessions,
        second_id,
        &[message("assistant", "batch validation second")],
    );
    ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    let verified = VerifiedIndex::open(&index).unwrap();
    let first = verified
        .source_event_page(&codex_source_key(first_id).unwrap(), None, 8)
        .unwrap()
        .items
        .into_iter()
        .next()
        .unwrap();
    let second = verified
        .source_event_page(&codex_source_key(second_id).unwrap(), None, 8)
        .unwrap()
        .items
        .into_iter()
        .next()
        .unwrap();
    let first_request = EventHydrationRequest::new(first.event_id, first.locator.clone()).unwrap();
    let second_request =
        EventHydrationRequest::new(second.event_id, second.locator.clone()).unwrap();
    assert!(matches!(
        BatchHydrationRequest::new(vec![first_request.clone(), first_request.clone()]),
        Err(SourceResolverContractError::DuplicateEventIdentity)
    ));

    let resolver = CodexLocatorResolverV0::discover([&sessions]).unwrap();
    let cross_source =
        BatchHydrationRequest::new(vec![first_request.clone(), second_request]).unwrap();
    assert!(matches!(
        resolver.hydrate_batch_request(&cross_source),
        Err(CodexSourceBackedErrorV0::InvalidCodexLocator)
    ));

    let NativeRecordCoordinate::Jsonl {
        byte_offset,
        byte_length,
        physical_ordinal,
        native_session_key,
        native_event_key,
    } = first.locator.coordinate().clone()
    else {
        panic!("Codex event locator is not JSONL");
    };
    let invalid_ordinal_locator = SourceRecordLocator::new(
        first.locator.source().clone(),
        NativeRecordCoordinate::Jsonl {
            byte_offset,
            byte_length,
            physical_ordinal: physical_ordinal + 1,
            native_session_key,
            native_event_key,
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        *first.locator.record_digest(),
    )
    .unwrap();
    let invalid_ordinal = BatchHydrationRequest::new(vec![EventHydrationRequest::new(
        first.event_id,
        invalid_ordinal_locator,
    )
    .unwrap()])
    .unwrap();
    assert!(matches!(
        resolver.hydrate_batch_request(&invalid_ordinal),
        Err(CodexSourceBackedErrorV0::InvalidCodexLocator)
    ));

    let invalid_digest_locator = SourceRecordLocator::new(
        first.locator.source().clone(),
        first.locator.coordinate().clone(),
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        [0_u8; 32],
    )
    .unwrap();
    let invalid_digest = BatchHydrationRequest::new(vec![EventHydrationRequest::new(
        first.event_id,
        invalid_digest_locator,
    )
    .unwrap()])
    .unwrap();
    assert!(matches!(
        resolver.hydrate_batch_request(&invalid_digest),
        Err(CodexSourceBackedErrorV0::LocatorDigestMismatch)
    ));

    let invalid_source = SourceKey::derive(
        CaptureProvider::Codex.as_str(),
        CODEX_SESSION_SOURCE_FORMAT,
        "codex-invalid-batch-descriptor",
        1,
        first.locator.source().anchor().clone(),
    )
    .unwrap();
    let invalid_source_event =
        codex_event_identity(&invalid_source, first_id, physical_ordinal).unwrap();
    let invalid_source_locator = SourceRecordLocator::new(
        invalid_source,
        first.locator.coordinate().clone(),
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        *first.locator.record_digest(),
    )
    .unwrap();
    let invalid_source_batch = BatchHydrationRequest::new(vec![EventHydrationRequest::new(
        invalid_source_event,
        invalid_source_locator,
    )
    .unwrap()])
    .unwrap();
    assert!(matches!(
        resolver.hydrate_batch_request(&invalid_source_batch),
        Err(CodexSourceBackedErrorV0::InvalidCodexLocator)
    ));
}

#[test]
fn source_backed_batch_detects_source_revision_change_and_missing_source() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fa000-0000-7000-8000-000000000084";
    write_session(
        &sessions,
        native_session_id,
        &[message("user", "batch source revision sentinel")],
    );
    ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    let event = VerifiedIndex::open(&index)
        .unwrap()
        .source_event_page(&codex_source_key(native_session_id).unwrap(), None, 8)
        .unwrap()
        .items
        .into_iter()
        .next()
        .unwrap();
    let batch = BatchHydrationRequest::new(vec![EventHydrationRequest::new(
        event.event_id,
        event.locator,
    )
    .unwrap()])
    .unwrap();
    let resolver = CodexLocatorResolverV0::discover([&sessions]).unwrap();
    let source_path = session_path(&sessions, native_session_id);
    let original = fs::read(&source_path).unwrap();
    let mut replacement = original.clone();
    let marker = b"batch source revision sentinel";
    let marker_offset = replacement
        .windows(marker.len())
        .position(|window| window == marker)
        .unwrap();
    replacement[marker_offset] = b'B';
    fs::remove_file(&source_path).unwrap();
    fs::write(&source_path, replacement).unwrap();
    let changed_result = resolver.hydrate_batch_request(&batch);
    assert!(
        matches!(
            changed_result,
            Err(CodexSourceBackedErrorV0::Capture(
                CaptureError::SourceChangedDuringCapture
            ))
        ),
        "unexpected changed-source result: {changed_result:?}"
    );

    fs::remove_file(&source_path).unwrap();
    let missing_resolver = CodexLocatorResolverV0::discover([&sessions]).unwrap();
    assert!(matches!(
        missing_resolver.hydrate_batch_request(&batch),
        Err(CodexSourceBackedErrorV0::LocatorSourceNotFound(id))
            if id == native_session_id
    ));
}

/// Read-only production oracle. The provider corpus is only observed and
/// hydrated; both Core generations are written below a fresh tempdir.
#[test]
#[ignore = "set CTX_CODEX_PRO_HYDRATION_ORACLE_ROOT to a production Codex sessions root"]
fn production_corpus_cold_and_replay_source_pages_have_exact_id_content_parity() {
    let sessions = std::env::var_os("CTX_CODEX_PRO_HYDRATION_ORACLE_ROOT")
        .map(PathBuf::from)
        .expect("CTX_CODEX_PRO_HYDRATION_ORACLE_ROOT must be set");
    let before = discover_codex_root_inventory_v0(&sessions).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let index = temp.path().join("global-index");

    let cold = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    let cold_index = VerifiedIndex::open(&index).unwrap();
    let cold_oracle = exact_source_page_oracle(&sessions, &cold_index);
    assert_eq!(cold_oracle.0, cold.commit.indexed_documents);

    let replay = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    assert_eq!(replay.counters.staged_documents, 0);
    assert_eq!(replay.commit.generation_id, cold.commit.generation_id);
    let replayed_index = VerifiedIndex::open(&index).unwrap();
    let replayed_oracle = exact_source_page_oracle(&sessions, &replayed_index);
    assert_eq!(replayed_oracle, cold_oracle);

    let after = discover_codex_root_inventory_v0(&sessions).unwrap();
    assert_eq!(after.certificate, before.certificate);
}
