mod production;

use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind, Event,
    EventRole, EventType, Fidelity, Session, SessionStatus,
};
use ctx_history_store::Store;
use serde_json::json;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use super::{
    cursor_complete_content_source_from_admitted, cursor_complete_content_source_revision,
    discover_cursor_transcripts, freeze_cursor_source, resolve_cursor_missing_sources,
    scan_cursor_source, CursorCheckpoint, CursorCompletedExactInventory, CursorKnownSource,
    CursorMissingSourceDisposition, CursorPriorObservation, CursorReadOutcome,
    CursorSourceGeneration, CursorSourceMutation,
};
use super::{CursorPublicationPage, CursorPublicationSink};
use crate::complete_content::{
    jsonl::JsonlCompleteContentResolver, AuthorizedSourceRoute, CompleteContentErrorKind,
    CompleteContentHashAuthority, CompleteContentResolver, CompleteContentSourceFamily,
    CompleteMessageRequest, SourceAccessBroker, SourceSnapshot, VerifiedContentLocatorsV1,
    VerifiedContentRole, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
};
use crate::provider::importer::{
    provider_scoped_source_uuid, provider_session_uuid, provider_source_event_import_identity,
    provider_sync_metadata, timestamps,
};
use crate::provider::providers::cursor::{
    layout::{
        CURSOR_MAX_DIRECTORY_DEPTH, CURSOR_MAX_DIRECTORY_ENTRIES,
        CURSOR_MAX_DISCOVERY_ISSUE_SAMPLES, CURSOR_MAX_TRANSCRIPTS, CURSOR_MAX_TRAVERSAL_ENTRIES,
    },
    parser::{
        scan_cursor_bytes_into_sink, scan_cursor_bytes_with_limit, CursorParserOutcome,
        CursorRejectionKind, CURSOR_REJECTION_SAMPLE_LIMIT,
    },
};
use crate::{
    import_cursor_native_history, CaptureWorkLimit, CursorNativeImportOptions, ImportProfile,
    OutputOutcome, OutputSourceIdentity, ProOutputMaterializationPage, ProOutputPageResult,
    ProOutputProgress, ProOutputSink, ProOutputSinkError, ProOutputSourceDisposition,
    ProviderImportWorkResult, CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
    LEGACY_CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT, PROVIDER_MAX_TEXT_CHARS,
};

fn tempdir() -> TempDir {
    let temp_root = fs::canonicalize(std::env::temp_dir())
        .expect("system temporary directory should be canonicalizable");
    tempfile::Builder::new()
        .prefix("cursor-native-slice-")
        .tempdir_in(temp_root)
        .unwrap()
}

fn cursor_path(root: &Path, project: &str, session: &str) -> PathBuf {
    root.join(project)
        .join("agent-transcripts")
        .join(session)
        .join(format!("{session}.jsonl"))
}

fn write_transcript(root: &Path, project: &str, session: &str, bytes: &[u8]) -> PathBuf {
    let path = cursor_path(root, project, session);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, bytes).unwrap();
    path
}

fn jsonl(rows: impl IntoIterator<Item = serde_json::Value>) -> Vec<u8> {
    let mut bytes = Vec::new();
    for row in rows {
        serde_json::to_writer(&mut bytes, &row).unwrap();
        bytes.push(b'\n');
    }
    bytes
}

fn user(text: &str) -> serde_json::Value {
    json!({
        "timestamp": "2026-07-24T12:00:00Z",
        "role": "user",
        "message": {
            "role": "user",
            "content": [{"type": "text", "text": text}]
        }
    })
}

fn assistant(text: &str) -> serde_json::Value {
    json!({
        "timestamp": "2026-07-24T12:00:01Z",
        "role": "assistant",
        "message": {
            "role": "assistant",
            "content": [{"type": "text", "text": text}]
        }
    })
}

fn call(index: usize) -> serde_json::Value {
    json!({
        "timestamp": "2026-07-24T12:00:02Z",
        "role": "assistant",
        "message": {
            "role": "assistant",
            "content": [{
                "input": {
                    "content": format!("call-input-must-not-retain-{index}"),
                    "path": format!("safe-{index}.txt")
                },
                "name": "write_file",
                "id": format!("call-{index}"),
                "type": "tool_use"
            }]
        }
    })
}

fn result(index: usize, body: &str) -> serde_json::Value {
    json!({
        "timestamp": "2026-07-24T12:00:03Z",
        "role": "user",
        "message": {
            "role": "user",
            "content": [{
                "content": format!("{body}-{index}"),
                "tool_use_id": format!("call-{index}"),
                "is_error": false,
                "type": "tool_result"
            }]
        }
    })
}

fn summary(text: &str) -> serde_json::Value {
    json!({
        "timestamp": "2026-07-24T12:00:04Z",
        "event": "turn_ended",
        "message": {
            "content": [{"type": "text", "text": text}]
        }
    })
}

fn parsed(
    bytes: &[u8],
    checkpoint: Option<&CursorCheckpoint>,
) -> super::parser::CursorParsedGeneration {
    match scan_cursor_bytes_with_limit(bytes, checkpoint, 1024 * 1024).unwrap() {
        CursorParserOutcome::Parsed(parsed) => *parsed,
        CursorParserOutcome::PrefixMismatch(_) => panic!("unexpected prefix mismatch"),
    }
}

fn generation(outcome: CursorReadOutcome) -> CursorSourceGeneration {
    match outcome {
        CursorReadOutcome::Generation(generation) => *generation,
        CursorReadOutcome::Unchanged(_) => panic!("expected a parsed generation"),
    }
}

fn one_source(root: &Path) -> super::CursorTranscriptPath {
    let inventory = discover_cursor_transcripts(root);
    assert!(inventory.completed, "{:?}", inventory.issues);
    assert_eq!(inventory.transcripts.len(), 1, "{inventory:#?}");
    inventory.transcripts.into_iter().next().unwrap()
}

fn prior(generation: &CursorSourceGeneration, key: &str) -> CursorPriorObservation {
    CursorPriorObservation {
        canonical_source_key: key.to_owned(),
        observation: generation.observation.clone(),
        checkpoint: generation.checkpoint.clone(),
    }
}

