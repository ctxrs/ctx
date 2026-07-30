use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use chrono::{TimeZone, Utc};
use ctx_history_core::{
    BatchHydrationRequest, CaptureProvider, ContentSourceResolver, EventHydrationRequest,
    HydrationFailureKind, NativeRecordCoordinate, SourceRecordLocator, TypedKey,
};
use ctx_history_index::{EventRecord, VerifiedIndex, WriterOptions};
use serde_json::{json, Value};

use super::*;
use crate::{
    provider::source_backed::{
        refresh_source_backed_generation, SourceBackedProviderRegistry, SourceBackedRouteSelection,
    },
    test_support_paths::tempdir,
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceStatus,
};

fn adapter_context(root: &Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "rovodev-source-backed-test".to_owned(),
        source_path: Some(root.to_path_buf()),
        source_root: Some(root.to_path_buf()),
        imported_at: Utc.timestamp_millis_opt(1_700_000_000_000).unwrap(),
    }
}

fn route_source(root: &Path) -> ProviderSource {
    ProviderSource {
        provider: CaptureProvider::RovoDev,
        path: root.to_path_buf(),
        exists: true,
        source_format: ROVODEV_SOURCE_FORMAT,
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
    }
}

fn writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

fn registry_with_counters(
    root: &Path,
) -> (
    SourceBackedProviderRegistry,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
) {
    let projection_scans = Arc::new(AtomicUsize::new(0));
    let hydration_scans = Arc::new(AtomicUsize::new(0));
    let adapter = RovoDevDocumentTreeAdapter::new(root.to_path_buf(), adapter_context(root))
        .with_projection_scans(Arc::clone(&projection_scans))
        .with_hydration_scans(Arc::clone(&hydration_scans));
    let mut registry = SourceBackedProviderRegistry::new();
    crate::provider::source_backed::family::document::register_replacement_document_tree_route(
        &mut registry,
        route_source(root),
        SourceBackedRouteSelection::Automatic,
        adapter,
    )
    .unwrap();
    (registry, projection_scans, hydration_scans)
}

