use super::*;
use crate::{
    provider::source_backed::{
        refresh_source_backed_generation, register_landed_source_backed_route,
        SourceBackedProviderRegistry, SourceBackedRouteSelection,
    },
    repository_attribution::RepositoryAttributor,
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceStatus,
};
use ctx_history_core::{
    CertifiedSource, EventOrigin, RepositoryFileInvocationKind, ScannedSourceCounts,
    SourceObservation,
};
use ctx_history_index::{GenerationWriter, VerifiedIndex, WriterOptions};
#[cfg(unix)]
use std::process::Command;
use std::{fs::OpenOptions, io::Write};

const HISTORY: &str =
    include_str!("../../../../../../tests/fixtures/repository_attribution/openclaw-native.jsonl");
const SESSIONS: &str =
    include_str!("../../../../../../tests/fixtures/repository_attribution/openclaw-sessions.json");

fn openclaw_lifecycle_transcript(root: &Path) -> PathBuf {
    root.join("agents/main/sessions/lifecycle.jsonl")
}

fn write_openclaw_lifecycle_transcript(path: &Path, records: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, record).unwrap();
        bytes.push(b'\n');
    }
    fs::write(path, bytes).unwrap();
}

fn append_openclaw_lifecycle_transcript(path: &Path, record: &Value) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    serde_json::to_writer(&mut file, record).unwrap();
    file.write_all(b"\n").unwrap();
    file.sync_all().unwrap();
}

fn openclaw_lifecycle_registry(root: &Path) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        ProviderSource {
            provider: CaptureProvider::OpenClaw,
            path: root.to_path_buf(),
            exists: true,
            source_format: OPENCLAW_SOURCE_FORMAT,
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
        },
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    registry
}

fn openclaw_lifecycle_writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

fn indexed_openclaw_lifecycle_records(index: &Path, transcript: &Path) -> Vec<CoreRecord> {
    let source = source_key(&native_session_id(transcript)).unwrap();
    VerifiedIndex::open(index)
        .unwrap()
        .core_source_event_page(&source, None, 64)
        .unwrap()
        .items
        .into_iter()
        .map(|item| item.core_record)
        .collect()
}

#[test]
fn openclaw_cold_noop_and_append_refreshes_preserve_the_source() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("openclaw");
    let transcript = openclaw_lifecycle_transcript(&root);
    let first = serde_json::json!({
        "type": "message",
        "id": "lifecycle-first",
        "timestamp": "2026-08-06T12:00:00Z",
        "message": {"role": "user", "content": "OpenClaw cold record"}
    });
    let second = serde_json::json!({
        "type": "message",
        "id": "lifecycle-second",
        "timestamp": "2026-08-06T12:00:01Z",
        "message": {"role": "assistant", "content": "OpenClaw appended record"}
    });
    write_openclaw_lifecycle_transcript(&transcript, &[first]);
    let registry = openclaw_lifecycle_registry(&root);
    let index = temp.path().join("index");

    let cold =
        refresh_source_backed_generation(&index, &registry, openclaw_lifecycle_writer_options())
            .unwrap();
    assert_eq!(cold.sources.len(), 1);
    assert_eq!(cold.sources[0].counts().complete_records, 1);
    let cold_records = indexed_openclaw_lifecycle_records(&index, &transcript);
    assert_eq!(cold_records.len(), 1);

    let noop =
        refresh_source_backed_generation(&index, &registry, openclaw_lifecycle_writer_options())
            .unwrap();
    assert_eq!(noop.sources.len(), 1);
    assert_eq!(noop.sources[0].counts().complete_records, 1);
    assert_eq!(
        indexed_openclaw_lifecycle_records(&index, &transcript),
        cold_records
    );

    append_openclaw_lifecycle_transcript(&transcript, &second);
    let appended =
        refresh_source_backed_generation(&index, &registry, openclaw_lifecycle_writer_options())
            .unwrap();
    assert_eq!(appended.sources.len(), 1);
    assert_eq!(appended.sources[0].counts().complete_records, 2);
    let appended_records = indexed_openclaw_lifecycle_records(&index, &transcript);
    assert_eq!(appended_records.len(), 2);
    assert!(appended_records.iter().any(|record| {
        record.content.normalized_body.as_deref() == Some("OpenClaw appended record")
    }));
}

#[test]
fn openclaw_discovered_leaf_rejects_mutation_before_scan() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("openclaw");
    let transcript = openclaw_lifecycle_transcript(&root);
    let original = serde_json::json!({
        "type": "message",
        "id": "mutation-original",
        "message": {"role": "user", "content": "original"}
    });
    let mutated = serde_json::json!({
        "type": "message",
        "id": "mutation-replacement",
        "message": {"role": "user", "content": "mutated after discovery"}
    });
    write_openclaw_lifecycle_transcript(&transcript, &[original]);
    let adapter = openclaw_source_backed_adapter_v0();
    let inventory = adapter.discover(&root).unwrap();
    let leaf = inventory.leaves().first().unwrap();

    write_openclaw_lifecycle_transcript(&transcript, &[mutated]);

    assert!(matches!(
        leaf.open_verified(),
        Err(CaptureError::SourceChangedDuringCapture)
    ));
}

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
        &OpenClawNativeSessionFamily::Absent,
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
            pending_calls: HashMap::new(),
            running_processes: HashMap::new(),
            terminal_authority: OpenClawTerminalAuthority::unscanned_for_test(),
            linkage_capacity_exceeded: false,
            fallback_identities: FallbackEventIdentityState::default(),
        },
    )
}