#[test]
fn discovery_accepts_only_exact_cursor_layout_in_deterministic_order() {
    let temp = tempdir();
    let root = temp.path().join("projects");
    let second = write_transcript(&root, "z-project", "session-b", b"");
    let first = write_transcript(&root, "a-project", "session-a", b"");
    write_transcript(&root, "a-project", "mismatch", b"");
    let mismatch = root.join("a-project/agent-transcripts/mismatch/wrong.jsonl");
    fs::rename(cursor_path(&root, "a-project", "mismatch"), &mismatch).unwrap();
    let loose = root.join("loose/nested/project/agent-transcripts/session/session.jsonl");
    fs::create_dir_all(loose.parent().unwrap()).unwrap();
    fs::write(&loose, b"").unwrap();
    fs::write(root.join("ordinary.jsonl"), b"").unwrap();

    let inventory = discover_cursor_transcripts(&root);

    assert!(inventory.completed);
    assert_eq!(
        inventory
            .transcripts
            .iter()
            .map(|source| source.path().to_path_buf())
            .collect::<Vec<_>>(),
        [first, second]
    );
    assert_eq!(inventory.projects_roots, [root]);
    assert_eq!(inventory.stats.selected_transcripts, 2);
    assert_eq!(inventory.stats.rejected_candidates, 2);
    assert!(inventory
        .issues
        .iter()
        .all(|issue| issue.kind == super::CursorDiscoveryIssueKind::InvalidLayout));
}

#[cfg(unix)]
#[test]
fn discovery_rejects_symlinked_sources_and_withholds_deletion_authority() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let root = temp.path().join("projects");
    let real = write_transcript(&root, "project", "real-session", &jsonl([user("real")]));
    let linked_session = root.join("project/agent-transcripts/linked-session");
    symlink(real.parent().unwrap(), &linked_session).unwrap();

    let inventory = discover_cursor_transcripts(&root);

    assert!(!inventory.completed);
    assert_eq!(inventory.transcripts.len(), 1);
    assert!(inventory
        .issues
        .iter()
        .any(|issue| issue.kind == super::CursorDiscoveryIssueKind::Symlink));
}

#[test]
fn discovery_enforces_directory_depth_entry_and_transcript_bounds() {
    let temp = tempdir();

    let entry_root = temp.path().join("entry-projects");
    fs::create_dir_all(&entry_root).unwrap();
    for index in 0..=CURSOR_MAX_DIRECTORY_ENTRIES {
        fs::write(entry_root.join(format!("entry-{index:04}.txt")), b"").unwrap();
    }
    let entry_inventory = discover_cursor_transcripts(&entry_root);
    assert!(!entry_inventory.completed);
    assert_eq!(
        entry_inventory.stats.entries_visited,
        CURSOR_MAX_DIRECTORY_ENTRIES
    );
    assert!(entry_inventory
        .issues
        .iter()
        .any(|issue| issue.kind == super::CursorDiscoveryIssueKind::LimitExceeded));

    let depth_root = temp.path().join("depth-projects");
    fs::create_dir_all(&depth_root).unwrap();
    let mut directory = depth_root.clone();
    for index in 0..=CURSOR_MAX_DIRECTORY_DEPTH {
        directory = directory.join(format!("d{index}"));
        fs::create_dir(&directory).unwrap();
    }
    let depth_inventory = discover_cursor_transcripts(&depth_root);
    assert!(!depth_inventory.completed);
    assert_eq!(
        depth_inventory.stats.directories_visited,
        CURSOR_MAX_DIRECTORY_DEPTH + 1
    );

    let transcript_root = temp.path().join("transcript-projects");
    for index in 0..=CURSOR_MAX_TRANSCRIPTS {
        write_transcript(
            &transcript_root,
            "project",
            &format!("session-{index:04}"),
            b"",
        );
    }
    let transcript_inventory = discover_cursor_transcripts(&transcript_root);
    assert!(!transcript_inventory.completed);
    assert_eq!(
        transcript_inventory.transcripts.len(),
        CURSOR_MAX_TRANSCRIPTS
    );
}

#[test]
fn discovery_caps_total_entries_and_issue_samples_while_counting_all_rejections() {
    let temp = tempdir();
    let traversal_root = temp.path().join("traversal-projects");
    fs::create_dir_all(&traversal_root).unwrap();
    for directory_index in 0..4 {
        let directory = traversal_root.join(format!("d{directory_index}"));
        fs::create_dir(&directory).unwrap();
        for entry_index in 0..CURSOR_MAX_DIRECTORY_ENTRIES {
            fs::write(directory.join(format!("e{entry_index:04}.txt")), b"").unwrap();
        }
    }
    let traversal_inventory = discover_cursor_transcripts(&traversal_root);
    assert!(!traversal_inventory.completed);
    assert_eq!(
        traversal_inventory.stats.entries_visited,
        CURSOR_MAX_TRAVERSAL_ENTRIES
    );

    let issue_root = temp.path().join("issue-projects");
    let rejection_count = CURSOR_MAX_DISCOVERY_ISSUE_SAMPLES + 17;
    for index in 0..rejection_count {
        let session = format!("session-{index:04}");
        let path = cursor_path(&issue_root, "project", &session);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path.with_file_name("mismatch.jsonl"), b"").unwrap();
    }
    let issue_inventory = discover_cursor_transcripts(&issue_root);
    assert_eq!(issue_inventory.stats.rejected_candidates, rejection_count);
    assert_eq!(
        issue_inventory.issues.len(),
        CURSOR_MAX_DISCOVERY_ISSUE_SAMPLES
    );
}

