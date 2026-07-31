use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use ctx_history_core::{
    BatchHydrationRequest, CaptureProvider, ContentSourceResolver, EventHydrationRequest,
    HydrationFailureKind, LocatorRevisionPolicy, NativeRecordCoordinate, SourceRecordLocator,
    TypedKey,
};
use ctx_history_index::{LexicalDocument, VerifiedIndex, WriterOptions};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{
    provider::source_backed::{
        family::jsonl::{
            jsonl_family_projection_bytes, jsonl_family_work, jsonl_prefix_hash_bytes,
            reset_jsonl_family_work, reset_jsonl_prefix_hash_bytes, JsonlFamilyWork,
        },
        refresh_source_backed_generation, register_landed_source_backed_route,
        SourceBackedProviderRegistry, SourceBackedRouteSelection,
    },
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceStatus,
};

fn registry(root: &Path) -> SourceBackedProviderRegistry {
    registry_for_roots(&[(root, SourceBackedRouteSelection::Automatic)])
}

fn registry_for_roots(
    roots: &[(&Path, SourceBackedRouteSelection)],
) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    for (root, selection) in roots {
        let source = ProviderSource {
            provider: CaptureProvider::Claude,
            path: (*root).to_path_buf(),
            exists: true,
            source_format: "claude_projects_jsonl_tree",
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
        };
        register_landed_source_backed_route(&mut registry, source, *selection).unwrap();
    }
    registry
}

#[test]
fn shared_family_empty_automatic_root_coexists_with_distinct_explicit_replay() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let automatic = temp.path().join(".claude/projects");
    let explicit = temp.path().join("explicit/projects");
    fs::create_dir_all(&automatic).unwrap();
    write_lines(
        &session_path(&explicit, "-project", "session-1"),
        &[message("session-1", "message-1", "explicit body")],
    );
    let registry = registry_for_roots(&[
        (&automatic, SourceBackedRouteSelection::Automatic),
        (&explicit, SourceBackedRouteSelection::ExplicitManual),
    ]);
    let index_root = temp.path().join("index");

    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(cold.sources.len(), 1);
    let replay =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(replay.sources, cold.sources);
    assert_eq!(replay.commit.generation_id, cold.commit.generation_id);
    let source = replay.sources[0].observation().source();
    let event = VerifiedIndex::open(&index_root)
        .unwrap()
        .source_event_page(source, None, 1)
        .unwrap()
        .items
        .remove(0);
    let hydrated = registry
        .resolver_registry()
        .hydrate_event(&EventHydrationRequest::new(event.event_id, event.locator.clone()).unwrap())
        .unwrap();
    assert_eq!(hydrated.provider_bytes, b"explicit body");
}

fn writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

