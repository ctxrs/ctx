use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use ctx_history_core::{
    BatchHydrationRequest, CaptureProvider, ContentSourceResolver, EventHydrationRequest,
    LocatorRevisionPolicy,
};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{
    provider::source_backed::{
        family::jsonl::{jsonl_family_work, reset_jsonl_family_work, JsonlFamilyWork},
        refresh_source_backed_generation, register_landed_source_backed_route,
        SourceBackedProviderRegistry, SourceBackedRouteSelection,
    },
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceStatus,
};

fn registry(root: &Path) -> SourceBackedProviderRegistry {
    let source = ProviderSource {
        provider: CaptureProvider::Mux,
        path: root.to_path_buf(),
        exists: true,
        source_format: "mux_session_jsonl_tree",
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

fn message(id: &str, role: &str, sequence: i64, text: &str) -> Value {
    json!({
        "id": id,
        "workspaceId": "session-1",
        "role": role,
        "createdAt": "2026-07-28T12:00:00Z",
        "parts": [{"type": "text", "text": text}],
        "metadata": {"historySequence": sequence},
    })
}

fn write_metadata(session: &Path, project_path: &str) {
    fs::write(
        session.join("metadata.json"),
        serde_json::to_vec(&json!({
            "workspaceId": "session-1",
            "createdAt": "2026-07-28T11:59:00Z",
            "projectPath": project_path,
            "model": "mux-test-model",
        }))
        .unwrap(),
    )
    .unwrap();
}

fn write_chat(session: &Path, rows: &[Value]) {
    let mut bytes = Vec::new();
    for row in rows {
        serde_json::to_writer(&mut bytes, row).unwrap();
        bytes.push(b'\n');
    }
    fs::write(session.join("chat.jsonl"), bytes).unwrap();
}

fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session = root.join("session-1");
    fs::create_dir_all(&session).unwrap();
    write_metadata(&session, "/work/mux-project");
    (temp, root, session)
}

#[test]
fn shared_family_mux_cold_noop_and_grouped_full_body_hydration() {
    let (temp, root, session) = fixture();
    let long = format!("mux-head-{}-mux-tail", "x".repeat(4_096));
    write_chat(
        &session,
        &[
            message("message-0", "user", 0, &long),
            message("message-1", "assistant", 1, "mux response"),
        ],
    );
    let registry = registry(&root);
    let index_root = temp.path().join("index");

    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(cold.sources.len(), 1);
    assert_eq!(cold.sources[0].counts().complete_records, 2);
    assert_eq!(cold.sources[0].counts().indexed_documents, 2);

    reset_jsonl_family_work();
    let unchanged =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(jsonl_family_work().provider_projections, 0);
    assert_eq!(unchanged.sources, cold.sources);
    assert_eq!(unchanged.commit.generation_id, cold.commit.generation_id);
    assert_eq!(unchanged.commit.opstamp, cold.commit.opstamp);

    let source = cold.sources[0].observation().source();
    let index = VerifiedIndex::open(&index_root).unwrap();
    let mut events = index.source_event_page(source, None, 10).unwrap().items;
    events.sort_by_key(|event| event.event_sequence);
    assert_eq!(
        index
            .search_event_candidates("mux-tail", 10)
            .unwrap()
            .first()
            .map(|candidate| candidate.event.event_id),
        Some(events[0].event_id),
        "the full lexical body, including its tail, must be indexed"
    );
    assert!(events.iter().all(|event| {
        event.locator.revision_policy() == LocatorRevisionPolicy::StableRecordEvidence
            && event.locator.certified_source_revision_digest().is_some()
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
        vec![b"mux response".as_slice(), long.as_bytes()]
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
        "d7ab78494aecc92c8cd49b338c9b641e7ba3498cea6c9b2821803436d5ef38bb"
    );
}

#[test]
fn shared_family_mux_replacement_truncate_deletion_and_unavailable_root() {
    let (temp, root, session) = fixture();
    write_chat(
        &session,
        &[
            message("message-0", "user", 0, "before"),
            message("message-1", "assistant", 1, "second"),
        ],
    );
    let registry = registry(&root);
    let index_root = temp.path().join("index");
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let source = cold.sources[0].observation().source().clone();
    let cold_ids = {
        let index = VerifiedIndex::open(&index_root).unwrap();
        let mut events = index.source_event_page(&source, None, 10).unwrap().items;
        events.sort_by_key(|event| event.event_sequence);
        events
            .iter()
            .map(|event| event.event_id)
            .collect::<Vec<_>>()
    };

    let before = fs::read_to_string(session.join("chat.jsonl")).unwrap();
    let rewritten = before.replace("before", "change");
    assert_eq!(rewritten.len(), before.len());
    fs::write(session.join("chat.jsonl"), rewritten).unwrap();
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
    assert_eq!(events[0].event_id, cold_ids[0]);
    let changed = registry
        .resolver_registry()
        .hydrate_event(
            &EventHydrationRequest::new(events[0].event_id, events[0].locator.clone()).unwrap(),
        )
        .unwrap();
    assert_eq!(changed.provider_bytes, b"change");

    writeln!(
        fs::OpenOptions::new()
            .append(true)
            .open(session.join("chat.jsonl"))
            .unwrap(),
        "{}",
        message("message-2", "user", 2, "tiny append")
    )
    .unwrap();
    reset_jsonl_family_work();
    let grown = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(
        jsonl_family_work().provider_projections,
        3,
        "one Mux record append still reprojects the complete three-record source"
    );
    assert_eq!(grown.sources[0].counts().indexed_documents, 3);

    write_chat(&session, &[message("message-0", "user", 0, "truncated")]);
    reset_jsonl_family_work();
    let truncated =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(jsonl_family_work().provider_projections, 1);
    assert_eq!(truncated.sources[0].counts().indexed_documents, 1);

    fs::remove_dir_all(&session).unwrap();
    let deleted =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(deleted.sources.is_empty());
    assert_ne!(deleted.commit.generation_id, truncated.commit.generation_id);

    fs::remove_dir_all(&root).unwrap();
    assert!(
        refresh_source_backed_generation(&index_root, &registry, writer_options()).is_err(),
        "a missing authority root is unavailable, not a deletion certificate"
    );
    let index = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(index.generation_id(), deleted.commit.generation_id);
}

#[test]
fn shared_family_mux_compound_identity_and_exact_content_parity() {
    let (temp, root, session) = fixture();
    let chat_body = format!("chat-{}-tail", "c".repeat(2_048));
    let partial_body = format!("partial-{}-tail", "p".repeat(2_048));
    write_chat(&session, &[message("chat-0", "user", 0, &chat_body)]);
    fs::write(
        session.join("partial.json"),
        serde_json::to_vec(&message("partial-1", "assistant", 1, &partial_body)).unwrap(),
    )
    .unwrap();
    let registry = registry(&root);
    let index_root = temp.path().join("index");
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(cold.sources.len(), 1);
    assert_eq!(cold.sources[0].counts().indexed_documents, 2);

    let source = cold.sources[0].observation().source();
    let index = VerifiedIndex::open(&index_root).unwrap();
    let mut events = index.source_event_page(source, None, 10).unwrap().items;
    events.sort_by_key(|event| event.event_sequence);
    assert_eq!(
        events
            .iter()
            .map(|event| event.cwd.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("/work/mux-project"), Some("/work/mux-project")]
    );
    assert!(events[0]
        .source_path
        .as_deref()
        .is_some_and(|path| path.ends_with("/session-1/chat.jsonl")));
    assert!(events[1]
        .source_path
        .as_deref()
        .is_some_and(|path| path.ends_with("/session-1/partial.json")));
    assert_eq!(
        events[0].locator.revision_policy(),
        LocatorRevisionPolicy::StableRecordEvidence
    );
    assert_eq!(
        events[1].locator.revision_policy(),
        LocatorRevisionPolicy::ExactSourceRevision
    );

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
        vec![chat_body.as_bytes(), partial_body.as_bytes()]
    );

    write_metadata(&session, "/work/mux-changed");
    reset_jsonl_family_work();
    let metadata_replaced =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(
        jsonl_family_work().provider_projections,
        1,
        "compound metadata identity change must replace the primary exactly once"
    );
    assert_ne!(
        metadata_replaced.commit.generation_id,
        cold.commit.generation_id
    );
    let index = VerifiedIndex::open(&index_root).unwrap();
    let mut changed = index.source_event_page(source, None, 10).unwrap().items;
    changed.sort_by_key(|event| event.event_sequence);
    assert!(changed
        .iter()
        .all(|event| event.cwd.as_deref() == Some("/work/mux-changed")));
}