#[test]
fn selective_parser_excludes_every_result_surface_before_hashing() {
    let sentinel = "NATIVEPATH_SYNTHETIC_OUTPUT_NEVER_RETAIN";
    let huge_output = sentinel.repeat(4_096);
    let mixed = json!({
        "timestamp": "2026-07-24T12:00:05Z",
        "role": "assistant",
        "message": {
            "role": "assistant",
            "content": [
                {"type": "text", "text": "safe mixed text"},
                {
                    "content": huge_output,
                    "tool_use_id": "call-mixed",
                    "type": "tool_result"
                },
                {
                    "input": {
                        "content": sentinel.repeat(512),
                        "path": "safe-mixed.txt"
                    },
                    "name": "write_file",
                    "id": "call-mixed",
                    "type": "tool_use"
                }
            ]
        }
    });
    let mut bytes = jsonl([
        user("safe user"),
        call(0),
        result(0, &huge_output),
        mixed,
        json!({
            "type": "command_output",
            "content": huge_output,
        }),
        summary("safe summary"),
    ]);
    bytes.extend_from_slice(
        format!(
            "{{\"type\":\"command_output\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"{sentinel}\"}}]}},\"type\":\"future_cursor_event\"}}\n"
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(
        format!(
            "{{\"message\":{{\"content\":[{{\"type\":\"tool_result\",\"text\":\"{sentinel}\",\"type\":\"text\"}}]}}}}\n"
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(
        format!(
            "{{\"role\":\"tool\",\"role\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"{sentinel}\"}}]}}}}\n"
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&jsonl([
        json!({
            "role": "tool",
            "message": {
                "role": "tool",
                "content": [{"type": "text", "text": sentinel}]
            }
        }),
        json!({
            "type": "function_call_output",
            "output": sentinel
        }),
        json!({
            "type": "future_result_wrapper",
            "role": "assistant",
            "message": {
                "role": "assistant",
                "content": [{"type": "text", "text": sentinel}]
            }
        }),
        json!({
            "role": "user",
            "message": {
                "role": "user",
                "content": [{
                    "type": "tool_use",
                    "id": "not-an-assistant-call",
                    "name": "write_file",
                    "input": {"path": sentinel}
                }]
            }
        }),
    ]));

    let parsed = parsed(&bytes, None);

    assert_eq!(parsed.events.len(), 5);
    assert_eq!(
        parsed
            .events
            .iter()
            .map(|event| (
                event.native_order.semantic_ordinal,
                event.native_order.part_ordinal,
                event.event_type,
            ))
            .collect::<Vec<_>>(),
        [
            (0, 0, EventType::Message),
            (1, 0, EventType::ToolCall),
            (3, 0, EventType::Message),
            (3, 1, EventType::ToolCall),
            (5, 0, EventType::Summary),
        ]
    );
    let serialized = serde_json::to_string(&parsed.events).unwrap();
    assert!(!serialized.contains(sentinel));
    assert!(!serialized.contains("call-input-must-not-retain"));
    assert!(serialized.contains("safe-0.txt"));
    assert!(serialized.contains("safe-mixed.txt"));
    assert_eq!(parsed.stats.native_result_records, 10);
    assert!(parsed.stats.native_result_bytes > huge_output.len() as u64);
    assert_eq!(parsed.stats.result_body_bytes_decoded_or_allocated, 0);
    assert_eq!(parsed.stats.result_hashes_created, 0);
    assert_eq!(parsed.stats.result_previews_created, 0);
    assert_eq!(parsed.stats.result_touches_created, 0);
    assert_eq!(parsed.stats.result_fts_created, 0);
    assert_eq!(parsed.stats.result_handoffs_created, 0);
    assert_eq!(
        parsed.stats.nativepath_publication_rows,
        parsed.events.len() as u64
    );
    assert_eq!(parsed.rejections.total, 0);
}

#[test]
fn malformed_and_incomplete_rows_do_not_consume_semantic_ordinals() {
    let baseline = jsonl([user("first"), assistant("second")]);
    let mut malformed = jsonl([user("first")]);
    malformed.extend_from_slice(b"{\"not valid\"\n");
    malformed.extend_from_slice(&jsonl([assistant("second")]));
    let baseline = parsed(&baseline, None);
    let malformed = parsed(&malformed, None);

    assert_eq!(baseline.events.len(), 2);
    assert_eq!(malformed.events.len(), 2);
    assert_eq!(
        baseline
            .events
            .iter()
            .map(|event| event.identity)
            .collect::<Vec<_>>(),
        malformed
            .events
            .iter()
            .map(|event| event.identity)
            .collect::<Vec<_>>()
    );
    assert_eq!(malformed.rejections.total, 1);
    assert_eq!(
        malformed.rejections.samples[0].kind,
        CursorRejectionKind::MalformedJson
    );
    assert_eq!(
        malformed.checkpoint.disposition,
        super::CursorCheckpointDisposition::WithholdForRejections
    );

    let first = jsonl([user("complete")]);
    let partial = serde_json::to_vec(&assistant("valid JSON without LF")).unwrap();
    let mut incomplete = first.clone();
    incomplete.extend_from_slice(&partial);
    let incomplete = parsed(&incomplete, None);
    assert_eq!(incomplete.events.len(), 1);
    assert_eq!(incomplete.rejections.total, 0);
    assert!(!incomplete.checkpoint.terminal);
    assert_eq!(incomplete.checkpoint.next_byte_offset, first.len() as u64);
    assert_eq!(incomplete.stats.incomplete_tail_records, 1);
}

#[test]
fn rejection_details_keep_an_exact_count_and_fixed_samples() {
    let rejection_count = CURSOR_REJECTION_SAMPLE_LIMIT + 17;
    let bytes = b"{\"malformed\"\n".repeat(rejection_count);

    let parsed = parsed(&bytes, None);

    assert_eq!(parsed.rejections.total, rejection_count as u64);
    assert_eq!(
        parsed.rejections.samples.len(),
        CURSOR_REJECTION_SAMPLE_LIMIT
    );
    assert_eq!(parsed.rejections.samples[0].physical_line, 0);
    assert_eq!(
        parsed.rejections.samples.last().unwrap().physical_line,
        (CURSOR_REJECTION_SAMPLE_LIMIT - 1) as u64
    );
    assert!(parsed.events.is_empty());
}

#[test]
fn v025_bridge_uses_physical_ordinal_only_for_the_first_sibling() {
    let mut bytes = b"\n{\"malformed\"\n".to_vec();
    bytes.extend_from_slice(&jsonl([json!({
        "timestamp": "2026-07-24T12:00:01Z",
        "role": "assistant",
        "message": {
            "role": "assistant",
            "content": [
                {"type": "text", "text": "first sibling"},
                {"type": "text", "text": "second sibling"}
            ]
        }
    })]));

    let parsed = parsed(&bytes, None);

    assert_eq!(parsed.rejections.total, 1);
    assert_eq!(parsed.events.len(), 2);
    assert!(parsed.events.iter().all(|event| {
        event.native_order.semantic_ordinal == 0 && event.native_order.physical_ordinal == 2
    }));
    assert_eq!(parsed.events[0].legacy_provider_event_index(), Some(2));
    assert_eq!(parsed.events[1].legacy_provider_event_index(), None);
}

#[test]
fn historical_v025_store_reuses_physical_line_id_without_collapsing_siblings() {
    const MACHINE: &str = "cursor-v025-upgrade-machine";
    let temp = tempdir();
    let root = temp.path().join("projects");
    let mut bytes = b"\n{\"malformed\"\n".to_vec();
    bytes.extend_from_slice(&jsonl([json!({
        "timestamp": "2026-07-24T12:00:01Z",
        "role": "assistant",
        "message": {
            "role": "assistant",
            "content": [
                {"type": "text", "text": "released first sibling"},
                {"type": "text", "text": "new second sibling"}
            ]
        }
    })]));
    let path = write_transcript(&root, "project", "v025-upgrade", &bytes);
    let parsed = parsed(&bytes, None);
    let first_hash = parsed.events[0].provider_event_hash.clone();
    let canonical_path = fs::canonicalize(&path).unwrap();
    let raw_source_path = canonical_path.display().to_string();
    let locator_identity =
        crate::provider::importer::provider_path_identity(&canonical_path).unwrap();
    let source_identity = format!("cursor-native-path-v1:{locator_identity}");
    let source_id = provider_scoped_source_uuid(
        CaptureProvider::Cursor,
        "v025-upgrade",
        LEGACY_CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
        Some(&raw_source_path),
    );
    let session_id = provider_session_uuid(CaptureProvider::Cursor, "v025-upgrade");
    let occurred_at = "2026-07-24T12:00:01Z".parse().unwrap();
    let mut store = Store::open(temp.path().join("history.sqlite")).unwrap();
    store
        .upsert_capture_source(&CaptureSource {
            id: source_id,
            descriptor: CaptureSourceDescriptor {
                kind: CaptureSourceKind::ProviderImport,
                provider: CaptureProvider::Cursor,
                machine_id: MACHINE.to_owned(),
                process_id: None,
                cwd: None,
                raw_source_path: Some(raw_source_path.clone()),
                source_format: Some(LEGACY_CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT.to_owned()),
                source_root: Some(root.display().to_string()),
                source_identity: Some(source_identity),
                external_session_id: Some("v025-upgrade".to_owned()),
            },
            started_at: occurred_at,
            ended_at: Some(occurred_at),
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({"source_format": LEGACY_CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT}),
            ),
        })
        .unwrap();
    store
        .upsert_session(&Session {
            id: session_id,
            history_record_id: None,
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: Some(source_id),
            provider: CaptureProvider::Cursor,
            external_session_id: Some("v025-upgrade".to_owned()),
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at: occurred_at,
            ended_at: Some(occurred_at),
            timestamps: timestamps(occurred_at),
            sync: provider_sync_metadata(Fidelity::Imported, json!({})),
        })
        .unwrap();
    let released = provider_source_event_import_identity(source_id, 2, &first_hash);
    store
        .upsert_event(&Event {
            id: released.id,
            seq: released.seq,
            history_record_id: None,
            session_id: Some(session_id),
            run_id: None,
            event_type: EventType::Message,
            role: Some(EventRole::Assistant),
            occurred_at,
            capture_source_id: Some(source_id),
            payload: json!({
                "text": "released first sibling",
                "body": {"kind": "text", "text": "released first sibling"}
            }),
            payload_blob_id: None,
            dedupe_key: Some(released.dedupe_key),
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider_event_hash": first_hash,
                    "provider_event_hash_authority": "provider_supplied",
                    "source_format": LEGACY_CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
                    "source_record_ordinal": 2,
                    "source_record_subrecord_index": 0,
                }),
            ),
        })
        .unwrap();

    let summary =
        production::import_cursor_proof(&root, &mut store, MACHINE, ImportProfile::CoreOnly);

    assert_eq!(summary.failed, 1);
    assert_eq!(
        store
            .get_capture_source(source_id)
            .unwrap()
            .descriptor
            .source_format,
        Some(CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT.to_owned())
    );
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 2);
    assert!(events.iter().any(|event| event.id == released.id));
    assert_eq!(
        events
            .iter()
            .map(|event| event.id)
            .collect::<BTreeSet<_>>()
            .len(),
        2
    );
    assert!(events
        .iter()
        .any(|event| { event.payload["text"].as_str() == Some("new second sibling") }));
}

