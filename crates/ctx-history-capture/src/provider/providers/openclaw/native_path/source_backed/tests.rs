use super::*;
use ctx_history_core::{CertifiedSource, ScannedSourceCounts, SourceObservation};
use ctx_history_index::{GenerationWriter, WriterOptions};

const HISTORY: &str =
    include_str!("../../../../../../tests/fixtures/repository_attribution/openclaw-native.jsonl");
const SESSIONS: &str =
    include_str!("../../../../../../tests/fixtures/repository_attribution/openclaw-sessions.json");

fn test_projector() -> (tempfile::TempDir, OpenClawProjector) {
    let temp = tempfile::tempdir().unwrap();
    let authority = Arc::new(ProviderSourceRoot::open(temp.path()).unwrap());
    let native_session_id = "main/test-session";
    let source = source_key(native_session_id).unwrap();
    let session_id = session_identity(&source, native_session_id).unwrap();
    let session = SessionState::new(
        Path::new("/agents/main/sessions/test-session.jsonl"),
        native_session_id,
        &Value::Null,
        None,
        None,
        DateTime::<Utc>::UNIX_EPOCH,
        session_id,
    )
    .unwrap();
    (
        temp,
        OpenClawProjector {
            source,
            native_session_id: native_session_id.to_owned(),
            session_id,
            session,
            index_file: None,
            authority,
            attributor: RepositoryAttributor::default(),
            pending_calls: HashMap::new(),
            running_processes: HashMap::new(),
            linkage_capacity_exceeded: false,
            fallback_identities: FallbackEventIdentityState::default(),
        },
    )
}

