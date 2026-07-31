use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use ctx_history_core::{
    BatchHydrationRequest, CaptureProvider, ContentSourceResolver, EventHydrationRequest,
    HydrationFailureKind, LocatorRevisionPolicy,
};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use serde_json::{json, Value};

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
    ProviderSourceStatus, PROVIDER_MAX_TEXT_CHARS,
};

fn registry(root: &Path) -> SourceBackedProviderRegistry {
    let source = ProviderSource {
        provider: CaptureProvider::Cursor,
        path: root.to_path_buf(),
        exists: true,
        source_format: "cursor_agent_transcript_jsonl_tree",
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
    };
    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        source,
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    registry
}

fn writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

fn transcript_path(root: &Path, session: &str) -> PathBuf {
    root.join("projects")
        .join("project")
        .join("agent-transcripts")
        .join(session)
        .join(format!("{session}.jsonl"))
}

fn write_transcript(root: &Path, session: &str, rows: &[Value]) -> PathBuf {
    let path = transcript_path(root, session);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = Vec::new();
    for row in rows {
        serde_json::to_writer(&mut bytes, row).unwrap();
        bytes.push(b'\n');
    }
    fs::write(&path, bytes).unwrap();
    path
}

fn append_transcript(path: &Path, row: &Value) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    serde_json::to_writer(&mut file, row).unwrap();
    file.write_all(b"\n").unwrap();
}

fn user(text: &str) -> Value {
    json!({
        "timestamp": "2026-07-24T12:00:00Z",
        "role": "user",
        "message": {
            "role": "user",
            "content": [{"type": "text", "text": text}]
        }
    })
}

fn assistant(text: &str) -> Value {
    json!({
        "timestamp": "2026-07-24T12:00:01Z",
        "role": "assistant",
        "message": {
            "role": "assistant",
            "content": [{"type": "text", "text": text}]
        }
    })
}

fn multipart() -> Value {
    json!({
        "timestamp": "2026-07-24T12:00:01Z",
        "role": "assistant",
        "message": {
            "role": "assistant",
            "content": [
                {"type": "text", "text": "first"},
                {
                    "type": "tool_use",
                    "id": "call-1",
                    "name": "write_file",
                    "input": {"path": "src/main.rs"}
                },
                {"type": "text", "text": "second"}
            ]
        }
    })
}

fn tool_result(text: &str) -> Value {
    json!({
        "timestamp": "2026-07-24T12:00:02Z",
        "role": "user",
        "message": {
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "call-1",
                "content": text
            }]
        }
    })
}

#[test]
fn shared_family_cursor_cold_noop_and_grouped_full_body_hydration() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("cursor-data");
    let long = format!(
        "needle {}{}",
        "prefix-".repeat(1_024),
        "suffix-".repeat(PROVIDER_MAX_TEXT_CHARS)
    );
    let rows = [user(&long), assistant("Cursor response")];
    let cold_payload_bytes = rows
        .iter()
        .map(|row| serde_json::to_vec(row).unwrap().len())
        .sum::<usize>();
    write_transcript(&root, "session-a", &rows);
    let registry = registry(&root);
    let index_root = temp.path().join("index");

    reset_jsonl_family_work();
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(cold.sources.len(), 1);
    assert_eq!(cold.sources[0].counts().complete_records, 2);
    assert_eq!(cold.sources[0].counts().indexed_documents, 2);
    assert_eq!(jsonl_family_work().provider_projections, 2);
    assert_eq!(jsonl_family_projection_bytes(), cold_payload_bytes);

    reset_jsonl_family_work();
    reset_jsonl_prefix_hash_bytes();
    let unchanged =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(jsonl_family_work().provider_projections, 0);
    assert_eq!(jsonl_family_projection_bytes(), 0);
    assert_eq!(jsonl_prefix_hash_bytes(), 0);
    assert_eq!(unchanged.sources, cold.sources);
    assert_eq!(unchanged.commit.generation_id, cold.commit.generation_id);
    assert_eq!(unchanged.commit.opstamp, cold.commit.opstamp);

    let source = cold.sources[0].observation().source();
    let index = VerifiedIndex::open(&index_root).unwrap();
    let mut events = index.source_event_page(source, None, 10).unwrap().items;
    events.sort_by_key(|event| event.event_sequence);
    assert_eq!(events.len(), 2);
    assert_eq!(
        index
            .search_event_candidates("needle", 10)
            .unwrap()
            .first()
            .map(|candidate| candidate.event.event_id),
        Some(events[0].event_id)
    );
    assert!(events.iter().all(|event| {
        event.locator.revision_policy() == LocatorRevisionPolicy::StableRecordEvidence
            && event.locator.certified_source_revision_digest().is_none()
    }));
    let requests = events
        .iter()
        .rev()
        .map(|event| EventHydrationRequest::new(event.event_id, event.locator.clone()).unwrap())
        .collect::<Vec<_>>();
    reset_jsonl_family_work();
    let hydrated = registry
        .resolver_registry()
        .hydrate_batch(&BatchHydrationRequest::new(requests).unwrap())
        .unwrap()
        .into_records();
    assert_eq!(
        hydrated
            .iter()
            .map(|record| record.provider_bytes.as_slice())
            .collect::<Vec<_>>(),
        vec![b"Cursor response".as_slice(), long.as_bytes()]
    );
    assert_eq!(
        jsonl_family_work(),
        JsonlFamilyWork {
            discoveries: 0,
            leaf_opens: 1,
            provider_projections: 0,
        }
    );
}