#[test]
fn oversized_complete_rows_are_drained_boundedly_without_consuming_ordinal() {
    let mut bytes = vec![b'x'; 300];
    bytes.push(b'\n');
    bytes.extend_from_slice(&jsonl([user("survivor")]));

    let parsed = match scan_cursor_bytes_with_limit(&bytes, None, 128).unwrap() {
        CursorParserOutcome::Parsed(parsed) => parsed,
        CursorParserOutcome::PrefixMismatch(_) => panic!("unexpected prefix mismatch"),
    };

    assert_eq!(parsed.events.len(), 1);
    assert_eq!(parsed.events[0].identity.semantic_ordinal, 0);
    assert_eq!(parsed.rejections.total, 1);
    assert_eq!(
        parsed.rejections.samples[0].kind,
        CursorRejectionKind::Oversized
    );
    assert_eq!(parsed.rejections.samples[0].observed_bytes, 300);
    assert!(parsed.stats.max_line_buffer_bytes <= 130);
}

#[test]
fn unsupported_retained_shapes_are_record_local_rejections() {
    let bytes = jsonl([
        json!({
            "role": "user",
            "message": {
                "role": "user",
                "content": [{"type": "text", "text": 42}]
            }
        }),
        user("survivor"),
    ]);

    let parsed = parsed(&bytes, None);

    assert_eq!(parsed.events.len(), 1);
    assert_eq!(parsed.events[0].identity.semantic_ordinal, 0);
    assert_eq!(parsed.rejections.total, 1);
    assert_eq!(
        parsed.rejections.samples[0].kind,
        CursorRejectionKind::UnsupportedShape
    );
}

