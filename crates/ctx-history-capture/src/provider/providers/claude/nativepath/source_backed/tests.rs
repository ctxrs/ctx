use super::*;
use ctx_history_core::{CertifiedSource, ScannedSourceCounts, SourceObservation};
use ctx_history_index::{GenerationWriter, WriterOptions};

#[test]
fn claude_body_above_the_page_target_is_retained_whole() {
    let body = format!(
        "{}claude-full-body-tail",
        "x".repeat(8 * 1024 * 1024 + 64 * 1024)
    );
    let bytes = serde_json::json!({
        "type": "user",
        "sessionId": "large-body-session",
        "timestamp": "2026-07-31T12:00:00Z",
        "message": {"role": "user", "content": body},
    })
    .to_string()
    .into_bytes();
    assert!(bytes.len() <= crate::MAX_PROVIDER_JSONL_LINE_BYTES);
    let locator = ClaudePhysicalLocator {
        path: PathBuf::from("large-body-session.jsonl"),
        byte_start: 0,
        byte_end_exclusive: bytes.len() as u64,
        line_number: 1,
        record_sha256: Sha256::digest(&bytes).into(),
    };

    let parsed = parse_native_record(&bytes, 0, &locator).unwrap();

    assert_eq!(parsed.rows.len(), 1);
    assert_eq!(parsed.rows[0].body.as_deref(), Some(body.as_str()));
    assert!(parsed.rows[0]
        .body
        .as_deref()
        .is_some_and(|value| value.ends_with("claude-full-body-tail")));
}

fn fallback_row(body: &str, ordinal: u64) -> ClaudeRetainedRow {
    let bytes = serde_json::json!({
        "type": "user",
        "sessionId": "fallback-session",
        "timestamp": "2026-07-31T12:00:00Z",
        "message": {"role": "user", "content": body},
    })
    .to_string()
    .into_bytes();
    let locator = ClaudePhysicalLocator {
        path: PathBuf::from("fallback-session.jsonl"),
        byte_start: 0,
        byte_end_exclusive: bytes.len() as u64,
        line_number: ordinal + 1,
        record_sha256: Sha256::digest(&bytes).into(),
    };
    parse_native_record(&bytes, ordinal, &locator)
        .unwrap()
        .rows
        .into_iter()
        .next()
        .unwrap()
}

fn fallback_event_ids(bodies: &[&str]) -> Vec<StableEntityId> {
    let key = ClaudeSessionKey {
        root_session_id: "fallback-session".to_owned(),
        workflow_run_id: None,
        agent_id: None,
    };
    let source = source_key(&key).unwrap();
    let session_id = session_identity(&source, &session_typed_key(&key).unwrap()).unwrap();
    let mut state = FallbackEventIdentityState::default();
    bodies
        .iter()
        .enumerate()
        .map(|(ordinal, body)| {
            let row = fallback_row(body, ordinal as u64);
            let fallback = next_fallback_event_identity(&row, &source, session_id, &mut state)
                .unwrap()
                .unwrap();
            let native_item_key = native_item_key(&row, Some(fallback)).unwrap();
            derive_event_id(EventIdentityInput {
                source: &source,
                session_id,
                logical_item_kind: LOGICAL_EVENT_KIND,
                native_item_key: &native_item_key,
                subrecord_selector: None,
            })
            .unwrap()
        })
        .collect()
}

fn fallback_event_id(
    row: &ClaudeRetainedRow,
    source: &SourceKey,
    session_id: StableEntityId,
    state: &mut FallbackEventIdentityState,
) -> (StableEntityId, TypedKey) {
    let fallback = next_fallback_event_identity(row, source, session_id, state)
        .unwrap()
        .unwrap();
    let native_item_key = native_item_key(row, Some(fallback)).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .unwrap();
    let native_event_id = native_event_typed_key(row, Some(fallback)).unwrap();
    (event_id, native_event_id)
}