#[test]
fn shared_family_cursor_replacement_truncate_deletion_and_unavailable_root() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("cursor-data");
    let transcript = write_transcript(&root, "session-a", &[user("before"), assistant("second")]);
    let registry = registry(&root);
    let index_root = temp.path().join("index");
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let source = cold.sources[0].observation().source().clone();
    let cold_events = {
        let index = VerifiedIndex::open(&index_root).unwrap();
        let mut events = index.source_event_page(&source, None, 10).unwrap().items;
        events.sort_by_key(|event| event.event_sequence);
        events
    };
    let cold_ids = cold_events
        .iter()
        .map(|event| event.event_id)
        .collect::<Vec<_>>();

    let before = fs::read_to_string(&transcript).unwrap();
    let rewritten = before.replace("before", "change");
    assert_eq!(rewritten.len(), before.len());
    fs::write(&transcript, rewritten).unwrap();
    reset_jsonl_family_work();
    let replaced =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(
        jsonl_family_work().provider_projections,
        2,
        "same-length rewrite must be one complete replacement pass"
    );
    assert_ne!(replaced.commit.generation_id, cold.commit.generation_id);
    let index = VerifiedIndex::open(&index_root).unwrap();
    let mut events = index.source_event_page(&source, None, 10).unwrap().items;
    events.sort_by_key(|event| event.event_sequence);
    assert_eq!(
        events
            .iter()
            .map(|event| event.event_id)
            .collect::<Vec<_>>(),
        cold_ids
    );
    let stale = registry
        .resolver_registry()
        .hydrate_event(
            &EventHydrationRequest::new(cold_events[0].event_id, cold_events[0].locator.clone())
                .unwrap(),
        )
        .unwrap_err();
    assert_eq!(stale.kind, HydrationFailureKind::StaleRecordEvidence);
    let unchanged = registry
        .resolver_registry()
        .hydrate_event(
            &EventHydrationRequest::new(cold_events[1].event_id, cold_events[1].locator.clone())
                .unwrap(),
        )
        .unwrap();
    assert_eq!(unchanged.provider_bytes, b"second");

    let replacement_events = events;
    let replacement_requests = replacement_events
        .iter()
        .map(|event| EventHydrationRequest::new(event.event_id, event.locator.clone()).unwrap())
        .collect::<Vec<_>>();
    let frozen_prefix_bytes = replaced.sources[0]
        .frontier()
        .unwrap()
        .certified_prefix_bytes();
    let grown_row = user("grown");
    let grown_payload_bytes = serde_json::to_vec(&grown_row).unwrap().len();
    append_transcript(&transcript, &grown_row);
    reset_jsonl_family_work();
    reset_jsonl_prefix_hash_bytes();
    let grown = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(
        jsonl_family_work().provider_projections,
        1,
        "certified Cursor growth projects only the appended record"
    );
    assert_eq!(
        jsonl_family_projection_bytes(),
        grown_payload_bytes,
        "the Cursor projector receives only appended payload bytes"
    );
    assert_eq!(
        jsonl_prefix_hash_bytes(),
        frozen_prefix_bytes,
        "Cursor append certification rehashes exactly the frozen prefix"
    );
    assert_eq!(grown.sources[0].counts().indexed_documents, 3);
    let mut grown_events = VerifiedIndex::open(&index_root)
        .unwrap()
        .source_event_page(&source, None, 10)
        .unwrap()
        .items;
    grown_events.sort_by_key(|event| event.event_sequence);
    assert_eq!(
        grown_events
            .iter()
            .take(2)
            .map(|event| (event.event_id, event.event_sequence, event.locator.clone()))
            .collect::<Vec<_>>(),
        replacement_events
            .iter()
            .map(|event| (event.event_id, event.event_sequence, event.locator.clone()))
            .collect::<Vec<_>>(),
        "pre-append Cursor IDs, order, and locators must remain exact"
    );
    assert_eq!(
        grown_events
            .iter()
            .map(|event| event.event_sequence)
            .collect::<Vec<_>>(),
        vec![0, 1 << 16, 2 << 16]
    );
    let hydrated = registry
        .resolver_registry()
        .hydrate_batch(&BatchHydrationRequest::new(replacement_requests).unwrap())
        .unwrap()
        .into_records();
    assert_eq!(
        hydrated
            .iter()
            .map(|record| record.provider_bytes.as_slice())
            .collect::<Vec<_>>(),
        vec![b"change".as_slice(), b"second".as_slice()]
    );
    let grown_request =
        EventHydrationRequest::new(grown_events[2].event_id, grown_events[2].locator.clone())
            .unwrap();

    write_transcript(&root, "session-a", &[user("truncated")]);
    reset_jsonl_family_work();
    let truncated =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(jsonl_family_work().provider_projections, 1);
    assert_eq!(truncated.sources[0].counts().indexed_documents, 1);
    let stale = registry
        .resolver_registry()
        .hydrate_event(&grown_request)
        .unwrap_err();
    assert_eq!(stale.kind, HydrationFailureKind::StaleRecordEvidence);

    fs::remove_dir_all(transcript.parent().unwrap()).unwrap();
    let deleted =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(deleted.sources.is_empty());
    assert_ne!(deleted.commit.generation_id, truncated.commit.generation_id);

    fs::remove_dir_all(&root).unwrap();
    assert!(
        refresh_source_backed_generation(&index_root, &registry, writer_options()).is_err(),
        "a missing root is unavailable, not a deletion certificate"
    );
    let index = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(index.generation_id(), deleted.commit.generation_id);
}