#[test]
fn append_resume_verifies_classified_prefix_and_emits_only_suffix() {
    let baseline = jsonl([user("baseline"), result(0, "discarded-output")]);
    let initial = parsed(&baseline, None);
    assert_eq!(initial.events.len(), 1);
    let mut appended = baseline.clone();
    appended.extend_from_slice(&jsonl([assistant("appended")]));

    let resumed = parsed(&appended, Some(&initial.checkpoint));

    assert!(resumed.resumed);
    assert_eq!(resumed.events.len(), 1);
    assert_eq!(resumed.events[0].identity.semantic_ordinal, 2);
    assert_eq!(resumed.stats.verification_bytes_read, baseline.len() as u64);
    assert_eq!(
        resumed.stats.projected_bytes_read,
        (appended.len() - baseline.len()) as u64
    );
    assert_eq!(resumed.stats.result_body_bytes_decoded_or_allocated, 0);

    let rewritten = jsonl([user("changed!"), result(0, "discarded-output")]);
    assert!(matches!(
        scan_cursor_bytes_with_limit(&rewritten, Some(&initial.checkpoint), 1024 * 1024).unwrap(),
        CursorParserOutcome::PrefixMismatch(_)
    ));

    let output_only_rewrite = jsonl([user("baseline"), result(0, "discarded-OUTPUT")]);
    assert_eq!(output_only_rewrite.len(), baseline.len());
    assert!(matches!(
        scan_cursor_bytes_with_limit(&output_only_rewrite, Some(&initial.checkpoint), 1024 * 1024,)
            .unwrap(),
        CursorParserOutcome::PrefixMismatch(_)
    ));
}

fn c0_rows(count: usize) -> Vec<serde_json::Value> {
    (0..count)
        .map(|index| match index % 6 {
            0 => user(&format!("user-{index}")),
            1 | 5 => assistant(&format!("assistant-{index}")),
            2 => call(index),
            3 => result(index.saturating_sub(1), "discarded-output"),
            4 => summary(&format!("summary-{index}")),
            _ => unreachable!(),
        })
        .collect()
}

#[test]
fn cursor_c0_counts_follow_native_semantic_order() {
    let baseline_bytes = jsonl(c0_rows(20));
    let baseline = parsed(&baseline_bytes, None);
    let append = parsed(&jsonl(c0_rows(21)), None);
    let truncation = parsed(&jsonl(c0_rows(19)), None);
    let mut malformed_bytes = baseline_bytes.clone();
    malformed_bytes.splice(0..0, b"{\"malformed\"\n".iter().copied());
    let malformed = parsed(&malformed_bytes, None);
    let mut incomplete_bytes = jsonl(c0_rows(19));
    incomplete_bytes.extend_from_slice(&serde_json::to_vec(&assistant("partial")).unwrap());
    let incomplete = parsed(&incomplete_bytes, None);

    assert_eq!(baseline.events.len(), 17);
    assert_eq!(append.events.len(), 18);
    assert_eq!(truncation.events.len(), 16);
    assert_eq!(malformed.events.len(), 17);
    assert_eq!(malformed.rejections.total, 1);
    assert_eq!(incomplete.events.len(), 16);
    assert!(!incomplete.checkpoint.terminal);
    assert_eq!(baseline.stats.native_result_records, 3);
    assert_eq!(
        baseline
            .events
            .iter()
            .filter(|event| event.event_type == EventType::ToolCall)
            .count(),
        3
    );
}

#[test]
fn output_heavy_scale_scan_keeps_result_state_at_zero() {
    const RECORDS: usize = 10_000;
    let sentinel = "NATIVEPATH_SCALE_OUTPUT_SENTINEL";
    let rows = (0..RECORDS).map(|index| {
        if index % 2 == 0 {
            call(index)
        } else {
            result(index.saturating_sub(1), &sentinel.repeat(32))
        }
    });
    let bytes = jsonl(rows);

    let parsed = parsed(&bytes, None);

    assert_eq!(parsed.events.len(), RECORDS / 2);
    assert_eq!(parsed.stats.native_result_records, (RECORDS / 2) as u64);
    assert_eq!(parsed.stats.result_body_bytes_decoded_or_allocated, 0);
    assert_eq!(parsed.stats.result_hashes_created, 0);
    assert_eq!(parsed.stats.result_previews_created, 0);
    assert_eq!(parsed.stats.result_touches_created, 0);
    assert_eq!(parsed.stats.result_fts_created, 0);
    assert_eq!(parsed.stats.result_handoffs_created, 0);
    assert_eq!(parsed.stats.publication_pages, 79);
    assert_eq!(
        parsed.stats.nativepath_publication_rows,
        (RECORDS / 2) as u64
    );
    assert!(parsed.stats.max_publication_page_rows <= super::CURSOR_PUBLICATION_PAGE_MAX_ROWS);
    assert!(parsed.stats.max_publication_page_bytes <= super::CURSOR_PUBLICATION_PAGE_MAX_BYTES);
    assert!(!serde_json::to_string(&parsed.events)
        .unwrap()
        .contains(sentinel));
}

fn digest_update(hasher: &mut Sha256, fields: &[&str]) {
    for field in fields {
        hasher.update(field.len().to_le_bytes());
        hasher.update(field.as_bytes());
    }
}

struct CursorScaleDigestSink {
    content: Sha256,
    identity: Sha256,
    snapshot: Option<CursorScaleDigestSnapshot>,
    pages: usize,
    rows: usize,
    max_page_rows: usize,
    max_page_capacity: usize,
    max_page_serialized_bytes: usize,
    max_page_retained_bytes: usize,
}

struct CursorScaleDigestSnapshot {
    content: Sha256,
    identity: Sha256,
    pages: usize,
    rows: usize,
    max_page_rows: usize,
    max_page_capacity: usize,
    max_page_serialized_bytes: usize,
    max_page_retained_bytes: usize,
}

impl CursorScaleDigestSink {
    fn new() -> Self {
        Self {
            content: Sha256::new(),
            identity: Sha256::new(),
            snapshot: None,
            pages: 0,
            rows: 0,
            max_page_rows: 0,
            max_page_capacity: 0,
            max_page_serialized_bytes: 0,
            max_page_retained_bytes: 0,
        }
    }

    fn finish(self) -> (String, String) {
        (
            format!("{:x}", self.content.finalize()),
            format!("{:x}", self.identity.finalize()),
        )
    }
}

impl CursorPublicationSink for CursorScaleDigestSink {
    fn begin_cursor_publication(&mut self) -> crate::Result<()> {
        self.snapshot = Some(CursorScaleDigestSnapshot {
            content: self.content.clone(),
            identity: self.identity.clone(),
            pages: self.pages,
            rows: self.rows,
            max_page_rows: self.max_page_rows,
            max_page_capacity: self.max_page_capacity,
            max_page_serialized_bytes: self.max_page_serialized_bytes,
            max_page_retained_bytes: self.max_page_retained_bytes,
        });
        Ok(())
    }