fn base_lookup_with_events(
    source: &SourceKey,
    session_id: StableEntityId,
    events: &[(StableEntityId, TypedKey)],
) -> (tempfile::TempDir, BaseEventIdentityLookup) {
    let temp = tempfile::tempdir().unwrap();
    let options = WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    };
    let mut writer = GenerationWriter::open(temp.path(), options.clone()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    for (index, (event_id, native_event_id)) in events.iter().enumerate() {
        let event_sequence = u64::try_from(index).unwrap() + 1;
        let mut record = CoreRecord::new_selected(
            *event_id,
            session_id,
            session_id,
            source.clone(),
            event_sequence,
            "message",
            "primary",
            true,
            PARSER_REVISION,
            "Claude fallback lookup test",
        )
        .unwrap();
        record.provider_session_id = Some("fallback-session".to_owned());
        record.native_event_id = Some(native_event_id.clone());
        record.occurred_at_unix_ms = Some(i64::try_from(event_sequence).unwrap());
        record.role = Some("user".to_owned());
        writer.add_core_record(record).unwrap();
    }
    let observation =
        SourceObservation::new(source.clone(), "fallback-test-source-v1", vec![1]).unwrap();
    let count = u64::try_from(events.len()).unwrap();
    writer
        .certify_source(
            CertifiedSource::certify(
                observation.clone(),
                observation,
                PARSER_REVISION,
                [1; 32],
                ScannedSourceCounts {
                    complete_records: count,
                    retained_records: count,
                    indexed_documents: count,
                    certified_bytes: count,
                    ..ScannedSourceCounts::default()
                },
            )
            .unwrap(),
        )
        .unwrap();
    writer.commit(|_| true).unwrap();
    let writer = GenerationWriter::open(temp.path(), options).unwrap();
    let lookup = writer.base_event_identity_lookup();
    drop(writer);
    (temp, lookup)
}

#[test]
fn duplicate_call_ids_are_ambiguous_and_result_linkage_abstains() {
    let mut pending_calls = HashMap::new();
    let mut capacity_exceeded = false;
    for command in ["git commit -m first", "git commit -m second"] {
        remember_pending_call(
            &mut pending_calls,
            &mut capacity_exceeded,
            "duplicate-call",
            PendingCallState::Exact(PendingCall {
                command: Some(command.to_owned()),
                declared_workdir: Some("/tmp/repository".to_owned()),
                event_sequence: 1,
            }),
        );
    }
    assert!(matches!(
        pending_calls.get("duplicate-call"),
        Some(PendingCallState::Ambiguous)
    ));

    let mut input = AttributionInput::default();
    let (context, abstained) = resolve_pending_call(
        &mut pending_calls,
        Some("duplicate-call"),
        capacity_exceeded,
        &mut input,
    );
    assert!(context.is_none());
    assert!(abstained);
    let annotation = RepositoryAttributor::default().attribute(input);
    assert!(annotation.repository_vcs_observations.is_empty());
    assert!(annotation.repository_abstentions.iter().any(|abstention| {
        abstention.reason == RepositoryAbstentionReason::ProviderOutputUnjoined
            && abstention.detail.as_deref() == Some("claude_tool_result_call_id_is_ambiguous")
    }));
}

fn test_projector() -> ClaudeProjector {
    let key = ClaudeSessionKey {
        root_session_id: "test-session".to_owned(),
        workflow_run_id: None,
        agent_id: None,
    };
    let binding = Binding {
        project_dir: PathBuf::from("/tmp/project"),
        key: key.clone(),
        layout: SessionLayout::Primary,
    };
    ClaudeProjector {
        source: source_key(&key).unwrap(),
        source_path: "test-session.jsonl".to_owned(),
        identities: identities(&binding).unwrap(),
        binding,
        session: ClaudeSessionMetadata::new(key),
        attributor: RepositoryAttributor::default(),
        pending_calls: HashMap::new(),
        linkage_capacity_exceeded: false,
        rejected_records: 0,
        fallback_identities: FallbackEventIdentityState::default(),
    }
}