#[test]
fn shared_family_cursor_identity_projection_and_exact_content_parity() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("cursor-data");
    let transcript = transcript_path(&root, "session-a");
    fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    let rows = [
        user("opening"),
        multipart(),
        tool_result("CURSOR_OUTPUT_BODY_MUST_NOT_BE_INDEXED"),
        user("closing"),
    ];
    let mut bytes = b"not-json\n".to_vec();
    for row in rows {
        serde_json::to_writer(&mut bytes, &row).unwrap();
        bytes.push(b'\n');
    }
    fs::write(&transcript, bytes).unwrap();
    let registry = registry(&root);
    let index_root = temp.path().join("index");

    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(cold.sources.len(), 1);
    assert_eq!(cold.sources[0].counts().indexed_documents, 4);
    let source = cold.sources[0].observation().source();
    let index = VerifiedIndex::open(&index_root).unwrap();
    let mut events = index.source_event_page(source, None, 10).unwrap().items;
    events.sort_by_key(|event| event.event_sequence);
    assert_eq!(
        events
            .iter()
            .map(|event| event.event_sequence)
            .collect::<Vec<_>>(),
        vec![1 << 16, 2 << 16, (2 << 16) + 2, 4 << 16]
    );
    assert!(events.iter().all(|event| {
        event.provider_session_id.as_deref() == Some("session-a")
            && event.agent_type == "primary"
            && event.is_primary
            && event.source_path.as_deref() == transcript.to_str()
            && event.locator.revision_policy() == LocatorRevisionPolicy::StableRecordEvidence
            && event.locator.certified_source_revision_digest().is_none()
    }));
    for (query, event) in ["opening", "first", "second", "closing"]
        .into_iter()
        .zip(&events)
    {
        assert_eq!(
            index
                .search_event_candidates(query, 10)
                .unwrap()
                .first()
                .map(|candidate| candidate.event.event_id),
            Some(event.event_id)
        );
    }
    assert!(index
        .search_event_candidates("CURSOR_OUTPUT_BODY_MUST_NOT_BE_INDEXED", 10)
        .unwrap()
        .is_empty());

    let requests = events
        .iter()
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
            .map(|record| record.provider_bytes.as_slice())
            .collect::<Vec<_>>(),
        vec![
            b"opening".as_slice(),
            b"first".as_slice(),
            b"second".as_slice(),
            b"closing".as_slice(),
        ]
    );
}
