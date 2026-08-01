use super::*;
use ctx_history_core::{CertifiedSource, ScannedSourceCounts, SourceObservation};
use ctx_history_index::{GenerationWriter, WriterOptions};

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
    assert!(retain_result_event(
        false,
        abstained,
        ClaudeOutputOutcome::Success
    ));
    assert!(retain_result_event(
        false,
        false,
        ClaudeOutputOutcome::Failure
    ));
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