#[test]
fn shared_family_claude_noop_replacement_lineage_and_hydration_oracle() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let projects = temp.path().join(".claude/projects");
    let primary = session_path(&projects, "-project", "session-1");
    let subagent = projects.join("-project/session-1/subagents/agent-review.jsonl");
    write_lines(
        &primary,
        &[
            message("session-1", "message-1", "claude exact"),
            message("session-1", "message-2", "claude response"),
        ],
    );
    write_lines(
        &subagent,
        &[message("session-1", "subagent-message", "subagent body")],
    );
    let registry = registry(&projects);
    let index_root = temp.path().join("index");

    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(cold.sources.len(), 2);
    assert_eq!(
        cold.sources
            .iter()
            .map(|source| source.counts().indexed_documents)
            .sum::<u64>(),
        3
    );

    reset_jsonl_family_work();
    let unchanged =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(jsonl_family_work().provider_projections, 0);
    assert_eq!(unchanged.sources, cold.sources);
    assert_eq!(unchanged.commit.generation_id, cold.commit.generation_id);
    assert_eq!(unchanged.commit.opstamp, cold.commit.opstamp);

    let primary_source = cold
        .sources
        .iter()
        .find(|source| source.counts().indexed_documents == 2)
        .unwrap()
        .observation()
        .source();
    let index = VerifiedIndex::open(&index_root).unwrap();
    let mut events = index
        .source_event_page(primary_source, None, 10)
        .unwrap()
        .items;
    events.sort_by_key(|event| event.event_sequence);
    assert!(events.iter().all(|event| {
        event.parent_session_id.is_none()
            && event.root_session_id == event.session_id
            && event.agent_type == "primary"
            && event.is_primary
            && event.branch.as_deref() == Some("main")
            && event.cwd.as_deref() == Some("/workspace/project")
            && event.locator.revision_policy() == LocatorRevisionPolicy::StableRecordEvidence
    }));
    let requests = events
        .iter()
        .rev()
        .map(|event| EventHydrationRequest::new(event.event_id, event.locator.clone()).unwrap())
        .collect::<Vec<_>>();
    reset_jsonl_family_work();
    let hydrated = registry
        .resolver_registry()
        .hydrate_batch(&BatchHydrationRequest::new(requests.clone()).unwrap())
        .unwrap()
        .into_records();
    assert_eq!(
        hydrated
            .iter()
            .map(|record| record.provider_bytes.as_slice())
            .collect::<Vec<_>>(),
        vec![b"claude response".as_slice(), b"claude exact".as_slice()]
    );
    assert_eq!(
        jsonl_family_work(),
        JsonlFamilyWork {
            discoveries: 0,
            leaf_opens: 1,
            provider_projections: 0,
        }
    );
    let mut digest = Sha256::new();
    for (request, record) in requests.iter().zip(hydrated) {
        digest.update(request.event_id().digest());
        digest.update((record.provider_bytes.len() as u64).to_be_bytes());
        digest.update(record.provider_bytes);
    }
    assert_eq!(
        format!("{:x}", digest.finalize()),
        "c454f0ccd49ec9598691b60c969a1c6f77b7aa685de2a40289e5dac8ab32394a"
    );

    let before = fs::read_to_string(&primary).unwrap();
    let rewritten = before.replace("claude exact", "claude other");
    assert_eq!(rewritten.len(), before.len());
    fs::write(&primary, rewritten).unwrap();
    reset_jsonl_family_work();
    let rewrite =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(
        jsonl_family_work().provider_projections,
        2,
        "same-length Claude rewrite is one replacement pass"
    );
    assert_ne!(rewrite.commit.generation_id, cold.commit.generation_id);

    let rewrite_primary = rewrite
        .sources
        .iter()
        .find(|source| source.counts().indexed_documents == 2)
        .unwrap();
    let rewrite_frontier = rewrite_primary.frontier().unwrap();
    let frozen_prefix_digest = *rewrite_frontier.certified_prefix_digest();
    let mut appended = message("session-1", "message-3", "claude growth");
    let appended_object = appended.as_object_mut().unwrap();
    appended_object.remove("cwd");
    appended_object.remove("version");
    appended_object.remove("gitBranch");
    append_record(&primary, &appended);
    let replacement_payload_bytes = fs::read(&primary).unwrap().len() - 3;
    reset_jsonl_family_work();
    reset_jsonl_prefix_hash_bytes();
    let growth =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(
        jsonl_family_work().provider_projections,
        3,
        "Claude growth must replay prior session metadata"
    );
    assert_eq!(
        jsonl_family_projection_bytes(),
        replacement_payload_bytes,
        "replacement-only Claude reparses every payload byte"
    );
    assert_eq!(
        jsonl_prefix_hash_bytes(),
        0,
        "replacement-only Claude does not attempt append certification"
    );
    let growth_primary = growth
        .sources
        .iter()
        .find(|source| source.counts().indexed_documents == 3)
        .unwrap();
    assert_eq!(growth_primary.counts().complete_records, 3);
    assert_eq!(growth_primary.counts().indexed_documents, 3);
    assert_eq!(
        growth_primary.frontier().unwrap().certified_prefix_bytes(),
        fs::metadata(&primary).unwrap().len()
    );
    assert_ne!(
        growth_primary.frontier().unwrap().certified_prefix_digest(),
        &frozen_prefix_digest
    );
    assert_eq!(
        growth
            .sources
            .iter()
            .map(|source| source.counts().indexed_documents)
            .sum::<u64>(),
        4
    );
    let mut primary_events = VerifiedIndex::open(&index_root)
        .unwrap()
        .source_event_page(growth_primary.observation().source(), None, 10)
        .unwrap()
        .items;
    primary_events.sort_by_key(|event| event.event_sequence);
    let appended_event = primary_events.last().unwrap();
    assert_eq!(appended_event.branch.as_deref(), Some("main"));
    assert_eq!(appended_event.cwd.as_deref(), Some("/workspace/project"));

    let subagent_event = VerifiedIndex::open(&index_root)
        .unwrap()
        .source_event_page(
            growth
                .sources
                .iter()
                .find(|source| source.counts().indexed_documents == 1)
                .unwrap()
                .observation()
                .source(),
            None,
            4,
        )
        .unwrap()
        .items
        .remove(0);
    assert_eq!(subagent_event.agent_type, "subagent");
    assert!(!subagent_event.is_primary);
    assert!(subagent_event.parent_session_id.is_some());
    assert_eq!(
        subagent_event.root_session_id,
        subagent_event.parent_session_id.unwrap()
    );
}