#[test]
fn sixty_five_small_result_blocks_are_all_emitted() {
    let content = (0..65)
        .map(|index| {
            serde_json::json!({
                "type": "tool_result",
                "tool_use_id": format!("call-{index}"),
                "content": format!("result-{index}"),
                "is_error": false,
            })
        })
        .collect::<Vec<_>>();
    let bytes = serde_json::to_vec(&serde_json::json!({
        "type": "user",
        "uuid": "sixty-five-results",
        "sessionId": "test-session",
        "timestamp": "2026-08-01T12:00:00Z",
        "message": {"role": "user", "content": content},
    }))
    .unwrap();
    assert!(bytes.len() <= crate::MAX_PROVIDER_JSONL_LINE_BYTES);

    let mut projector = test_projector();
    let mut emitted = Vec::new();
    projector
        .project(JsonlRecordRef::for_test(&bytes, 0), &mut |record| {
            emitted.push(record);
            Ok(())
        })
        .unwrap();

    assert_eq!(emitted.len(), 65);
    assert_eq!(projector.rejected_records(), 0);
    assert_eq!(
        emitted.first().unwrap().content.normalized_body.as_deref(),
        Some("result-0")
    );
    assert_eq!(
        emitted.last().unwrap().content.normalized_body.as_deref(),
        Some("result-64")
    );
}

#[test]
fn malformed_empty_and_row_overflow_records_are_counted_rejections() {
    let malformed = b"{\"type\":\"user\",\"sessionId\":\"test-session\",\"message\":";
    let empty =
        br#"{"type":"user","sessionId":"test-session","message":{"role":"user","content":[]}}"#;
    let overflow_content = (0..=CLAUDE_MAX_RECORD_ROWS)
        .map(|_| serde_json::json!({"type": "tool_result"}))
        .collect::<Vec<_>>();
    let overflow = serde_json::to_vec(&serde_json::json!({
        "type": "user",
        "uuid": "overflow-results",
        "sessionId": "test-session",
        "message": {"role": "user", "content": overflow_content},
    }))
    .unwrap();
    assert!(overflow.len() <= crate::MAX_PROVIDER_JSONL_LINE_BYTES);
    let locator = ClaudePhysicalLocator {
        path: PathBuf::from("overflow-results.jsonl"),
        byte_start: 0,
        byte_end_exclusive: overflow.len() as u64,
        line_number: 1,
        record_sha256: Sha256::digest(&overflow).into(),
    };
    let overflow_error = parse_native_record(&overflow, 0, &locator).unwrap_err();
    assert!(overflow_error
        .to_string()
        .contains("Claude result exceeds the representable row limit"));

    let mut projector = test_projector();
    let mut emitted = Vec::new();
    projector
        .project(JsonlRecordRef::for_test(malformed, 0), &mut |record| {
            emitted.push(record);
            Ok(())
        })
        .unwrap();
    assert_eq!(projector.rejected_records(), 1);
    projector
        .project(JsonlRecordRef::for_test(empty, 1), &mut |record| {
            emitted.push(record);
            Ok(())
        })
        .unwrap();
    assert_eq!(projector.rejected_records(), 2);
    projector
        .project(JsonlRecordRef::for_test(&overflow, 2), &mut |record| {
            emitted.push(record);
            Ok(())
        })
        .unwrap();

    assert!(emitted.is_empty());
    assert_eq!(projector.rejected_records(), 3);
}

#[test]
fn exact_linked_unknown_tool_result_is_emitted_without_outcome_evidence() {
    let call = br#"{"type":"assistant","uuid":"call-record","sessionId":"test-session","message":{"role":"assistant","content":[{"type":"tool_use","id":"unknown-call","name":"Read","input":{"file_path":"src/lib.rs"}}]}}"#;
    let result = br#"{"type":"user","uuid":"result-record","sessionId":"test-session","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"unknown-call","content":"exact unknown output"}]}}"#;
    let mut projector = test_projector();
    let mut emitted = Vec::new();
    projector
        .project(JsonlRecordRef::for_test(call, 0), &mut |record| {
            emitted.push(record);
            Ok(())
        })
        .unwrap();
    projector
        .project(JsonlRecordRef::for_test(result, 1), &mut |record| {
            emitted.push(record);
            Ok(())
        })
        .unwrap();

    assert_eq!(emitted.len(), 2);
    assert!(emitted[1]
        .content
        .normalized_body
        .as_deref()
        .is_some_and(|body| body.contains("exact unknown output")));
    assert!(emitted[1].repository_vcs_observations.is_empty());
}