fn fallback_event_ids(bodies: &[&str]) -> Vec<StableEntityId> {
    let native_session_id = "main/fallback-session";
    let source = source_key(native_session_id).unwrap();
    let session_id = session_identity(&source, native_session_id).unwrap();
    let occurred_at = "2026-07-31T12:00:00Z".parse::<DateTime<Utc>>().unwrap();
    let mut state = FallbackEventIdentityState::default();
    bodies
        .iter()
        .enumerate()
        .map(|(ordinal, body)| {
            let value = serde_json::json!({
                "type": "message",
                "timestamp": "2026-07-31T12:00:00Z",
                "message": {"role": "user", "content": body},
            });
            let event = normalization::event_fact(ordinal as u64, ordinal + 1, &value, occurred_at);
            let (native_item_key, _) =
                native_event_keys(None, &value, &event, &source, session_id, &mut state).unwrap();
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
    body: &str,
    ordinal: u64,
    source: &SourceKey,
    session_id: StableEntityId,
    state: &mut FallbackEventIdentityState,
) -> (StableEntityId, TypedKey) {
    let occurred_at = "2026-07-31T12:00:00Z".parse::<DateTime<Utc>>().unwrap();
    let value = serde_json::json!({
        "type": "message",
        "timestamp": "2026-07-31T12:00:00Z",
        "message": {"role": "user", "content": body},
    });
    let event = normalization::event_fact(
        ordinal,
        usize::try_from(ordinal).unwrap() + 1,
        &value,
        occurred_at,
    );
    let (native_item_key, native_event_id) =
        native_event_keys(None, &value, &event, source, session_id, state).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .unwrap();
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
            "OpenClaw fallback lookup test",
        )
        .unwrap();
        record.provider_session_id = Some("main/fallback-session".to_owned());
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
fn native_tool_call_result_and_spawned_family_are_exact() {
    let lines = HISTORY
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let calls = native_tool_calls(&lines[1]);
    let call = calls.first().unwrap();
    assert_eq!(call.call_id, Some("call-1"));
    assert_eq!(call.command.as_deref(), Some("git commit -m exact"));
    assert_eq!(call.declared_workdir.as_deref(), Some("/tmp/repository"));

    let result = native_tool_result(&lines[2]).unwrap();
    assert_eq!(result.call_id, Some("call-1"));
    assert_eq!(result.output_workdir, Some("/tmp/repository"));
    assert_eq!(
        openclaw_output_metadata(&lines[2]).unwrap().outcome.outcome,
        OutputOutcome::Success
    );

    let index = serde_json::from_str::<Value>(SESSIONS).unwrap();
    assert_eq!(
        native_session_family(
            Path::new("/agents/worker/sessions/child-session.jsonl"),
            &index
        ),
        (
            Some("main/parent-session".to_owned()),
            Some("main/parent-session".to_owned())
        )
    );
}

#[test]
fn every_tool_call_block_projects_with_a_stable_selector() {
    let value = serde_json::json!({
        "type": "message",
        "id": "multi-call-record",
        "timestamp": "2026-07-31T12:00:00Z",
        "message": {
            "role": "assistant",
            "content": [
                {"type": "text", "text": "before"},
                {"type": "toolCall", "id": "call-a", "name": "read_file", "arguments": {"path": "a.rs"}},
                {"type": "text", "text": "between"},
                {"type": "toolCall", "id": "call-b", "name": "write_file", "arguments": {"path": "b.rs"}}
            ]
        }
    });
    let bytes = serde_json::to_vec(&value).unwrap();
    let (_temp, mut projector) = test_projector();
    let mut emitted = Vec::new();
    projector
        .project(JsonlRecordRef::for_test(&bytes, 0), &mut |record| {
            emitted.push(record);
            Ok(())
        })
        .unwrap();

    assert_eq!(emitted.len(), 2);
    assert_ne!(emitted[0].event_id, emitted[1].event_id);
    assert_ne!(emitted[0].native_event_id, emitted[1].native_event_id);
    assert_eq!(emitted[0].event_sequence, 1);
    assert_eq!(emitted[1].event_sequence, 3);
    assert!(emitted[0]
        .content
        .normalized_body
        .as_deref()
        .is_some_and(|body| body.contains("call-a")));
    assert!(emitted[1]
        .content
        .normalized_body
        .as_deref()
        .is_some_and(|body| body.contains("call-b")));

    let (_temp, mut replay) = test_projector();
    let mut replayed = Vec::new();
    replay
        .project(JsonlRecordRef::for_test(&bytes, 0), &mut |record| {
            replayed.push(record.event_id);
            Ok(())
        })
        .unwrap();
    assert_eq!(
        emitted
            .iter()
            .map(|record| record.event_id)
            .collect::<Vec<_>>(),
        replayed
    );
}

#[test]
fn running_result_is_emitted_before_continuation_state_is_checkpointed() {
    let call = serde_json::to_vec(&serde_json::json!({
        "type": "message",
        "id": "running-call-record",
        "timestamp": "2026-07-31T12:00:00Z",
        "message": {"role": "assistant", "content": [{
            "type": "toolCall",
            "id": "running-call",
            "name": "exec",
            "arguments": {"command": "git status", "workdir": "/tmp/project"}
        }]}
    }))
    .unwrap();
    let running = serde_json::to_vec(&serde_json::json!({
        "type": "message",
        "id": "running-result-record",
        "timestamp": "2026-07-31T12:00:01Z",
        "message": {
            "role": "toolResult",
            "toolCallId": "running-call",
            "content": "partial exact output",
            "details": {"status": "running", "sessionId": "process-1"}
        }
    }))
    .unwrap();
    let (_temp, mut projector) = test_projector();
    let mut emitted = Vec::new();
    for (ordinal, bytes) in [&call, &running].into_iter().enumerate() {
        projector
            .project(
                JsonlRecordRef::for_test(bytes, ordinal as u64),
                &mut |record| {
                    emitted.push(record);
                    Ok(())
                },
            )
            .unwrap();
    }

    assert_eq!(emitted.len(), 2);
    assert!(emitted[1]
        .content
        .normalized_body
        .as_deref()
        .is_some_and(|body| body.contains("partial exact output")));
    assert!(projector.running_processes.contains_key("process-1"));
    assert!(encode_projector_checkpoint(&projector).is_ok());
}

#[test]
fn checkpoint_byte_overflow_degrades_to_typed_linkage_capacity() {
    let (_temp, mut projector) = test_projector();
    projector.remember_state(
        projection::StateBucket::Pending,
        "oversized-call",
        PendingCallState::Exact(PendingCall {
            origin_call_id: "oversized-call".to_owned(),
            command: Some("x".repeat(MAX_PROJECTOR_CHECKPOINT_BYTES)),
            declared_workdir: Some("/tmp/project".to_owned()),
            event_sequence: 1,
            continuation_call_id_sha256: Vec::new(),
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
fn duplicate_call_ids_are_ambiguous_and_result_linkage_abstains() {
    let mut pending_calls = HashMap::new();
    let mut capacity_exceeded = false;
    for command in ["git commit -m first", "git commit -m second"] {
        remember_pending_call(
            &mut pending_calls,
            &mut capacity_exceeded,
            MAX_PENDING_CALLS,
            "duplicate-call",
            PendingCallState::Exact(PendingCall {
                origin_call_id: "duplicate-call".to_owned(),
                command: Some(command.to_owned()),
                declared_workdir: Some("/tmp/repository".to_owned()),
                event_sequence: 1,
                continuation_call_id_sha256: Vec::new(),
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
            && abstention.detail.as_deref() == Some("openclaw_tool_result_call_id_is_ambiguous")
    }));

    let success = serde_json::json!({
        "type": "message",
        "message": {
            "role": "tool",
            "toolCallId": "success",
            "content": "ok",
            "details": {"exitCode": 0},
        },
    });
    let failure = serde_json::json!({
        "type": "message",
        "message": {
            "role": "tool",
            "toolCallId": "failure",
            "content": "failed",
            "details": {"exitCode": 1},
        },
    });
    let success = openclaw_output_metadata(&success).unwrap().outcome.outcome;
    let failure = openclaw_output_metadata(&failure).unwrap().outcome.outcome;
    assert_eq!(success, OutputOutcome::Success);
    assert_eq!(failure, OutputOutcome::Failure);
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
    let temp = tempfile::tempdir().unwrap();
    let authority = Arc::new(ProviderSourceRoot::open(temp.path()).unwrap());
    let native_session_id = "main/fallback-session";
    let binding = Binding {
        index_relative_path: PathBuf::from("sessions.json"),
        native_session_id: native_session_id.to_owned(),
        index: Value::Null,
        parent_native_session_id: None,
        root_native_session_id: None,
    };
    let source = source_key(native_session_id).unwrap();
    let session_id = session_identity(&source, native_session_id).unwrap();
    let source_path = PathBuf::from("/agents/main/sessions/fallback-session.jsonl");
    let session = SessionState::new(
        &source_path,
        native_session_id,
        &binding.index,
        None,
        None,
        DateTime::<Utc>::UNIX_EPOCH,
        session_id,
    )
    .unwrap();
    let mut cold_identity_state = FallbackEventIdentityState::default();
    let prefix_events = [0, 1]
        .into_iter()
        .map(|ordinal| {
            fallback_event_id(
                "duplicate",
                ordinal,
                &source,
                session_id,
                &mut cold_identity_state,
            )
        })
        .collect::<Vec<_>>();
    let (_base, base_lookup) = base_lookup_with_events(&source, session_id, &prefix_events);
    let mut pending_calls = HashMap::new();
    let mut linkage_capacity_exceeded = false;
    remember_pending_call(
        &mut pending_calls,
        &mut linkage_capacity_exceeded,
        MAX_PENDING_CALLS,
        "cross-append-call",
        PendingCallState::Exact(PendingCall {
            origin_call_id: "cross-append-call".to_owned(),
            command: Some("git commit -m prefix".to_owned()),
            declared_workdir: Some("/tmp/project".to_owned()),
            event_sequence: 0,
            continuation_call_id_sha256: Vec::new(),
        }),
    );
    let mut projector = OpenClawProjector {
        source: source.clone(),
        native_session_id: native_session_id.to_owned(),
        session_id,
        session,
        index_file: None,
        authority,
        attributor: RepositoryAttributor::default(),
        pending_calls,
        running_processes: HashMap::new(),
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

    let mut append_identity_state = FallbackEventIdentityState::new(Some(base_lookup));
    let suffix_event = fallback_event_id(
        "duplicate",
        2,
        &source,
        session_id,
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
        MAX_PENDING_CALLS,
        "cross-append-call",
        PendingCallState::Exact(PendingCall {
            origin_call_id: "cross-append-call".to_owned(),
            command: Some("git commit -m suffix".to_owned()),
            declared_workdir: Some("/tmp/project".to_owned()),
            event_sequence: 1_u64 << 16,
            continuation_call_id_sha256: Vec::new(),
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
            "openclaw_tool_result_call_id_is_ambiguous"
        )]
    );
}

#[test]
fn successful_textual_result_over_16k_remains_in_native_core_body() {
    let tail = "openclaw_success_result_tail_complete";
    let output = format!("{} {tail}", "successful openclaw output ".repeat(700));
    assert!(output.len() > 16_000);
    let value = serde_json::json!({
        "type": "message",
        "id": "complete-result",
        "timestamp": "2026-07-31T12:00:02Z",
        "message": {
            "role": "toolResult",
            "toolCallId": "complete-call",
            "toolName": "exec",
            "content": [{"type": "text", "text": output}],
            "details": {"status": "completed", "exitCode": 0}
        }
    });
    let result = native_tool_result(&value).unwrap();
    let body = serde_json::to_string(result.message).unwrap();

    assert!(body.len() > 16_000);
    assert!(body.contains(tail));
    assert_eq!(result.message["content"][0]["text"], output);
}