#[test]
fn shared_family_claude_hydrates_every_projected_compound_row_by_native_key() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let projects = temp.path().join(".claude/projects");
    let path = session_path(&projects, "-project", "session-1");
    write_lines(
        &path,
        &[
            message("session-1", "message-1", "message body"),
            json!({
                "sessionId": "session-1",
                "type": "system",
                "uuid": "notice-1",
                "summary": "notice body"
            }),
            json!({
                "sessionId": "session-1",
                "type": "summary",
                "uuid": "summary-1",
                "summary": "summary body"
            }),
            json!({
                "sessionId": "session-1",
                "type": "assistant",
                "uuid": "tool-only-1",
                "message": {
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": "call-pure",
                        "name": "Read",
                        "input": {"file_path": "src/lib.rs"}
                    }]
                }
            }),
            json!({
                "sessionId": "session-1",
                "type": "assistant",
                "uuid": "compound-1",
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "compound body"},
                        {
                            "type": "tool_use",
                            "id": "call-compound",
                            "name": "Edit",
                            "input": {"file_path": "src/main.rs"}
                        }
                    ]
                }
            }),
            json!({
                "sessionId": "session-1",
                "type": "user",
                "uuid": "failed-output-1",
                "message": {
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "call-failed",
                        "is_error": true,
                        "content": "provider output is not retained"
                    }]
                }
            }),
            json!({
                "sessionId": "session-1",
                "type": "user",
                "uuid": "timeout-output-1",
                "toolUseResult": {"status": "timeout"},
                "message": {
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "call-timeout",
                        "status": "timeout",
                        "content": "provider output is not retained"
                    }]
                }
            }),
        ],
    );
    let registry = registry(&projects);
    let index_root = temp.path().join("index");
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(cold.sources.len(), 1);
    assert_eq!(cold.sources[0].counts().indexed_documents, 8);

    let source = cold.sources[0].observation().source();
    let mut events = VerifiedIndex::open(&index_root)
        .unwrap()
        .source_event_page(source, None, 12)
        .unwrap()
        .items;
    events.sort_by_key(|event| event.event_sequence);
    assert_eq!(
        events
            .iter()
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>(),
        vec![
            "message",
            "notice",
            "summary",
            "tool_call",
            "message",
            "tool_call",
            "tool_output",
            "tool_output",
        ]
    );
    let requests = events
        .iter()
        .rev()
        .map(|event| EventHydrationRequest::new(event.event_id, event.locator.clone()).unwrap())
        .collect::<Vec<_>>();
    let hydrated = registry
        .resolver_registry()
        .hydrate_batch(&BatchHydrationRequest::new(requests).unwrap())
        .unwrap()
        .into_records();
    assert_eq!(
        hydrated
            .iter()
            .map(|record| String::from_utf8(record.provider_bytes.clone()).unwrap())
            .collect::<Vec<_>>(),
        vec![
            "tool output timeout call-timeout",
            "tool output failure call-failed",
            "tool call Edit call-compound src/main.rs",
            "compound body",
            "tool call Read call-pure src/lib.rs",
            "summary body",
            "notice body",
            "message body",
        ]
    );

    let first = &events[0];
    let second = &events[1];
    let mismatched_identity =
        EventHydrationRequest::new(second.event_id, first.locator.clone()).unwrap();
    let failure = registry
        .resolver_registry()
        .hydrate_event(&mismatched_identity)
        .unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::InvalidLocator);

    let NativeRecordCoordinate::Jsonl {
        byte_offset,
        byte_length,
        physical_ordinal,
        native_session_key,
        ..
    } = first.locator.coordinate()
    else {
        panic!("Claude locator must remain JSONL")
    };
    let mutated_locator = SourceRecordLocator::new(
        first.locator.source().clone(),
        NativeRecordCoordinate::Jsonl {
            byte_offset: *byte_offset,
            byte_length: *byte_length,
            physical_ordinal: *physical_ordinal,
            native_session_key: native_session_key.clone(),
            native_event_key: Some(TypedKey::utf8("mutated-event").unwrap()),
        },
        first.locator.revision_policy(),
        first.locator.certified_source_revision_digest().copied(),
        *first.locator.record_digest(),
    )
    .unwrap();
    let failure = registry
        .resolver_registry()
        .hydrate_event(&EventHydrationRequest::new(first.event_id, mutated_locator).unwrap())
        .unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::InvalidLocator);
}