fn write_session(
    root: &Path,
    directory_session_id: &str,
    provider_session_id: &str,
    parent_session_id: Option<&str>,
    messages: &[Value],
) -> PathBuf {
    let directory = root.join(directory_session_id);
    fs::create_dir_all(&directory).unwrap();
    let context = directory.join("session_context.json");
    fs::write(
        &context,
        serde_json::to_vec(&json!({
            "session_id": provider_session_id,
            "message_history": messages,
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        directory.join("metadata.json"),
        serde_json::to_vec(&json!({
            "session_id": provider_session_id,
            "parent_session_id": parent_session_id,
            "workspace_path": "/workspace/rovo",
            "git_branch": "feature/shared-document",
        }))
        .unwrap(),
    )
    .unwrap();
    context
}

fn source_events(index_root: &Path, source: &SourceKey) -> Vec<EventRecord> {
    let mut events = VerifiedIndex::open(index_root)
        .unwrap()
        .source_event_page(source, None, 16)
        .unwrap()
        .items;
    events.sort_by_key(|event| event.event_sequence);
    events
}

#[test]
fn shared_route_preserves_exact_projection_lineage_and_grouped_hydration() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".rovodev/sessions");
    let index_root = temp.path().join("index");
    write_session(
        &root,
        "root",
        "root-thread",
        None,
        &[json!({"id": "root-message", "role": "user", "content": "root"})],
    );
    let full_body = format!("full-prefix-{}-rovodev-tail", "x".repeat(3_000));
    let context_path = write_session(
        &root,
        "child",
        "child-thread",
        Some("root-thread"),
        &[
            json!({"id": "child-user", "role": "user", "content": full_body}),
            json!({"id": "child-assistant", "role": "assistant", "content": "exact response"}),
            json!({"id": "tool-success", "role": "tool_result", "status": "success", "content": "ignored"}),
            json!("malformed"),
        ],
    );
    let (registry, projection_scans, hydration_scans) = registry_with_counters(&root);

    let receipt =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(projection_scans.load(Ordering::Relaxed), 2);
    assert_eq!(receipt.sources.len(), 2);
    let child_source = rovodev_source_key("child-thread").unwrap();
    let child_certificate = receipt
        .sources
        .iter()
        .find(|source| {
            source
                .observation()
                .source()
                .exact_descriptor_eq(&child_source)
        })
        .unwrap();
    assert_eq!(child_certificate.parser_revision(), PARSER_REVISION);
    assert_eq!(child_certificate.counts().complete_records, 4);
    assert_eq!(child_certificate.counts().retained_records, 2);
    assert_eq!(child_certificate.counts().rejected_records, 1);
    assert_eq!(child_certificate.counts().ignored_records, 1);
    assert_eq!(child_certificate.counts().indexed_documents, 2);

    let events = source_events(&index_root, &child_source);
    assert_eq!(events.len(), 2);
    let root_source = rovodev_source_key("root-thread").unwrap();
    let root_session_id = rovodev_session_identity(&root_source, "root-thread").unwrap();
    let child_session_id = rovodev_session_identity(&child_source, "child-thread").unwrap();
    assert_eq!(events[0].session_id, child_session_id);
    assert_eq!(events[0].parent_session_id, Some(root_session_id));
    assert_eq!(events[0].root_session_id, root_session_id);
    assert_eq!(
        events[0].provider_session_id.as_deref(),
        Some("child-thread")
    );
    assert_eq!(events[0].branch.as_deref(), Some("feature/shared-document"));
    assert_eq!(events[0].source_path.as_deref(), context_path.to_str());
    assert_eq!(events[0].agent_type, AgentType::Subagent.as_str());
    assert!(!events[0].is_primary);
    assert_eq!(
        events[0].locator.certified_source_revision_digest(),
        Some(child_certificate.content_digest())
    );
    let NativeRecordCoordinate::TreeRecord {
        relative_file_key: TypedKey::Utf8(relative),
        record_coordinate: TypedKey::Composite(parts),
    } = events[0].locator.coordinate()
    else {
        panic!("Rovo Dev locator lost its typed tree coordinate");
    };
    assert_eq!(relative, RELATIVE_CONTEXT_FILE);
    assert_eq!(
        parts,
        &[
            TypedKey::Utf8(MESSAGE_OBJECT_KIND.to_owned()),
            TypedKey::U64(0),
            TypedKey::Utf8("child-user".to_owned()),
        ]
    );

    let requests = [1_usize, 0]
        .into_iter()
        .map(|index| {
            EventHydrationRequest::new(events[index].event_id, events[index].locator.clone())
                .unwrap()
        })
        .collect::<Vec<_>>();
    let batch = BatchHydrationRequest::new(requests.clone()).unwrap();
    let hydrated = registry.resolver_registry().hydrate_batch(&batch).unwrap();
    assert_eq!(hydration_scans.load(Ordering::Relaxed), 1);
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
    assert_eq!(hydrated.records()[0].provider_bytes, b"exact response");
    assert!(hydrated.records()[1]
        .provider_bytes
        .ends_with(b"rovodev-tail"));

    let missing_locator = SourceRecordLocator::new(
        child_source,
        NativeRecordCoordinate::TreeRecord {
            relative_file_key: TypedKey::utf8(RELATIVE_CONTEXT_FILE).unwrap(),
            record_coordinate: TypedKey::composite(vec![
                TypedKey::utf8(MESSAGE_OBJECT_KIND).unwrap(),
                TypedKey::U64(99),
                TypedKey::utf8("missing").unwrap(),
            ])
            .unwrap(),
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        events[0]
            .locator
            .certified_source_revision_digest()
            .copied(),
        *events[0].locator.record_digest(),
    )
    .unwrap();
    let partly_valid = BatchHydrationRequest::new(vec![
        requests[0].clone(),
        EventHydrationRequest::new(events[0].event_id, missing_locator).unwrap(),
    ])
    .unwrap();
    let error = registry
        .resolver_registry()
        .hydrate_batch(&partly_valid)
        .unwrap_err();
    assert_eq!(error.kind, HydrationFailureKind::MissingRecord);
    assert_eq!(hydration_scans.load(Ordering::Relaxed), 2);
}

#[test]
fn durable_replay_scans_one_changed_leaf_and_distinguishes_delete_from_unavailable() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".rovodev/sessions");
    let index_root = temp.path().join("index");
    let path_a = write_session(
        &root,
        "a",
        "session-a",
        None,
        &[json!({"id": "a-message", "role": "user", "content": "alpha-before"})],
    );
    write_session(
        &root,
        "b",
        "session-b",
        None,
        &[json!({"id": "b-message", "role": "user", "content": "bravo-stable"})],
    );
    let (registry, projection_scans, _) = registry_with_counters(&root);

    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(projection_scans.load(Ordering::Relaxed), 2);
    let unchanged =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(unchanged.commit.generation_id, cold.commit.generation_id);
    assert_eq!(projection_scans.load(Ordering::Relaxed), 2);

    let original = fs::metadata(&path_a).unwrap();
    write_session(
        &root,
        "a",
        "session-a",
        None,
        &[json!({"id": "a-message", "role": "user", "content": "alpha-after-"})],
    );
    assert_eq!(fs::metadata(&path_a).unwrap().len(), original.len());
    fs::OpenOptions::new()
        .write(true)
        .open(&path_a)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(original.modified().unwrap()))
        .unwrap();
    let changed =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_ne!(changed.commit.generation_id, cold.commit.generation_id);
    assert_eq!(projection_scans.load(Ordering::Relaxed), 3);

    fs::remove_dir_all(root.join("b")).unwrap();
    let deleted =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(deleted.sources.len(), 1);
    assert_eq!(deleted.removals.len(), 1);
    assert_eq!(projection_scans.load(Ordering::Relaxed), 3);

    write_session(
        &root,
        "b",
        "session-b",
        None,
        &[json!({"id": "b-message", "role": "user", "content": "bravo-stable"})],
    );
    let restored =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(projection_scans.load(Ordering::Relaxed), 4);
    let retained_generation = restored.commit.generation_id;
    fs::remove_dir_all(&root).unwrap();
    assert!(refresh_source_backed_generation(&index_root, &registry, writer_options()).is_err());
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().generation_id(),
        retained_generation
    );
}