    fn stage_cursor_page(&mut self, page: CursorPublicationPage) -> crate::Result<()> {
        self.pages += 1;
        self.rows += page.events.len();
        self.max_page_rows = self.max_page_rows.max(page.events.len());
        self.max_page_capacity = self.max_page_capacity.max(page.events.capacity());
        self.max_page_serialized_bytes = self.max_page_serialized_bytes.max(page.serialized_bytes);
        self.max_page_retained_bytes = self.max_page_retained_bytes.max(page.retained_bytes);
        assert!(page.events.len() <= super::CURSOR_PUBLICATION_PAGE_MAX_ROWS);
        assert!(page.events.capacity() <= super::CURSOR_PUBLICATION_PAGE_MAX_ROWS);
        assert!(page.serialized_bytes <= super::CURSOR_PUBLICATION_PAGE_MAX_BYTES);
        assert!(page.retained_bytes <= super::CURSOR_PUBLICATION_PAGE_MAX_BYTES);
        for event in page.events {
            let text = match event.body {
                super::CursorEventBody::Text { text } => text,
                super::CursorEventBody::None | super::CursorEventBody::ToolCall { .. } => {
                    String::new()
                }
            };
            digest_update(
                &mut self.content,
                &[event.event_type.as_str(), event.role.as_str(), &text],
            );
            let identity = event.identity.provider_identity();
            digest_update(&mut self.identity, &[&identity]);
        }
        Ok(())
    }

    fn abort_cursor_publication(&mut self) {
        let snapshot = self
            .snapshot
            .take()
            .expect("abort requires an active publication transaction");
        self.content = snapshot.content;
        self.identity = snapshot.identity;
        self.pages = snapshot.pages;
        self.rows = snapshot.rows;
        self.max_page_rows = snapshot.max_page_rows;
        self.max_page_capacity = snapshot.max_page_capacity;
        self.max_page_serialized_bytes = snapshot.max_page_serialized_bytes;
        self.max_page_retained_bytes = snapshot.max_page_retained_bytes;
    }

    fn commit_cursor_publication(&mut self) -> crate::Result<()> {
        self.snapshot
            .take()
            .ok_or(crate::CaptureError::SystemInvariant(
                "Cursor publication commit without begin",
            ))?;
        Ok(())
    }
}

fn cursor_scale_fixture() -> (Vec<u8>, String, String) {
    const RECORDS: usize = 196_608;
    const TEXT_PADDING_BYTES: usize = 512;
    let padding = "0123456789abcdef".repeat(TEXT_PADDING_BYTES.div_ceil(16));
    let padding = &padding[..TEXT_PADDING_BYTES];
    let mut bytes = Vec::with_capacity(RECORDS * 700);
    let mut expected_content = Sha256::new();
    let mut expected_identity = Sha256::new();
    for index in 0..RECORDS {
        let (role, text) = if index % 2 == 0 {
            (
                "user",
                format!("deterministic user message {index:06} {padding}"),
            )
        } else {
            (
                "assistant",
                format!("deterministic assistant message {index:06} {padding}"),
            )
        };
        serde_json::to_writer(
            &mut bytes,
            &json!({
                "id": format!("cursor-scale-{index:06}"),
                "timestamp": "2026-07-24T12:00:00Z",
                "role": role,
                "message": {
                    "role": role,
                    "content": [{"type": "text", "text": text}]
                }
            }),
        )
        .unwrap();
        bytes.push(b'\n');
        digest_update(
            &mut expected_content,
            &[EventType::Message.as_str(), role, &text],
        );
        digest_update(
            &mut expected_identity,
            &[&format!("cursor-semantic-v1:{index}:0")],
        );
    }
    (
        bytes,
        format!("{:x}", expected_content.finalize()),
        format!("{:x}", expected_identity.finalize()),
    )
}

#[test]
fn large_cursor_scan_streams_exactly_into_bounded_pages() {
    let (bytes, expected_content, expected_identity) = cursor_scale_fixture();
    let mut sink = CursorScaleDigestSink::new();
    let parsed = scan_cursor_bytes_into_sink(&bytes, None, 1024 * 1024, &mut sink).unwrap();
    let parsed = match parsed {
        CursorParserOutcome::Parsed(parsed) => parsed,
        CursorParserOutcome::PrefixMismatch(_) => panic!("full Cursor scan cannot mismatch"),
    };
    let (content, identity) = sink.finish();

    assert!(
        parsed.events.is_empty(),
        "streaming core must not retain events"
    );
    assert_eq!(parsed.stats.retained_messages, 196_608);
    assert_eq!(parsed.stats.publication_pages, 3_072);
    assert_eq!(parsed.stats.nativepath_publication_rows, 196_608);
    assert_eq!(parsed.stats.max_publication_page_rows, 64);
    assert!(parsed.stats.max_publication_page_bytes <= super::CURSOR_PUBLICATION_PAGE_MAX_BYTES);
    assert_eq!(content, expected_content);
    assert_eq!(identity, expected_identity);
}

struct CursorPageBoundsSink {
    pages: usize,
    rows: usize,
    max_actual_bytes: usize,
    max_upper_bound_bytes: usize,
}

impl CursorPageBoundsSink {
    fn new() -> Self {
        Self {
            pages: 0,
            rows: 0,
            max_actual_bytes: 0,
            max_upper_bound_bytes: 0,
        }
    }
}

impl CursorPublicationSink for CursorPageBoundsSink {
    fn begin_cursor_publication(&mut self) -> crate::Result<()> {
        self.pages = 0;
        self.rows = 0;
        self.max_actual_bytes = 0;
        self.max_upper_bound_bytes = 0;
        Ok(())
    }

    fn stage_cursor_page(&mut self, page: CursorPublicationPage) -> crate::Result<()> {
        let actual_bytes = serde_json::to_vec(&json!({
            "events": &page.events,
            "retained_bytes": page.retained_bytes,
            "serialized_bytes": page.serialized_bytes,
            "page_sha256": "0".repeat(64),
        }))?
        .len();
        assert!(actual_bytes <= super::CURSOR_PUBLICATION_PAGE_MAX_BYTES);
        assert!(page.serialized_bytes <= super::CURSOR_PUBLICATION_PAGE_MAX_BYTES);
        assert!(actual_bytes <= page.serialized_bytes);
        self.pages += 1;
        self.rows += page.events.len();
        self.max_actual_bytes = self.max_actual_bytes.max(actual_bytes);
        self.max_upper_bound_bytes = self.max_upper_bound_bytes.max(page.serialized_bytes);
        Ok(())
    }