#[test]
fn over_8_mib_tool_result_is_admitted_complete_without_structured_body_duplication() {
    let tail = "claude_large_result_tail_complete";
    let full_result = format!("{} {tail}", "x".repeat(9 * 1024 * 1024));
    let bytes = serde_json::to_vec(&serde_json::json!({
        "type": "user",
        "uuid": "large-result-record",
        "sessionId": "test-session",
        "timestamp": "2026-08-01T12:00:00Z",
        "message": {
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "large-result-call",
                "content": full_result,
                "is_error": false
            }]
        }
    }))
    .unwrap();
    assert!(bytes.len() > 8 * 1024 * 1024);
    assert!(bytes.len() <= crate::MAX_PROVIDER_JSONL_LINE_BYTES);

    let mut projector = test_projector();
    let mut emitted = Vec::new();
    projector
        .project(JsonlRecordRef::for_test(&bytes, 0), &mut |record| {
            emitted.push(record);
            Ok(())
        })
        .unwrap();

    let [record] = emitted.as_slice() else {
        panic!("expected exactly one Claude result record");
    };
    assert_eq!(
        record.content.normalized_body.as_deref(),
        Some(full_result.as_str())
    );
    let structured = record.content.structured_content.as_ref().unwrap();
    assert_eq!(structured["result_content_location"], "normalized_body");
    assert_eq!(structured["result_content_complete"], true);
    let encoded_structured = serde_json::to_vec(structured).unwrap();
    assert!(encoded_structured.len() < 4 * 1024);
    assert!(!String::from_utf8(encoded_structured)
        .unwrap()
        .contains(tail));
    record.validate_contract().unwrap();
    record.encode_stored().unwrap();
}

#[test]
fn source_storage_project_path_never_becomes_core_workspace() {
    let source_storage = "/home/private-user/.claude/projects/-home-private-user-secret-project";
    let logical_workspace = "/workspace/provider-native-project";
    let bytes = serde_json::to_vec(&serde_json::json!({
        "type": "user",
        "uuid": "privacy-record",
        "sessionId": "test-session",
        "timestamp": "2026-08-01T12:00:00Z",
        "cwd": logical_workspace,
        "message": {"role": "user", "content": "privacy-safe message"}
    }))
    .unwrap();
    let mut projector = test_projector();
    projector.binding.project_dir = PathBuf::from(source_storage);
    projector.source_path = format!("{source_storage}/test-session.jsonl");
    let mut emitted = Vec::new();
    projector
        .project(JsonlRecordRef::for_test(&bytes, 0), &mut |record| {
            emitted.push(record);
            Ok(())
        })
        .unwrap();

    let [record] = emitted.as_slice() else {
        panic!("expected exactly one Claude message record");
    };
    assert_eq!(record.workspace, None);
    assert_eq!(record.cwd.as_deref(), Some(logical_workspace));
    let encoded = String::from_utf8(record.encode_stored().unwrap()).unwrap();
    assert!(!encoded.contains(source_storage));
    assert!(!encoded.contains("/home/private-user/.claude/projects"));
}

#[test]
fn checkpoint_byte_overflow_degrades_to_typed_linkage_capacity() {
    let mut projector = test_projector();
    projector.remember_pending_call(
        "oversized-call",
        PendingCallState::Exact(PendingCall {
            command: Some("x".repeat(MAX_PROJECTOR_CHECKPOINT_BYTES)),
            declared_workdir: Some("/tmp/project".to_owned()),
            event_sequence: 1,
        }),
    );

    assert!(projector.pending_calls.is_empty());
    assert!(projector.linkage_capacity_exceeded);
    assert!(encode_projector_checkpoint(&projector).is_ok());
    let mut input = AttributionInput::default();
    let (context, abstained) = resolve_pending_call(
        &mut projector.pending_calls,
        Some("oversized-call"),
        projector.linkage_capacity_exceeded,
        &mut input,
    );
    assert!(context.is_none());
    assert!(abstained);
    assert!(input
        .outcome_abstentions
        .iter()
        .any(|(reason, _)| { *reason == RepositoryAbstentionReason::LinkageCapacityExceeded }));
}

#[test]
fn fallback_event_ids_survive_insert_and_delete_before_with_stable_duplicates() {
    let baseline = fallback_event_ids(&["prefix", "target", "suffix"]);
    let inserted = fallback_event_ids(&["inserted", "prefix", "target", "suffix"]);
    let deleted = fallback_event_ids(&["target", "suffix"]);
    assert_eq!(baseline[1], inserted[2]);
    assert_eq!(baseline[1], deleted[0]);
    assert_eq!(baseline[2], inserted[3]);
    assert_eq!(baseline[2], deleted[1]);

    let duplicates = fallback_event_ids(&["duplicate", "duplicate"]);
    let replayed = fallback_event_ids(&["duplicate", "duplicate"]);
    assert_ne!(duplicates[0], duplicates[1]);
    assert_eq!(duplicates, replayed);
}