#[test]
fn shared_family_claude_hydrates_full_source_text_beyond_index_retention() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let projects = temp.path().join(".claude/projects");
    let path = session_path(&projects, "-project", "session-1");
    let long_message = format!("{}ordinary-unique-tail", "m".repeat(20_000));
    let long_compound = format!("{}compound-unique-tail", "c".repeat(20_000));
    let ordinary_record = message("session-1", "message-long", &long_message);
    let compound_record = json!({
        "sessionId": "session-1",
        "type": "assistant",
        "uuid": "compound-long",
        "message": {
            "role": "assistant",
            "content": [
                {"type": "text", "text": long_compound},
                {
                    "type": "tool_use",
                    "id": "call-long",
                    "name": "Read",
                    "input": {"file_path": "src/long.rs"}
                }
            ]
        }
    });
    let ordinary_documents = project_test_record(&ordinary_record);
    assert_eq!(ordinary_documents.len(), 1);
    assert_eq!(ordinary_documents[0].body, long_message);
    assert!(ordinary_documents[0].body.ends_with("ordinary-unique-tail"));
    let compound_documents = project_test_record(&compound_record);
    assert_eq!(compound_documents.len(), 2);
    assert_eq!(compound_documents[0].body, long_compound);
    assert!(compound_documents[0].body.ends_with("compound-unique-tail"));
    write_lines(
        &path,
        &[
            ordinary_record,
            json!({
                "sessionId": "session-1",
                "type": "system",
                "uuid": "notice-1",
                "summary": "source-authored notice"
            }),
            compound_record,
        ],
    );
    let registry = registry(&projects);
    let index_root = temp.path().join("index");
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let mut events = VerifiedIndex::open(&index_root)
        .unwrap()
        .source_event_page(cold.sources[0].observation().source(), None, 8)
        .unwrap()
        .items;
    events.sort_by_key(|event| event.event_sequence);
    assert_eq!(events.len(), 4);
    let hydrated = registry
        .resolver_registry()
        .hydrate_batch(
            &BatchHydrationRequest::new(
                events
                    .iter()
                    .map(|event| {
                        EventHydrationRequest::new(event.event_id, event.locator.clone()).unwrap()
                    })
                    .collect(),
            )
            .unwrap(),
        )
        .unwrap()
        .into_records()
        .into_iter()
        .map(|record| String::from_utf8(record.provider_bytes).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(hydrated[0], long_message);
    assert!(hydrated[0].ends_with("ordinary-unique-tail"));
    assert_eq!(hydrated[1], "source-authored notice");
    assert_eq!(hydrated[2], long_compound);
    assert!(hydrated[2].ends_with("compound-unique-tail"));
    assert_eq!(hydrated[3], "tool call Read call-long src/long.rs");
}