    fn abort_cursor_publication(&mut self) {}

    fn commit_cursor_publication(&mut self) -> crate::Result<()> {
        Ok(())
    }
}

#[test]
fn control_character_heavy_pages_stay_under_actual_serialized_limit() {
    let text = "\0".repeat(21_650);
    let bytes = jsonl((0..64).map(|_| user(&text)));
    let mut sink = CursorPageBoundsSink::new();
    let parsed =
        super::parser::scan_cursor_bytes_into_sink(&bytes, None, 1024 * 1024, &mut sink).unwrap();
    assert!(matches!(parsed, CursorParserOutcome::Parsed(_)));
    assert_eq!(sink.rows, 64);
    assert_eq!(sink.pages, 1);
    assert!(sink.max_actual_bytes <= super::CURSOR_PUBLICATION_PAGE_MAX_BYTES);
    assert!(sink.max_upper_bound_bytes <= super::CURSOR_PUBLICATION_PAGE_MAX_BYTES);
}

struct TransactionProbeSink {
    staged_rows: usize,
    committed_rows: usize,
    pages: usize,
    fail_on_page: Option<usize>,
    fail_commit: bool,
    mutate_path: Option<PathBuf>,
    began: bool,
    aborted: bool,
}

impl TransactionProbeSink {
    fn new(fail_on_page: Option<usize>, mutate_path: Option<PathBuf>) -> Self {
        Self {
            staged_rows: 0,
            committed_rows: 0,
            pages: 0,
            fail_on_page,
            fail_commit: false,
            mutate_path,
            began: false,
            aborted: false,
        }
    }
}

impl CursorPublicationSink for TransactionProbeSink {
    fn begin_cursor_publication(&mut self) -> crate::Result<()> {
        self.began = true;
        self.aborted = false;
        self.staged_rows = 0;
        self.pages = 0;
        Ok(())
    }

    fn stage_cursor_page(&mut self, page: CursorPublicationPage) -> crate::Result<()> {
        self.pages += 1;
        if let Some(path) = self.mutate_path.take() {
            let mut file = OpenOptions::new().append(true).open(path)?;
            file.write_all(b"{}\n")?;
        }
        if self.fail_on_page == Some(self.pages) {
            return Err(crate::CaptureError::SystemInvariant(
                "test publication sink failure",
            ));
        }
        self.staged_rows += page.events.len();
        Ok(())
    }

    fn abort_cursor_publication(&mut self) {
        self.staged_rows = 0;
        self.aborted = true;
    }

    fn commit_cursor_publication(&mut self) -> crate::Result<()> {
        if self.fail_commit {
            return Err(crate::CaptureError::SystemInvariant(
                "test publication commit failure",
            ));
        }
        self.committed_rows += self.staged_rows;
        self.staged_rows = 0;
        Ok(())
    }
}

#[test]
fn sink_error_aborts_all_prior_pages_before_publication() {
    let temp = tempdir();
    let root = temp.path().join("projects");
    let _path = write_transcript(
        &root,
        "project",
        "session",
        &jsonl((0..65).map(|_| user("x"))),
    );
    let source = one_source(&root);
    let frozen = freeze_cursor_source(&source).unwrap();
    let mut sink = TransactionProbeSink::new(Some(2), None);

    let error = super::scan_cursor_source_into(&frozen, None, &mut sink).unwrap_err();

    assert!(matches!(error, crate::CaptureError::SystemInvariant(_)));
    assert!(sink.began);
    assert!(sink.aborted);
    assert_eq!(sink.committed_rows, 0);
    assert_eq!(sink.staged_rows, 0);
    assert_eq!(sink.pages, 2);
}

#[test]
fn final_source_revalidation_aborts_pages_staged_before_mutation() {
    let temp = tempdir();
    let root = temp.path().join("projects");
    let path = write_transcript(&root, "project", "session", &jsonl([user("x")]));
    let source = one_source(&root);
    let frozen = freeze_cursor_source(&source).unwrap();
    let mut sink = TransactionProbeSink::new(None, Some(path));

    let error = super::scan_cursor_source_into(&frozen, None, &mut sink).unwrap_err();

    assert!(matches!(
        error,
        crate::CaptureError::SourceChangedDuringCapture
    ));
    assert!(sink.began);
    assert!(sink.aborted);
    assert_eq!(sink.committed_rows, 0);
    assert_eq!(sink.staged_rows, 0);
}

#[test]
fn commit_error_aborts_pages_staged_by_the_sink() {
    let temp = tempdir();
    let root = temp.path().join("projects");
    write_transcript(&root, "project", "session", &jsonl([user("x")]));
    let source = one_source(&root);
    let frozen = freeze_cursor_source(&source).unwrap();
    let mut sink = TransactionProbeSink::new(None, None);
    sink.fail_commit = true;

    let error = super::scan_cursor_source_into(&frozen, None, &mut sink).unwrap_err();

    assert!(matches!(error, crate::CaptureError::SystemInvariant(_)));
    assert!(sink.aborted);
    assert_eq!(sink.committed_rows, 0);
    assert_eq!(sink.staged_rows, 0);
}