#[test]
fn append_after_prior_duplicate_probes_base_and_restores_call_ambiguity() {
    let key = ClaudeSessionKey {
        root_session_id: "fallback-session".to_owned(),
        workflow_run_id: None,
        agent_id: None,
    };
    let binding = Binding {
        project_dir: PathBuf::from("/tmp/project"),
        key: key.clone(),
        layout: SessionLayout::Primary,
    };
    let source = source_key(&key).unwrap();
    let identities = identities(&binding).unwrap();
    let mut cold_identity_state = FallbackEventIdentityState::default();
    let prefix_events = [fallback_row("duplicate", 0), fallback_row("duplicate", 1)]
        .iter()
        .map(|row| {
            fallback_event_id(
                row,
                &source,
                identities.session_id,
                &mut cold_identity_state,
            )
        })
        .collect::<Vec<_>>();
    let (_base, base_lookup) =
        base_lookup_with_events(&source, identities.session_id, &prefix_events);
    let mut pending_calls = HashMap::new();
    let mut linkage_capacity_exceeded = false;
    remember_pending_call(
        &mut pending_calls,
        &mut linkage_capacity_exceeded,
        "cross-append-call",
        PendingCallState::Exact(PendingCall {
            command: Some("git commit -m prefix".to_owned()),
            declared_workdir: Some("/tmp/project".to_owned()),
            event_sequence: 0,
        }),
    );
    let mut projector = ClaudeProjector {
        source: source.clone(),
        source_path: "fallback-session.jsonl".to_owned(),
        binding: binding.clone(),
        identities,
        session: ClaudeSessionMetadata::new(key),
        attributor: RepositoryAttributor::default(),
        pending_calls,
        linkage_capacity_exceeded,
        rejected_records: 0,
        fallback_identities: FallbackEventIdentityState::default(),
    };
    let checkpoint = encode_projector_checkpoint(&projector).unwrap();
    for occurrence in 0_u64..1_024 {
        let mut digest = [0; 32];
        digest[..8].copy_from_slice(&occurrence.to_be_bytes());
        projector
            .fallback_identities
            .next_occurrences
            .insert(digest, occurrence + 1);
    }
    assert_eq!(encode_projector_checkpoint(&projector).unwrap(), checkpoint);
    let mut restored = decode_projector_checkpoint(&checkpoint, &binding).unwrap();

    let suffix_row = fallback_row("duplicate", 2);
    let mut append_identity_state = FallbackEventIdentityState::new(Some(base_lookup));
    let suffix_event = fallback_event_id(
        &suffix_row,
        &source,
        projector.identities.session_id,
        &mut append_identity_state,
    );
    let replayed = fallback_event_ids(&["duplicate", "duplicate", "duplicate"]);
    assert_eq!(prefix_events[0].0, replayed[0]);
    assert_eq!(prefix_events[1].0, replayed[1]);
    assert_eq!(suffix_event.0, replayed[2]);
    assert_ne!(prefix_events[0].0, suffix_event.0);
    assert_ne!(prefix_events[1].0, suffix_event.0);

    remember_pending_call(
        &mut restored.pending_calls,
        &mut restored.linkage_capacity_exceeded,
        "cross-append-call",
        PendingCallState::Exact(PendingCall {
            command: Some("git commit -m suffix".to_owned()),
            declared_workdir: Some("/tmp/project".to_owned()),
            event_sequence: 1_u64 << 16,
        }),
    );
    let mut input = AttributionInput::default();
    let (context, abstained) = resolve_pending_call(
        &mut restored.pending_calls,
        Some("cross-append-call"),
        restored.linkage_capacity_exceeded,
        &mut input,
    );
    assert!(context.is_none());
    assert!(abstained);
    assert_eq!(
        input.outcome_abstentions,
        vec![(
            RepositoryAbstentionReason::ProviderOutputUnjoined,
            "claude_tool_result_call_id_is_ambiguous"
        )]
    );
}