fn project_session_event(
    path: &Path,
    native_session_id: &str,
    index: &Value,
    native_session_family: &OpenClawNativeSessionFamily,
) -> CoreRecord {
    let temp = tempfile::tempdir().unwrap();
    let authority = Arc::new(ProviderSourceRoot::open(temp.path()).unwrap());
    let source = source_key(native_session_id).unwrap();
    let session_id = session_identity(&source, native_session_id).unwrap();
    let session = SessionState::new(
        path,
        native_session_id,
        index,
        native_session_family,
        DateTime::<Utc>::UNIX_EPOCH,
        session_id,
    )
    .unwrap();
    let mut projector = OpenClawProjector {
        source,
        native_session_id: native_session_id.to_owned(),
        session_id,
        session,
        index_file: None,
        authority,
        pending_calls: HashMap::new(),
        running_processes: HashMap::new(),
        terminal_authority: OpenClawTerminalAuthority::unscanned_for_test(),
        linkage_capacity_exceeded: false,
        fallback_identities: FallbackEventIdentityState::default(),
    };
    let value = serde_json::json!({
        "type": "message",
        "id": "child-event",
        "timestamp": "2026-08-05T12:00:00Z",
        "message": {"role": "user", "content": "exact child-owned OpenClaw event"}
    });
    let bytes = serde_json::to_vec(&value).unwrap();
    let mut worker = JsonlFamilyWorkerContext::default();
    let mut emitted = Vec::new();
    projector
        .project(
            JsonlRecordRef::for_test(&bytes, 0),
            &mut worker,
            &mut |record| {
                emitted.push(record);
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(emitted.len(), 1);
    emitted.pop().unwrap()
}

fn project_values(projector: &mut OpenClawProjector, values: &[Value]) -> Vec<CoreRecord> {
    projector.terminal_authority = terminal_authority_for_values(values);
    let mut worker = JsonlFamilyWorkerContext::default();
    let mut emitted = Vec::new();
    for (ordinal, value) in values.iter().enumerate() {
        let bytes = serde_json::to_vec(value).unwrap();
        projector
            .project(
                JsonlRecordRef::for_test(&bytes, ordinal as u64),
                &mut worker,
                &mut |record| {
                    emitted.push(record);
                    Ok(())
                },
            )
            .unwrap();
    }
    emitted
}

fn retrieval_excluded(record: &CoreRecord) -> bool {
    record.content.discovery_exclusion
        == Some(ctx_history_core::CoreDiscoveryExclusion::CtxRetrievalDerived)
}

#[test]
fn openclaw_exact_cli_and_success_envelope_are_excluded() {
    let (_temp, mut projector) = test_projector();
    let records = project_values(
        &mut projector,
        &[
            serde_json::json!({
                "type": "message",
                "id": "retrieval-call-record",
                "timestamp": "2026-08-05T12:00:00Z",
                "message": {"role": "assistant", "content": [{
                    "type": "toolCall",
                    "id": "retrieval-call",
                    "name": "exec",
                    "arguments": {"command": "ctx search needle"}
                }]}
            }),
            serde_json::json!({
                "type": "message",
                "id": "retrieval-result-record",
                "timestamp": "2026-08-05T12:00:01Z",
                "message": {
                    "role": "toolResult",
                    "toolCallId": "retrieval-call",
                    "content": "exact OpenClaw payload",
                    "details": {"status": "completed", "exitCode": 0}
                }
            }),
        ],
    );

    assert_eq!(records.len(), 2);
    assert!(records.iter().all(retrieval_excluded));
    assert_eq!(
        records[1].content.normalized_body.as_deref(),
        Some("exact OpenClaw payload")
    );
}

#[test]
fn openclaw_mixed_diagnostic_and_unknown_shapes_fail_open() {
    let (_temp, mut projector) = test_projector();
    let records = project_values(
        &mut projector,
        &[
            serde_json::json!({
                "type": "message",
                "id": "mixed-call-record",
                "timestamp": "2026-08-05T12:00:00Z",
                "message": {"role": "assistant", "content": [
                    {"type": "text", "text": "ordinary sibling"},
                    {"type": "toolCall", "id": "mixed-call", "name": "exec", "arguments": {"command": "ctx search needle"}}
                ]}
            }),
            serde_json::json!({
                "type": "message",
                "id": "mixed-result-record",
                "timestamp": "2026-08-05T12:00:01Z",
                "message": {"role": "toolResult", "toolCallId": "mixed-call", "content": "mixed payload", "details": {"status": "completed", "exitCode": 0}}
            }),
            serde_json::json!({
                "type": "message",
                "id": "diagnostic-call-record",
                "timestamp": "2026-08-05T12:00:02Z",
                "message": {"role": "assistant", "content": [{"type": "toolCall", "id": "diagnostic-call", "name": "exec", "arguments": {"command": "ctx show event deadbeef"}}]}
            }),
            serde_json::json!({
                "type": "message",
                "id": "diagnostic-result-record",
                "timestamp": "2026-08-05T12:00:03Z",
                "message": {"role": "toolResult", "toolCallId": "diagnostic-call", "content": "kept diagnostic", "details": {"status": "completed", "exitCode": 0, "stderr": "warning"}}
            }),
            serde_json::json!({
                "type": "message",
                "id": "unknown-call-record",
                "timestamp": "2026-08-05T12:00:04Z",
                "message": {"role": "assistant", "content": [{"type": "toolCall", "id": "unknown-call", "name": "exec", "arguments": {"command": "ctx search another"}}]}
            }),
            serde_json::json!({
                "type": "message",
                "id": "unknown-result-record",
                "timestamp": "2026-08-05T12:00:05Z",
                "message": {"role": "toolResult", "toolCallId": "unknown-call", "content": "kept unknown", "details": {"status": "completed", "exitCode": 0, "mystery": true}}
            }),
            serde_json::json!({
                "type": "message",
                "id": "alias-call-record",
                "timestamp": "2026-08-05T12:00:06Z",
                "message": {"role": "assistant", "content": [{"type": "toolCall", "id": "alias-call", "name": "exec", "arguments": {"command": "ctx search alias-ambiguity"}}]}
            }),
            serde_json::json!({
                "type": "message",
                "id": "alias-result-record",
                "timestamp": "2026-08-05T12:00:07Z",
                "message": {"role": "toolResult", "toolCallId": "alias-call", "tool_call_id": "conflicting-call", "content": "kept alias ambiguity", "details": {"status": "completed", "exitCode": 0}}
            }),
        ],
    );

    assert_eq!(records.len(), 8);
    assert!(!retrieval_excluded(&records[0]));
    assert!(!retrieval_excluded(&records[1]));
    assert!(retrieval_excluded(&records[2]));
    assert!(!retrieval_excluded(&records[3]));
    assert!(retrieval_excluded(&records[4]));
    assert!(!retrieval_excluded(&records[5]));
    assert!(retrieval_excluded(&records[6]));
    assert!(!retrieval_excluded(&records[7]));
    assert_eq!(
        records[3].content.normalized_body.as_deref(),
        Some("kept diagnostic")
    );
    assert_eq!(
        records[5].content.normalized_body.as_deref(),
        Some("kept unknown")
    );
    assert_eq!(
        records[7].content.normalized_body.as_deref(),
        Some("kept alias ambiguity")
    );
}

#[test]
fn openclaw_running_retrieval_survives_checkpoint_and_continuation_linkage() {
    let (_temp, mut projector) = test_projector();
    let prefix = project_values(
        &mut projector,
        &[
            serde_json::json!({
                "type": "message",
                "id": "running-call-record",
                "timestamp": "2026-08-05T12:00:00Z",
                "message": {"role": "assistant", "content": [{"type": "toolCall", "id": "running-call", "name": "exec", "arguments": {"command": "ctx search needle"}}]}
            }),
            serde_json::json!({
                "type": "message",
                "id": "running-result-record",
                "timestamp": "2026-08-05T12:00:01Z",
                "message": {"role": "toolResult", "toolCallId": "running-call", "content": "partial payload", "details": {"status": "running", "sessionId": "process-1"}}
            }),
        ],
    );
    assert!(retrieval_excluded(&prefix[0]));
    assert!(!retrieval_excluded(&prefix[1]));

    let checkpoint = encode_projector_checkpoint(&projector).unwrap();
    let binding = Binding {
        index_relative_path: PathBuf::from("sessions.json"),
        native_session_id: projector.native_session_id.clone(),
        index: Value::Null,
        native_session_family: OpenClawNativeSessionFamily::Absent,
        terminal_ambiguity_fingerprint: OpenClawTerminalAuthority::for_scan()
            .ambiguity_fingerprint(),
    };
    let restored = decode_projector_checkpoint(&checkpoint, &binding).unwrap();
    assert!(matches!(
        restored.running_processes.get("process-1"),
        Some(PendingCallState::Exact(PendingCall {
            retrieval_contribution: StoredContributionClass::RetrievalDerived,
            ..
        }))
    ));
    projector.pending_calls = restored.pending_calls;
    projector.running_processes = restored.running_processes;
    projector.linkage_capacity_exceeded = restored.linkage_capacity_exceeded;

    let suffix = project_values(
        &mut projector,
        &[
            serde_json::json!({
                "type": "message",
                "id": "continuation-call-record",
                "timestamp": "2026-08-05T12:00:02Z",
                "message": {"role": "assistant", "content": [{"type": "toolCall", "id": "continuation-call", "name": "process", "arguments": {"sessionId": "process-1"}}]}
            }),
            serde_json::json!({
                "type": "message",
                "id": "continuation-result-record",
                "timestamp": "2026-08-05T12:00:03Z",
                "message": {"role": "toolResult", "toolCallId": "continuation-call", "content": "final payload", "details": {"status": "completed", "exitCode": 0}}
            }),
        ],
    );
    assert_eq!(suffix.len(), 2);
    assert!(suffix.iter().all(retrieval_excluded));
    assert_eq!(
        suffix[1].content.normalized_body.as_deref(),
        Some("final payload")
    );
    assert!(!projector.running_processes.contains_key("process-1"));
    let retired_checkpoint = encode_projector_checkpoint(&projector).unwrap();
    let retired = decode_projector_checkpoint(&retired_checkpoint, &binding).unwrap();
    assert!(retired.running_processes.is_empty());
}

#[test]
fn openclaw_foreign_tool_cannot_inherit_running_retrieval_by_session_id() {
    let (_temp, mut projector) = test_projector();
    let records = project_values(
        &mut projector,
        &[
            serde_json::json!({
                "type": "message",
                "id": "running-call-record",
                "timestamp": "2026-08-05T12:00:00Z",
                "message": {"role": "assistant", "content": [{"type": "toolCall", "id": "running-call", "name": "exec", "arguments": {"command": "ctx search needle"}}]}
            }),
            serde_json::json!({
                "type": "message",
                "id": "running-result-record",
                "timestamp": "2026-08-05T12:00:01Z",
                "message": {"role": "toolResult", "toolCallId": "running-call", "content": "partial payload", "details": {"status": "running", "sessionId": "process-1"}}
            }),
            serde_json::json!({
                "type": "message",
                "id": "foreign-call-record",
                "timestamp": "2026-08-05T12:00:02Z",
                "message": {"role": "assistant", "content": [{"type": "toolCall", "id": "foreign-call", "name": "custom", "arguments": {"sessionId": "process-1"}}]}
            }),
            serde_json::json!({
                "type": "message",
                "id": "foreign-result-record",
                "timestamp": "2026-08-05T12:00:03Z",
                "message": {"role": "toolResult", "toolCallId": "foreign-call", "content": "foreign payload", "details": {"status": "completed", "exitCode": 0}}
            }),
        ],
    );

    assert_eq!(records.len(), 4);
    assert!(retrieval_excluded(&records[0]));
    assert!(!retrieval_excluded(&records[1]));
    assert!(!retrieval_excluded(&records[2]));
    assert!(!retrieval_excluded(&records[3]));
    assert!(projector.running_processes.contains_key("process-1"));
}

#[test]
fn openclaw_unattested_command_tool_aliases_fail_open() {
    let values = ["exec_command", "shell", "bash", "command"]
        .into_iter()
        .enumerate()
        .flat_map(|(index, name)| {
            [
                serde_json::json!({
                    "type": "message",
                    "id": format!("alias-call-record-{index}"),
                    "timestamp": "2026-08-05T12:00:00Z",
                    "message": {"role": "assistant", "content": [{"type": "toolCall", "id": format!("alias-call-{index}"), "name": name, "arguments": {"command": "ctx search needle"}}]}
                }),
                serde_json::json!({
                    "type": "message",
                    "id": format!("alias-result-record-{index}"),
                    "timestamp": "2026-08-05T12:00:01Z",
                    "message": {"role": "toolResult", "toolCallId": format!("alias-call-{index}"), "content": format!("alias payload {index}"), "details": {"status": "completed", "exitCode": 0}}
                }),
            ]
        })
        .collect::<Vec<_>>();
    let (_temp, mut projector) = test_projector();
    let records = project_values(&mut projector, &values);

    assert_eq!(records.len(), values.len());
    assert!(records.iter().all(|record| !retrieval_excluded(record)));
}

#[cfg(unix)]
fn run_git(path: &Path, arguments: &[&str]) {
    let status = Command::new("/usr/bin/git")
        .arg("-C")
        .arg(path)
        .args(arguments)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .status()
        .unwrap();
    assert!(status.success(), "git {arguments:?} failed");
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
    let mut writer = GenerationWriter::open(temp.path(), options.clone())
        .unwrap()
        .into_writer()
        .unwrap();
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
    let writer = GenerationWriter::open(temp.path(), options)
        .unwrap()
        .into_writer()
        .unwrap();
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
        OpenClawNativeSessionFamily::Resolved {
            parent_native_session_id: "main/parent-session".to_owned(),
            root_native_session_id: "main/parent-session".to_owned(),
        }
    );
}

#[test]
fn provider_p1_lineage_malformed_dangling_and_cyclic_spawned_by_are_invalid() {
    let path = Path::new("/agents/worker/sessions/child-session.jsonl");
    for index in [
        serde_json::json!({
            "agent:worker:child": {
                "sessionId": "child-session",
                "spawnedBy": 7
            }
        }),
        serde_json::json!({
            "agent:worker:child": {
                "sessionId": "child-session",
                "spawnedBy": "missing-parent-key"
            }
        }),
        serde_json::json!({
            "agent:worker:child": {
                "sessionId": "child-session",
                "spawnedBy": "agent:main:parent"
            },
            "agent:main:parent": {
                "sessionId": "parent-session",
                "spawnedBy": "agent:worker:child"
            }
        }),
    ] {
        assert_eq!(
            native_session_family(path, &index),
            OpenClawNativeSessionFamily::Invalid
        );
    }
}

#[test]
fn provider_p1_lineage_invalid_spawned_by_without_parent_fails_closed() {
    let path = Path::new("/agents/worker/sessions/child-session.jsonl");
    let native_session_id = "worker/child-session";
    let source = source_key(native_session_id).unwrap();
    let session_id = session_identity(&source, native_session_id).unwrap();

    let error = SessionState::new(
        path,
        native_session_id,
        &Value::Null,
        &OpenClawNativeSessionFamily::Invalid,
        DateTime::<Utc>::UNIX_EPOCH,
        session_id,
    )
    .err()
    .expect("invalid spawnedBy must not publish a root");

    assert!(error.to_string().contains("without a resolvable parent"));
}

#[test]
fn spawned_children_are_delegated_unique_while_generic_parents_stay_unknown() {
    let spawned_index = serde_json::from_str::<Value>(SESSIONS).unwrap();
    let spawned_path = Path::new("/agents/worker/sessions/child-session.jsonl");
    let spawned_family = native_session_family(spawned_path, &spawned_index);
    let spawned = project_session_event(
        spawned_path,
        "worker/child-session",
        &Value::Null,
        &spawned_family,
    );
    assert_eq!(
        spawned.session_relationship,
        SessionRelationshipKind::Delegated
    );
    assert_eq!(spawned.event_origin, EventOrigin::UniqueToSession);
    assert!(!spawned.is_primary);
    assert_eq!(
        spawned.content.meaningful_text(),
        "exact child-owned OpenClaw event"
    );
    assert_eq!(
        spawned.native_event_id,
        Some(TypedKey::utf8("child-event").unwrap())
    );

    let generic = project_session_event(
        Path::new("/agents/worker/sessions/generic-child.jsonl"),
        "worker/generic-child",
        &serde_json::json!({
            "parentSessionId": "generic-parent",
            "rootSessionId": "generic-root"
        }),
        &OpenClawNativeSessionFamily::Absent,
    );
    assert_eq!(
        generic.session_relationship,
        SessionRelationshipKind::RelatedUnknown
    );
    assert_eq!(generic.event_origin, EventOrigin::Unknown);
    assert!(generic.is_primary);
    assert_eq!(
        generic.content.meaningful_text(),
        "exact child-owned OpenClaw event"
    );
    assert_eq!(
        generic.native_event_id,
        Some(TypedKey::utf8("child-event").unwrap())
    );

    let unsupported_fork_source = project_session_event(
        Path::new("/agents/worker/sessions/sqlite-fork-lookalike.jsonl"),
        "worker/sqlite-fork-lookalike",
        &serde_json::json!({
            "forkSource": {
                "sessionId": "unsupported-sqlite-parent",
                "entryId": "unsupported-sqlite-entry"
            }
        }),
        &OpenClawNativeSessionFamily::Absent,
    );
    assert_eq!(
        unsupported_fork_source.session_relationship,
        SessionRelationshipKind::Root
    );
    assert_eq!(unsupported_fork_source.event_origin, EventOrigin::Unknown);
    assert!(unsupported_fork_source.parent_session_id.is_none());
}

#[test]
fn provider_p1_lineage_contradictory_native_and_generic_authorities_are_unknown() {
    let record = project_session_event(
        Path::new("/agents/worker/sessions/child-session.jsonl"),
        "worker/child-session",
        &serde_json::json!({
            "parentSessionId": "generic-parent",
            "rootSessionId": "generic-root"
        }),
        &OpenClawNativeSessionFamily::Resolved {
            parent_native_session_id: "main/native-parent".to_owned(),
            root_native_session_id: "main/native-root".to_owned(),
        },
    );

    assert_eq!(
        record.session_relationship,
        SessionRelationshipKind::RelatedUnknown
    );
    assert_eq!(record.event_origin, EventOrigin::Unknown);
    assert!(record.is_primary);
    assert!(record.parent_session_id.is_some());
}

#[test]
fn provider_p1_lineage_contradictory_generic_parent_aliases_are_unknown() {
    let record = project_session_event(
        Path::new("/agents/worker/sessions/generic-child.jsonl"),
        "worker/generic-child",
        &serde_json::json!({
            "parentSessionId": "first-parent",
            "parent_session_id": "second-parent"
        }),
        &OpenClawNativeSessionFamily::Absent,
    );

    assert_eq!(
        record.session_relationship,
        SessionRelationshipKind::RelatedUnknown
    );
    assert_eq!(record.event_origin, EventOrigin::Unknown);
    assert!(record.is_primary);
    assert!(record.parent_session_id.is_some());
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
    let mut worker = JsonlFamilyWorkerContext::default();
    let mut emitted = Vec::new();
    projector
        .project(
            JsonlRecordRef::for_test(&bytes, 0),
            &mut worker,
            &mut |record| {
                emitted.push(record);
                Ok(())
            },
        )
        .unwrap();

    assert_eq!(emitted.len(), 2);
    assert_ne!(emitted[0].event_id, emitted[1].event_id);
    assert_ne!(emitted[0].native_event_id, emitted[1].native_event_id);
    assert_eq!(emitted[0].event_sequence, 1);
    assert_eq!(emitted[1].event_sequence, 3);
    let calls = native_tool_calls(&value);
    let first = strict_tool_call_projection(calls[0].block, calls[0].block_index as u64).unwrap();
    let second = strict_tool_call_projection(calls[1].block, calls[1].block_index as u64).unwrap();
    let [read] = first.file_invocations.as_slice() else {
        panic!("expected one exact read invocation");
    };
    let [write] = second.file_invocations.as_slice() else {
        panic!("expected one exact write invocation");
    };
    assert_eq!(read.operation_ordinal, 1);
    assert_eq!(read.tool_name.as_deref(), Some("read_file"));
    assert_eq!(read.path, "a.rs");
    assert_eq!(read.kind, RepositoryFileInvocationKind::Read);
    assert_eq!(write.operation_ordinal, 3);
    assert_eq!(write.tool_name.as_deref(), Some("write_file"));
    assert_eq!(write.path, "b.rs");
    assert_eq!(write.kind, RepositoryFileInvocationKind::Write);
    for (projected, invocation) in [(&first, read), (&second, write)] {
        let range = invocation.normalized_text_range.unwrap();
        assert_eq!(
            &projected.normalized_body[range.start as usize..range.end as usize],
            projected.normalized_body
        );
    }
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
    let mut replay_worker = JsonlFamilyWorkerContext::default();
    let mut replayed = Vec::new();
    replay
        .project(
            JsonlRecordRef::for_test(&bytes, 0),
            &mut replay_worker,
            &mut |record| {
                replayed.push(record.event_id);
                Ok(())
            },
        )
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
fn strict_tool_call_ambiguity_rename_and_overflow_abstain_without_narrowing_observations() {
    let ambiguous = serde_json::json!({
        "type": "toolCall",
        "id": "ambiguous",
        "name": "edit_file",
        "arguments": {"path": "src/a.rs", "file_path": "src/b.rs"}
    });
    let ambiguous_record = serde_json::json!({
        "message": {"content": [ambiguous.clone()]}
    });
    let call = native_tool_calls(&ambiguous_record).pop().unwrap();
    assert_eq!(call.file_observations.len(), 2);
    let projected = strict_tool_call_projection(call.block, call.block_index as u64).unwrap();
    assert!(projected.file_invocations.is_empty());
    assert_eq!(
        projected.abstention,
        Some(StrictInvocationAbstention::Opaque)
    );

    let duplicate_same = serde_json::json!({
        "type": "toolCall",
        "name": "edit_file",
        "arguments": {"path": "src/a.rs", "file_path": "src/a.rs"}
    });
    let projected = strict_tool_call_projection(&duplicate_same, 1).unwrap();
    assert!(projected.file_invocations.is_empty());
    assert_eq!(
        projected.abstention,
        Some(StrictInvocationAbstention::Opaque)
    );

    let rename = serde_json::json!({
        "type": "toolCall",
        "name": "rename_file",
        "arguments": {"oldPath": "src/old.rs", "newPath": "src/new.rs"}
    });
    let projected = strict_tool_call_projection(&rename, 7).unwrap();
    let [invocation] = projected.file_invocations.as_slice() else {
        panic!("expected one exact rename invocation");
    };
    assert_eq!(invocation.operation_ordinal, 7);
    assert_eq!(invocation.path, "src/new.rs");
    assert_eq!(invocation.prior_path.as_deref(), Some("src/old.rs"));
    assert_eq!(invocation.kind, RepositoryFileInvocationKind::Rename);

    let incomplete_rename = serde_json::json!({
        "type": "toolCall",
        "name": "rename_file",
        "arguments": {"newPath": "src/new.rs"}
    });
    let projected = strict_tool_call_projection(&incomplete_rename, 8).unwrap();
    assert!(projected.file_invocations.is_empty());
    assert_eq!(
        projected.abstention,
        Some(StrictInvocationAbstention::Opaque)
    );

    let paths = (0..=MAX_STRICT_FILE_TARGETS)
        .map(|index| format!("src/{index}.rs"))
        .collect::<Vec<_>>();
    let overflow = serde_json::json!({
        "type": "toolCall",
        "name": "read_file",
        "arguments": {"files": paths}
    });
    let projected = strict_tool_call_projection(&overflow, 9).unwrap();
    assert!(projected.file_invocations.is_empty());
    assert_eq!(
        projected.abstention,
        Some(StrictInvocationAbstention::Capacity)
    );
    let overflow_record = serde_json::to_vec(&serde_json::json!({
        "type": "message",
        "id": "overflow-record",
        "timestamp": "2026-07-31T12:00:00Z",
        "message": {"role": "assistant", "content": [overflow]}
    }))
    .unwrap();
    let (_temp, mut projector) = test_projector();
    let mut worker = JsonlFamilyWorkerContext::default();
    let mut emitted = Vec::new();
    projector
        .project(
            JsonlRecordRef::for_test(&overflow_record, 0),
            &mut worker,
            &mut |record| {
                emitted.push(record);
                Ok(())
            },
        )
        .unwrap();
    let [record] = emitted.as_slice() else {
        panic!("expected the overflowing call to remain a Core record");
    };
    assert!(record.repository_file_invocation_evidence.is_empty());
    assert!(record.repository_abstentions.iter().any(|abstention| {
        abstention.reason == RepositoryAbstentionReason::CandidateLimitExceeded
            && abstention.detail.as_deref() == Some("openclaw_file_invocation_evidence_overflow")
    }));

    for name in [
        "READ_FILE",
        "Read_File",
        "grep",
        "glob",
        "search",
        "apply_patch",
        "patch",
    ] {
        let projected = strict_tool_call_projection(
            &serde_json::json!({"type": "toolCall", "name": name, "arguments": {"path": "src/no.rs"}}),
            10,
        )
        .unwrap();
        assert!(projected.file_invocations.is_empty(), "promoted {name}");
        assert_eq!(
            projected.abstention,
            Some(StrictInvocationAbstention::Opaque)
        );
    }

    let byte_overflow = serde_json::json!({
        "type": "toolCall",
        "name": "read_file",
        "arguments": {"files": (0..5).map(|index| format!("{}-{index}", "x".repeat(16 * 1024 - 2))).collect::<Vec<_>>()}
    });
    let projected = strict_tool_call_projection(&byte_overflow, 11).unwrap();
    assert!(projected.file_invocations.is_empty());
    assert_eq!(
        projected.abstention,
        Some(StrictInvocationAbstention::Capacity)
    );
    assert!(strict_text_range(0, u32::MAX as usize + 1).is_none());
}

#[cfg(unix)]
#[test]
fn strict_tool_call_evidence_is_scoped_additively_and_selects_the_complete_call() {
    let (temp, mut projector) = test_projector();
    let mut worker = JsonlFamilyWorkerContext::default();
    let repository = temp.path().join("repository");
    fs::create_dir(&repository).unwrap();
    run_git(&repository, &["init", "-q"]);
    fs::create_dir(repository.join("src")).unwrap();
    fs::write(repository.join("src/lib.rs"), "pub fn before() {}\n").unwrap();

    let header = serde_json::to_vec(&serde_json::json!({
        "type": "session",
        "id": "test-session",
        "cwd": repository,
        "timestamp": "2026-07-31T12:00:00Z"
    }))
    .unwrap();
    projector
        .project(
            JsonlRecordRef::for_test(&header, 0),
            &mut worker,
            &mut |_| Ok(()),
        )
        .unwrap();
    let call = serde_json::to_vec(&serde_json::json!({
        "type": "message",
        "id": "strict-call-record",
        "timestamp": "2026-07-31T12:00:01Z",
        "message": {"role": "assistant", "content": [
            {"type": "text", "text": "before"},
            {
                "type": "toolCall",
                "id": "strict-call",
                "name": "edit_file",
                "arguments": {"path": "src/lib.rs", "replacement": "after"}
            }
        ]}
    }))
    .unwrap();
    let mut emitted = Vec::new();
    projector
        .project(
            JsonlRecordRef::for_test(&call, 1),
            &mut worker,
            &mut |record| {
                emitted.push(record);
                Ok(())
            },
        )
        .unwrap();

    let [record] = emitted.as_slice() else {
        panic!("expected one OpenClaw tool-call subrecord");
    };
    let [evidence] = record.repository_file_invocation_evidence.as_slice() else {
        panic!("expected one scoped invocation evidence item");
    };
    assert_eq!(evidence.operation_ordinal, 1);
    assert_eq!(evidence.relative_path, "src/lib.rs");
    assert_eq!(evidence.kind, RepositoryFileInvocationKind::Modify);
    assert_eq!(evidence.tool_name.as_deref(), Some("edit_file"));
    let body = record.content.normalized_body.as_deref().unwrap();
    let range = evidence.normalized_text_range.unwrap();
    assert_eq!(&body[range.start as usize..range.end as usize], body);
    assert_eq!(
        record.content.structured_content.as_ref().unwrap()["arguments"]["path"],
        "src/lib.rs"
    );
    assert_eq!(record.repository_file_observations.len(), 1);
    assert_eq!(
        record.repository_file_observations[0].kind,
        RepositoryFileObservationKind::Modified
    );
    record.validate_contract().unwrap();
}

#[test]
fn openclaw_large_tool_arguments_preserve_body_and_identity_within_aggregate_limit() {
    let tail = "openclaw_large_tool_argument_tail_complete";
    let full_argument = format!("{}{tail}", "x".repeat(8 * 1024 * 1024));
    let value = serde_json::json!({
        "type": "message",
        "id": "large-tool-call-record",
        "timestamp": "2026-07-31T12:00:00Z",
        "message": {
            "role": "assistant",
            "content": [{
                "type": "toolCall",
                "id": "large-call-1",
                "name": "custom_complete_tool",
                "arguments": {"prompt": &full_argument}
            }]
        }
    });
    let bytes = serde_json::to_vec(&value).unwrap();
    assert!(bytes.len() <= crate::MAX_PROVIDER_JSONL_LINE_BYTES);
    let (_temp, mut projector) = test_projector();
    let mut worker = JsonlFamilyWorkerContext::default();
    let mut emitted = Vec::new();
    projector
        .project(
            JsonlRecordRef::for_test(&bytes, 0),
            &mut worker,
            &mut |record| {
                emitted.push(record);
                Ok(())
            },
        )
        .unwrap();

    let [record] = emitted.as_slice() else {
        panic!("expected exactly one OpenClaw tool-call record");
    };
    let normalized = record.content.normalized_body.as_deref().unwrap();
    let base_native_event_id = TypedKey::utf8("large-tool-call-record").unwrap();
    let call_id = TypedKey::utf8("large-call-1").unwrap();
    let expected_native_event_id = TypedKey::composite(vec![
        base_native_event_id.clone(),
        TypedKey::composite(vec![
            TypedKey::utf8("tool_call_id").unwrap(),
            call_id.clone(),
        ])
        .unwrap(),
    ])
    .unwrap();
    let expected_event_id = derive_event_id(EventIdentityInput {
        source: &record.source,
        session_id: record.session_id,
        logical_item_kind: "openclaw-legacy-event",
        native_item_key: &NativeItemKey::native_id("openclaw.legacy-event", base_native_event_id)
            .unwrap(),
        subrecord_selector: Some(
            &ctx_history_core::SubrecordSelector::native_id("openclaw.tool-call-block", call_id)
                .unwrap(),
        ),
    })
    .unwrap();
    let duplicate_structured = value.pointer("/message/content/0").unwrap();
    assert!(normalized.contains(tail));
    assert_eq!(record.event_id, expected_event_id);
    assert_eq!(
        record.native_event_id.as_ref(),
        Some(&expected_native_event_id)
    );
    assert!(record.content.structured_content.is_none());
    assert!(
        normalized.len() + serde_json::to_vec(duplicate_structured).unwrap().len()
            > ctx_history_core::MAX_CORE_CONTENT_BYTES
    );
    assert!(
        record.content.encoded_content_bytes().unwrap() <= ctx_history_core::MAX_CORE_CONTENT_BYTES
    );
    record.validate_contract().unwrap();
    record.encode_stored().unwrap();
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
    let mut worker = JsonlFamilyWorkerContext::default();
    let mut emitted = Vec::new();
    for (ordinal, bytes) in [&call, &running].into_iter().enumerate() {
        projector
            .project(
                JsonlRecordRef::for_test(bytes, ordinal as u64),
                &mut worker,
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
    assert!(emitted[1].repository_file_invocation_evidence.is_empty());
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
            process_session_id: None,
            retrieval_contribution: StoredContributionClass::Unknown,
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
                process_session_id: None,
                retrieval_contribution: StoredContributionClass::Unknown,
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
fn source_wide_reused_result_id_keeps_both_results_searchable() {
    let values = vec![
        serde_json::json!({
            "type": "message",
            "id": "duplicate-call-1",
            "timestamp": "2026-08-05T12:00:00Z",
            "message": {"role": "assistant", "content": [{"type": "toolCall", "id": "reused-result", "name": "exec", "arguments": {"command": "ctx search first"}}]}
        }),
        serde_json::json!({
            "type": "message",
            "id": "duplicate-result-1",
            "timestamp": "2026-08-05T12:00:01Z",
            "message": {"role": "toolResult", "toolCallId": "reused-result", "content": "first payload", "details": {"status": "completed", "exitCode": 0}}
        }),
        serde_json::json!({
            "type": "message",
            "id": "duplicate-call-2",
            "timestamp": "2026-08-05T12:00:02Z",
            "message": {"role": "assistant", "content": [{"type": "toolCall", "id": "reused-result", "name": "exec", "arguments": {"command": "ctx search second"}}]}
        }),
        serde_json::json!({
            "type": "message",
            "id": "duplicate-result-2",
            "timestamp": "2026-08-05T12:00:03Z",
            "message": {"role": "toolResult", "toolCallId": "reused-result", "content": "second payload", "details": {"status": "completed", "exitCode": 0}}
        }),
        serde_json::json!({
            "type": "message",
            "id": "unique-call",
            "timestamp": "2026-08-05T12:00:04Z",
            "message": {"role": "assistant", "content": [{"type": "toolCall", "id": "unique-result", "name": "exec", "arguments": {"command": "ctx search unique"}}]}
        }),
        serde_json::json!({
            "type": "message",
            "id": "unique-result-record",
            "timestamp": "2026-08-05T12:00:05Z",
            "message": {"role": "toolResult", "toolCallId": "unique-result", "content": "unique payload", "details": {"status": "completed", "exitCode": 0}}
        }),
    ];
    let (_temp, mut projector) = test_projector();
    let records = project_values(&mut projector, &values);

    assert_eq!(records.len(), values.len());
    assert!(!retrieval_excluded(&records[1]));
    assert!(!retrieval_excluded(&records[3]));
    assert!(retrieval_excluded(&records[5]));
    for record in [&records[1], &records[3]] {
        assert!(record.repository_abstentions.iter().any(|abstention| {
            abstention.reason == RepositoryAbstentionReason::ProviderOutputUnjoined
                && abstention.detail.as_deref() == Some("openclaw_tool_result_call_id_is_ambiguous")
        }));
    }
}

#[test]
fn terminal_ambiguity_fingerprint_invalidates_only_affected_append() {
    let temp = tempfile::tempdir().unwrap();
    let transcript_path = temp.path().join("session.jsonl");
    let source = source_key("main/fingerprint-session").unwrap();
    let authority = ProviderSourceRoot::open(temp.path()).unwrap();
    let scan = |values: &[Value]| {
        let mut bytes = Vec::new();
        for value in values {
            serde_json::to_writer(&mut bytes, value).unwrap();
            bytes.push(b'\n');
        }
        fs::write(&transcript_path, bytes).unwrap();
        let opened = Arc::new(authority.open_file(Path::new("session.jsonl")).unwrap());
        terminal_authority_for_source(&source, &transcript_path, opened).unwrap()
    };
    let first_call = serde_json::json!({
        "type": "message",
        "id": "call-a",
        "timestamp": "2026-08-05T12:00:00Z",
        "message": {"role": "assistant", "content": [{"type": "toolCall", "id": "call-a", "name": "exec", "arguments": {"command": "ctx search a"}}]}
    });
    let first_result = serde_json::json!({
        "type": "message",
        "id": "result-a",
        "timestamp": "2026-08-05T12:00:01Z",
        "message": {"role": "toolResult", "toolCallId": "call-a", "content": "a", "details": {"status": "completed", "exitCode": 0}}
    });
    let second_call = serde_json::json!({
        "type": "message",
        "id": "call-b",
        "timestamp": "2026-08-05T12:00:02Z",
        "message": {"role": "assistant", "content": [{"type": "toolCall", "id": "call-b", "name": "exec", "arguments": {"command": "ctx search b"}}]}
    });
    let second_result = serde_json::json!({
        "type": "message",
        "id": "result-b",
        "timestamp": "2026-08-05T12:00:03Z",
        "message": {"role": "toolResult", "toolCallId": "call-b", "content": "b", "details": {"status": "completed", "exitCode": 0}}
    });
    let reused_result = serde_json::json!({
        "type": "message",
        "id": "result-a-reused",
        "timestamp": "2026-08-05T12:00:04Z",
        "message": {"role": "toolResult", "toolCallId": "call-a", "content": "a again", "details": {"status": "completed", "exitCode": 0}}
    });

    let prefix = scan(&[first_call.clone(), first_result.clone()]);
    let unique_append = scan(&[
        first_call.clone(),
        first_result.clone(),
        second_call.clone(),
        second_result.clone(),
    ]);
    assert_eq!(
        prefix.ambiguity_fingerprint(),
        unique_append.ambiguity_fingerprint()
    );
    assert!(unique_append.is_unique("call-a"));
    assert!(unique_append.is_unique("call-b"));

    let duplicate_append = scan(&[
        first_call,
        first_result,
        second_call,
        second_result,
        reused_result,
    ]);
    assert_ne!(
        prefix.ambiguity_fingerprint(),
        duplicate_append.ambiguity_fingerprint()
    );
    assert!(!duplicate_append.is_unique("call-a"));
    assert!(duplicate_append.is_unique("call-b"));
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
        native_session_family: OpenClawNativeSessionFamily::Absent,
        terminal_ambiguity_fingerprint: OpenClawTerminalAuthority::for_scan()
            .ambiguity_fingerprint(),
    };
    let source = source_key(native_session_id).unwrap();
    let session_id = session_identity(&source, native_session_id).unwrap();
    let source_path = PathBuf::from("/agents/main/sessions/fallback-session.jsonl");
    let session = SessionState::new(
        &source_path,
        native_session_id,
        &binding.index,
        &binding.native_session_family,
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
            process_session_id: None,
            retrieval_contribution: StoredContributionClass::Unknown,
        }),
    );
    let mut projector = OpenClawProjector {
        source: source.clone(),
        native_session_id: native_session_id.to_owned(),
        session_id,
        session,
        index_file: None,
        authority,
        pending_calls,
        running_processes: HashMap::new(),
        terminal_authority: OpenClawTerminalAuthority::unscanned_for_test(),
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
            process_session_id: None,
            retrieval_contribution: StoredContributionClass::Unknown,
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

#[test]
fn over_8_mib_tool_result_is_admitted_complete_without_structured_body_duplication() {
    let tail = "openclaw_large_result_tail_complete";
    let full_result = format!("{} {tail}", "x".repeat(9 * 1024 * 1024));
    let bytes = serde_json::to_vec(&serde_json::json!({
        "type": "message",
        "id": "large-result-record",
        "timestamp": "2026-08-01T12:00:00Z",
        "message": {
            "role": "toolResult",
            "toolCallId": "large-result-call",
            "toolName": "exec",
            "content": [{"type": "text", "text": full_result}],
            "details": {"status": "completed", "exitCode": 0}
        }
    }))
    .unwrap();
    assert!(bytes.len() > 8 * 1024 * 1024);
    assert!(bytes.len() <= crate::MAX_PROVIDER_JSONL_LINE_BYTES);

    let (_temp, mut projector) = test_projector();
    let mut worker = JsonlFamilyWorkerContext::default();
    let mut emitted = Vec::new();
    projector
        .project(
            JsonlRecordRef::for_test(&bytes, 0),
            &mut worker,
            &mut |record| {
                emitted.push(record);
                Ok(())
            },
        )
        .unwrap();

    let [record] = emitted.as_slice() else {
        panic!("expected exactly one OpenClaw result record");
    };
    assert_eq!(
        record.content.normalized_body.as_deref(),
        Some(full_result.as_str())
    );
    let structured = record.content.structured_content.as_ref().unwrap();
    assert_eq!(structured["result_content_location"], "normalized_body");
    assert_eq!(structured["result_content_complete"], true);
    assert_eq!(structured["result_metadata"]["status"], "completed");
    let encoded_structured = serde_json::to_vec(structured).unwrap();
    assert!(encoded_structured.len() < 4 * 1024);
    assert!(!String::from_utf8(encoded_structured)
        .unwrap()
        .contains(tail));
    record.validate_contract().unwrap();
    record.encode_stored().unwrap();
}