#[test]
fn terminal_tree_fence_runs_once_and_rejects_a_race() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".rovodev/sessions");
    let index_root = temp.path().join("index");
    write_session(
        &root,
        "session",
        "session",
        None,
        &[json!({"id": "message", "role": "user", "content": "cold"})],
    );
    let (cold_registry, _, _) = registry_with_counters(&root);
    let cold =
        refresh_source_backed_generation(&index_root, &cold_registry, writer_options()).unwrap();
    write_session(
        &root,
        "session",
        "session",
        None,
        &[json!({"id": "message", "role": "user", "content": "before-fence"})],
    );

    let projection_scans = Arc::new(AtomicUsize::new(0));
    let fence_calls = Arc::new(AtomicUsize::new(0));
    let hook_root = root.clone();
    let hook_calls = Arc::clone(&fence_calls);
    let adapter = RovoDevDocumentTreeAdapter::new(root.clone(), adapter_context(&root))
        .with_projection_scans(Arc::clone(&projection_scans))
        .with_terminal_revalidation_hook(Arc::new(move || {
            hook_calls.fetch_add(1, Ordering::Relaxed);
            write_session(
                &hook_root,
                "session",
                "session",
                None,
                &[json!({"id": "message", "role": "user", "content": "during-fence"})],
            );
        }));
    let mut race_registry = SourceBackedProviderRegistry::new();
    crate::provider::source_backed::family::document::register_replacement_document_tree_route(
        &mut race_registry,
        route_source(&root),
        SourceBackedRouteSelection::Automatic,
        adapter,
    )
    .unwrap();

    assert!(
        refresh_source_backed_generation(&index_root, &race_registry, writer_options()).is_err()
    );
    assert_eq!(projection_scans.load(Ordering::Relaxed), 1);
    assert_eq!(fence_calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().generation_id(),
        cold.commit.generation_id
    );
}

#[test]
fn production_route_is_thin_and_below_the_loc_gate() {
    let adapter = include_str!("../source_backed.rs");
    let document = include_str!("document.rs");
    let registration = include_str!("../../../../source_backed/registration/families/document.rs");
    for (name, production) in [
        ("rovodev_adapter", adapter),
        ("rovodev_document", document),
        ("document_registration", registration),
    ] {
        assert!(
            production.lines().count() < 1_000,
            "{name} exceeded the 1,000-line production gate"
        );
    }
    let start = registration.find("fn register_rovodev_route").unwrap();
    let end = registration[start..]
        .find("fn register_continue_route")
        .map(|offset| start + offset)
        .unwrap();
    let rovodev_registration = &registration[start..end];
    let captured = ["captured_route", "_driver"].concat();
    assert!(!rovodev_registration.contains(&captured));
    for obsolete in [
        "RovoDevSourceBackedPage",
        "RovoDevSourceBackedScan",
        "next_page",
        "finish(",
        "CertifiedSourceInventory",
    ] {
        assert!(!adapter.contains(obsolete) && !document.contains(obsolete));
    }
    assert_eq!(adapter.matches("scan_rovodev_document(").count(), 1);
    assert!(!adapter.contains("Vec<LexicalDocument>"));
    assert!(!document.contains("Vec<LexicalDocument>"));
}