#[test]
fn source_scan_models_append_rewrite_and_truncation() {
    let temp = tempdir();
    let root = temp.path().join("projects");
    let path = write_transcript(&root, "project", "session", &jsonl([user("base")]));
    let source = one_source(&root);
    let baseline =
        generation(scan_cursor_source(&freeze_cursor_source(&source).unwrap(), None).unwrap());
    let baseline_prior = prior(&baseline, "source-key");

    let mut append_file = OpenOptions::new().append(true).open(&path).unwrap();
    append_file
        .write_all(&jsonl([assistant("append")]))
        .unwrap();
    drop(append_file);
    let appended = generation(
        scan_cursor_source(
            &freeze_cursor_source(&source).unwrap(),
            Some(&baseline_prior),
        )
        .unwrap(),
    );
    assert_eq!(appended.mutation, CursorSourceMutation::AppendCandidate);
    assert_eq!(appended.events.len(), 1);
    assert_eq!(appended.events[0].identity.semantic_ordinal, 1);
    assert_eq!(appended.stats.publication_pages, 1);
    assert_eq!(appended.stats.nativepath_publication_rows, 1);
    let appended_session = appended.session.as_ref().unwrap();
    assert_eq!(appended_session.native_session_id, "session");
    assert_eq!(appended_session.title.as_deref(), Some("base"));
    assert_eq!(
        appended_session.started_at,
        Some("2026-07-24T12:00:00Z".parse().unwrap())
    );
    assert_eq!(
        appended_session.ended_at,
        Some("2026-07-24T12:00:01Z".parse().unwrap())
    );

    let append_prior = prior(&appended, "source-key");
    let rewritten_bytes = jsonl([user("same!"), assistant("bytes!")]);
    fs::write(&path, &rewritten_bytes).unwrap();
    let rewritten = generation(
        scan_cursor_source(&freeze_cursor_source(&source).unwrap(), Some(&append_prior)).unwrap(),
    );
    assert_eq!(rewritten.mutation, CursorSourceMutation::Rewrite);
    assert_eq!(rewritten.events.len(), 2);

    let rewrite_prior = prior(&rewritten, "source-key");
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(jsonl([user("short")]).len() as u64)
        .unwrap();
    let truncated = generation(
        scan_cursor_source(
            &freeze_cursor_source(&source).unwrap(),
            Some(&rewrite_prior),
        )
        .unwrap(),
    );
    assert_eq!(truncated.mutation, CursorSourceMutation::Truncation);
    assert_eq!(truncated.events.len(), 1);
}

#[test]
fn same_path_mutations_converge_without_platform_file_identity() {
    let temp = tempdir();
    let root = temp.path().join("projects");
    write_transcript(&root, "project", "session", &jsonl([user("base")]));
    let source = one_source(&root);
    let baseline =
        generation(scan_cursor_source(&freeze_cursor_source(&source).unwrap(), None).unwrap());
    let mut prior = prior(&baseline, "source-key");
    prior.observation.file_identity = None;
    prior.observation.changed = None;

    let replay = prior.observation.clone();
    assert_eq!(
        super::source::cursor_source_mutation(&replay, Some(&prior)),
        CursorSourceMutation::ExactReplay
    );

    let mut append = replay.clone();
    append.length = append.length.saturating_add(1);
    append.content_sha256 = [1; 32];
    assert_eq!(
        super::source::cursor_source_mutation(&append, Some(&prior)),
        CursorSourceMutation::AppendCandidate
    );

    let mut rewrite = replay.clone();
    rewrite.content_sha256 = [2; 32];
    assert_eq!(
        super::source::cursor_source_mutation(&rewrite, Some(&prior)),
        CursorSourceMutation::Rewrite
    );

    let mut truncation = replay;
    truncation.length = truncation.length.saturating_sub(1);
    truncation.content_sha256 = [3; 32];
    assert_eq!(
        super::source::cursor_source_mutation(&truncation, Some(&prior)),
        CursorSourceMutation::Truncation
    );
}

#[test]
fn strong_content_observation_detects_same_size_restored_mtime_rewrite() {
    let temp = tempdir();
    let root = temp.path().join("projects");
    let baseline_bytes = jsonl([user("alpha")]);
    let rewrite_bytes = jsonl([user("bravo")]);
    assert_eq!(baseline_bytes.len(), rewrite_bytes.len());
    let path = write_transcript(&root, "project", "session", &baseline_bytes);
    let original_modified = fs::metadata(&path).unwrap().modified().unwrap();
    let source = one_source(&root);
    let baseline =
        generation(scan_cursor_source(&freeze_cursor_source(&source).unwrap(), None).unwrap());
    let baseline_prior = prior(&baseline, "source-key");

    OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(
            fs::FileTimes::new()
                .set_modified(original_modified + std::time::Duration::from_secs(5)),
        )
        .unwrap();
    assert!(matches!(
        scan_cursor_source(
            &freeze_cursor_source(&source).unwrap(),
            Some(&baseline_prior),
        )
        .unwrap(),
        CursorReadOutcome::Unchanged(_)
    ));

    fs::write(&path, &rewrite_bytes).unwrap();
    OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(original_modified))
        .unwrap();
    let rewritten = generation(
        scan_cursor_source(
            &freeze_cursor_source(&source).unwrap(),
            Some(&baseline_prior),
        )
        .unwrap(),
    );
    assert_eq!(rewritten.mutation, CursorSourceMutation::Rewrite);
    assert_ne!(
        rewritten.observation.content_sha256,
        baseline.observation.content_sha256
    );
    assert_eq!(rewritten.events.len(), 1);
}

#[test]
fn zero_row_sources_still_produce_exact_observation_and_checkpoint_authority() {
    let temp = tempdir();
    let root = temp.path().join("projects");
    write_transcript(&root, "project", "empty", b"");
    let source = one_source(&root);

    let empty =
        generation(scan_cursor_source(&freeze_cursor_source(&source).unwrap(), None).unwrap());

    assert!(empty.events.is_empty());
    assert!(empty.session.is_none());
    assert_eq!(empty.rejections.total, 0);
    assert!(empty.checkpoint.terminal);
    assert_eq!(empty.checkpoint.next_byte_offset, 0);
    assert_eq!(
        empty.checkpoint.disposition,
        super::CursorCheckpointDisposition::Publish
    );

    fs::write(&empty.observation.path, b"{\"malformed\"\n").unwrap();
    let malformed =
        generation(scan_cursor_source(&freeze_cursor_source(&source).unwrap(), None).unwrap());
    assert!(malformed.events.is_empty());
    assert!(malformed.session.is_none());
    assert_eq!(malformed.rejections.total, 1);
    assert!(malformed.checkpoint.terminal);
    assert_eq!(
        malformed.checkpoint.disposition,
        super::CursorCheckpointDisposition::WithholdForRejections
    );

    fs::write(
        &malformed.observation.path,
        serde_json::to_vec(&assistant("incomplete")).unwrap(),
    )
    .unwrap();
    let incomplete =
        generation(scan_cursor_source(&freeze_cursor_source(&source).unwrap(), None).unwrap());
    assert!(incomplete.events.is_empty());
    assert!(incomplete.session.is_none());
    assert!(!incomplete.checkpoint.terminal);
    assert_eq!(incomplete.stats.incomplete_tail_records, 1);
}