#[test]
fn shared_family_claude_same_length_rewrite_is_stale_record_evidence() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let projects = temp.path().join(".claude/projects");
    let path = session_path(&projects, "-project", "session-1");
    write_lines(&path, &[message("session-1", "message-1", "original body")]);
    let registry = registry(&projects);
    let index_root = temp.path().join("index");
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let event = VerifiedIndex::open(&index_root)
        .unwrap()
        .source_event_page(cold.sources[0].observation().source(), None, 1)
        .unwrap()
        .items
        .remove(0);
    let before = fs::read_to_string(&path).unwrap();
    let after = before.replace("original body", "rewritten bod");
    assert_eq!(before.len(), after.len());
    fs::write(&path, after).unwrap();

    let failure = registry
        .resolver_registry()
        .hydrate_event(&EventHydrationRequest::new(event.event_id, event.locator).unwrap())
        .unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::StaleRecordEvidence);
}

#[cfg(unix)]
#[test]
fn shared_family_claude_accepts_hardlinked_leaves_without_resident_file_handles() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let projects = temp.path().join(".claude/projects");
    let first = session_path(&projects, "-project", "session-1");
    write_lines(
        &first,
        &[message("session-1", "message-1", "hardlink body")],
    );
    fs::hard_link(&first, session_path(&projects, "-project", "session-2")).unwrap();
    let result = refresh_source_backed_generation(
        temp.path().join("index"),
        &registry(&projects),
        writer_options(),
    )
    .unwrap();
    assert_eq!(result.sources.len(), 2);
}

#[test]
fn shared_family_claude_complete_deletion_and_missing_root_are_distinct() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let projects = temp.path().join(".claude/projects");
    let path = session_path(&projects, "-project", "session-1");
    write_lines(&path, &[message("session-1", "message-1", "delete body")]);
    let registry = registry(&projects);
    let index_root = temp.path().join("index");
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(cold.sources.len(), 1);

    fs::remove_file(&path).unwrap();
    let deleted =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(deleted.sources.is_empty());

    fs::remove_dir_all(&projects).unwrap();
    assert!(refresh_source_backed_generation(&index_root, &registry, writer_options()).is_err());
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().generation_id(),
        deleted.commit.generation_id
    );
}

fn session_path(projects: &Path, project: &str, session: &str) -> PathBuf {
    projects.join(project).join(format!("{session}.jsonl"))
}

fn message(session: &str, uuid: &str, text: &str) -> Value {
    json!({
        "sessionId": session,
        "type": "user",
        "uuid": uuid,
        "timestamp": "2026-01-01T00:00:00.000Z",
        "cwd": "/workspace/project",
        "version": "2.1.219",
        "gitBranch": "main",
        "message": {
            "role": "user",
            "content": [{"type": "text", "text": text}]
        }
    })
}

fn write_lines(path: &Path, lines: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = Vec::new();
    for line in lines {
        serde_json::to_writer(&mut bytes, line).unwrap();
        bytes.push(b'\n');
    }
    fs::write(path, bytes).unwrap();
}

fn append_record(path: &Path, record: &Value) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    serde_json::to_writer(&mut file, record).unwrap();
    file.write_all(b"\n").unwrap();
}

fn project_test_record(record: &Value) -> Vec<LexicalDocument> {
    let bytes = serde_json::to_vec(record).unwrap();
    let locator = super::ClaudePhysicalLocator {
        path: PathBuf::from("/workspace/project/session-1.jsonl"),
        byte_start: 0,
        byte_end_exclusive: u64::try_from(bytes.len()).unwrap(),
        line_number: 1,
        record_sha256: Sha256::digest(&bytes).into(),
    };
    let parsed = super::parse_native_record(&bytes, 0, &locator).unwrap();
    let binding = super::Binding {
        project_dir: PathBuf::from("/workspace/project"),
        key: super::ClaudeSessionKey {
            root_session_id: "session-1".to_owned(),
            workflow_run_id: None,
            agent_id: None,
        },
        layout: super::SessionLayout::Primary,
    };
    let source = super::source_key(&binding.key).unwrap();
    let identities = super::identities(&binding).unwrap();
    let mut session = super::ClaudeSessionMetadata::new(binding.key.clone());
    session.observe(
        parsed.timestamp.as_deref(),
        parsed.cwd.as_deref(),
        parsed.version.as_deref(),
        parsed.git_branch.as_deref(),
    );
    parsed
        .rows
        .into_iter()
        .map(|row| {
            super::lexical_document(
                &source,
                "/workspace/project/session-1.jsonl",
                &binding,
                &identities,
                &session,
                row,
            )
            .unwrap()
        })
        .collect()
}
