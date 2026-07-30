use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use ctx_history_core::{
    BatchHydrationRequest, CaptureProvider, CertifiedSourceDeletion, CertifiedSourceInventory,
    ContentSourceResolver, EventHydrationRequest, HydrationFailureKind, SourceInventoryObservation,
    TypedKey,
};
use ctx_history_index::{GenerationWriter, IndexError, VerifiedIndex, WriterOptions};

use super::*;
use crate::{
    provider::source_backed::{
        refresh_source_backed_generation, SourceBackedCoordinatorError,
        SourceBackedProviderRegistry, SourceBackedRouteSelection,
    },
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceStatus,
};

const QWEN_LIFECYCLE_TRANSCRIPT: &str = concat!(
    "{\"uuid\":\"qwen-event\",\"sessionId\":\"qwen-life\",",
    "\"timestamp\":\"2026-07-25T12:00:01Z\",\"type\":\"user\",",
    "\"cwd\":\"/workspace/qwen\",\"message\":{\"role\":\"user\",",
    "\"content\":[{\"type\":\"text\",\"text\":\"lifecycle sentinel\"}]},",
    "\"model\":\"qwen3-coder\"}\n",
    "{\"uuid\":\"qwen-event-2\",\"sessionId\":\"qwen-life\",",
    "\"timestamp\":\"2026-07-25T12:00:02Z\",\"type\":\"assistant\",",
    "\"cwd\":\"/workspace/qwen\",\"message\":{\"role\":\"assistant\",",
    "\"content\":[{\"type\":\"text\",\"text\":\"second sentinel\"}]},",
    "\"model\":\"qwen3-coder\"}\n"
);

fn qwen_route_fixture() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    SourceBackedProviderRegistry,
) {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let provider_root = temp.path().join(".qwen/projects");
    let transcript = provider_root.join("workspace/chats/qwen-life.jsonl");
    fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    fs::write(&transcript, QWEN_LIFECYCLE_TRANSCRIPT).unwrap();
    let registry = qwen_registry(&provider_root);
    (temp, provider_root, transcript, registry)
}

fn qwen_registry(provider_root: &Path) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    registration::register(
        &mut registry,
        qwen_source(provider_root),
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    registry
}

fn qwen_registry_with_test_observer(
    provider_root: &Path,
    observer: registration::DirectJsonlRegistrationTestObserver,
) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    registration::register_with_test_observer(
        &mut registry,
        qwen_source(provider_root),
        SourceBackedRouteSelection::Automatic,
        observer,
    )
    .unwrap();
    registry
}

fn qwen_source(provider_root: &Path) -> ProviderSource {
    ProviderSource {
        provider: CaptureProvider::QwenCode,
        path: provider_root.to_path_buf(),
        exists: true,
        source_format: "qwen_code_chat_jsonl_tree",
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

#[test]
fn mixed_valid_and_malformed_records_publish_with_typed_rejection_evidence() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let provider_root = temp.path().join(".qwen/projects");
    let transcript = provider_root.join("workspace/chats/qwen-mixed.jsonl");
    fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    fs::write(
        &transcript,
        QWEN_LIFECYCLE_TRANSCRIPT.replacen(
            "\n",
            "\n{\"type\":\"message\",\"message\":{\"content\":[\n",
            1,
        ),
    )
    .unwrap();
    let adapter = super::super::qwen_code_source_backed_adapter();
    let inventory = adapter.discover(&provider_root).unwrap();
    let mut reader = adapter
        .open_leaf(
            &inventory.leaves()[0],
            chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
            None,
        )
        .unwrap();
    let mut documents = Vec::new();
    reader
        .visit_documents(&mut |document| {
            documents.push(document);
            Ok(())
        })
        .unwrap();
    let receipt = reader.finish().unwrap();

    assert_eq!(documents.len(), 2);
    assert_eq!(receipt.certificate().counts().complete_records, 3);
    assert_eq!(receipt.certificate().counts().retained_records, 2);
    assert_eq!(receipt.certificate().counts().rejected_records, 1);
    assert_eq!(receipt.rejections().len(), 1);
    assert_eq!(receipt.rejections()[0].raw_ordinal, 1);
    assert!(receipt.rejections()[0].reason.contains("malformed JSONL"));

    let base = receipt.certificate().clone();
    let replay_inventory = adapter.discover(&provider_root).unwrap();
    let mut replay = adapter
        .open_leaf(
            &replay_inventory.leaves()[0],
            chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
            Some(&base),
        )
        .unwrap();
    replay.visit_documents(&mut |_document| Ok(())).unwrap();
    let replay = replay.finish().unwrap();
    assert_eq!(replay.certificate(), &base);
    assert_eq!(replay.rejections(), receipt.rejections());
}

#[test]
fn cold_identity_rejection_isolated_from_valid_sibling_but_replacement_fails_closed() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let provider_root = temp.path().join(".qwen/projects");
    let chats = provider_root.join("workspace/chats");
    fs::create_dir_all(&chats).unwrap();
    let valid = chats.join("valid.jsonl");
    fs::write(&valid, QWEN_LIFECYCLE_TRANSCRIPT).unwrap();
    fs::write(chats.join("malformed.jsonl"), b"{\"sessionId\":\n").unwrap();
    let registry = qwen_registry(&provider_root);
    let index_root = temp.path().join("index");

    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(cold.sources.len(), 1);
    assert_eq!(cold.sources[0].counts().indexed_documents, 2);
    let unchanged =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(unchanged.sources, cold.sources);

    fs::write(&valid, b"{\"sessionId\":\n").unwrap();
    assert!(
        refresh_source_backed_generation(&index_root, &registry, writer_options()).is_err(),
        "a previously certified leaf becoming identity-less must fail the route"
    );
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().generation_id(),
        cold.commit.generation_id
    );
}

#[test]
fn active_source_family_contract_direct_jsonl_rejects_late_identity_admission() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let provider_root = temp.path().join(".qwen/projects");
    let chats = provider_root.join("workspace/chats");
    fs::create_dir_all(&chats).unwrap();
    fs::write(chats.join("valid.jsonl"), QWEN_LIFECYCLE_TRANSCRIPT).unwrap();
    let rejected = chats.join("rejected.jsonl");
    fs::write(&rejected, b"{\"sessionId\":\n").unwrap();
    let index_root = temp.path().join("index");
    let promote_rejected = Arc::new(AtomicBool::new(false));
    let promotion_armed = Arc::clone(&promote_rejected);
    let promoted_path = rejected.clone();
    let registry = qwen_registry_with_test_observer(
        &provider_root,
        Arc::new(move |event| {
            if event == registration::DirectJsonlRegistrationTestEvent::SourceRevalidated
                && promotion_armed.swap(false, Ordering::SeqCst)
            {
                fs::write(
                    &promoted_path,
                    QWEN_LIFECYCLE_TRANSCRIPT.replace("qwen-life", "qwen-promoted"),
                )
                .unwrap();
            }
        }),
    );
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    promote_rejected.store(true, Ordering::SeqCst);

    let error =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap_err();
    assert!(matches!(
        error,
        SourceBackedCoordinatorError::Index(IndexError::CompleteInventoryInvalidated { .. })
    ));
    assert!(!promote_rejected.load(Ordering::SeqCst));
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().generation_id(),
        cold.commit.generation_id
    );
}

#[test]
fn malformed_sibling_does_not_invalidate_exact_deletion_proof() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let provider_root = temp.path().join(".qwen/projects");
    let chats = provider_root.join("workspace/chats");
    fs::create_dir_all(&chats).unwrap();
    let deleted = chats.join("deleted.jsonl");
    fs::write(&deleted, QWEN_LIFECYCLE_TRANSCRIPT).unwrap();
    fs::write(
        chats.join("retained.jsonl"),
        QWEN_LIFECYCLE_TRANSCRIPT.replace("qwen-life", "qwen-retained"),
    )
    .unwrap();
    fs::write(chats.join("malformed.jsonl"), b"{\"sessionId\":\n").unwrap();
    let registry = qwen_registry(&provider_root);
    let index_root = temp.path().join("index");

    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(cold.sources.len(), 2);

    fs::remove_file(deleted).unwrap();
    let deletion =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(deletion.sources.len(), 1);
    let unchanged =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(unchanged.sources, deletion.sources);
}

#[test]
fn active_source_family_contract_direct_jsonl_recertifies_legacy_v1_removal() {
    let (temp, provider_root, transcript, registry) = qwen_route_fixture();
    let index_root = temp.path().join("index");
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let source = cold.sources[0].observation().source().clone();
    fs::remove_file(transcript).unwrap();

    let observation = SourceInventoryObservation::new(
        CaptureProvider::QwenCode.as_str(),
        DIRECT_JSONL_INVENTORY_AUTHORITY_NAMESPACE,
        TypedKey::bytes(provider_root.as_os_str().as_encoded_bytes().to_vec()).unwrap(),
        "direct-native-jsonl-inventory-sha256-v1",
        vec![0],
    )
    .unwrap();
    let legacy_inventory = CertifiedSourceInventory::certify(
        observation.clone(),
        observation,
        DIRECT_JSONL_LEGACY_DISCOVERY_REVISION,
        Vec::new(),
    )
    .unwrap();
    let legacy_deletion =
        CertifiedSourceDeletion::from_inventory(source.clone(), &legacy_inventory).unwrap();
    let mut writer = GenerationWriter::open(&index_root, writer_options()).unwrap();
    writer
        .certify_complete_inventory(legacy_inventory.clone())
        .unwrap();
    writer
        .delete_source(legacy_deletion, legacy_inventory)
        .unwrap();
    writer
        .commit_with_complete_inventory_revalidation(|_| true, |_| true)
        .unwrap();

    let migrated =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(migrated.sources.is_empty());
    assert_eq!(migrated.removals.len(), 1);
    assert_eq!(migrated.removals[0].deletion.source(), &source);
    assert_eq!(
        migrated.removals[0].deletion.discovery_revision(),
        DIRECT_JSONL_DISCOVERY_REVISION
    );
    assert_eq!(
        migrated.removals[0]
            .deletion
            .inventory()
            .authority_namespace(),
        DIRECT_JSONL_INVENTORY_AUTHORITY_NAMESPACE
    );
}

#[test]
fn exact_noop_performs_zero_provider_projections_and_preserves_generation() {
    let (temp, _provider_root, _transcript, registry) = qwen_route_fixture();
    let index_root = temp.path().join("index");
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();

    super::super::reader::reset_provider_projection_count();
    reset_inventory_traversals();
    let unchanged =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();

    assert_eq!(
        super::super::reader::provider_projection_count(),
        0,
        "exact no-op refresh must not project provider records"
    );
    assert_eq!(
        inventory_traversals(),
        3,
        "capture needs opening and closing traversals plus one terminal inventory traversal"
    );
    assert_eq!(unchanged.sources, cold.sources);
    assert_eq!(unchanged.commit.generation_id, cold.commit.generation_id);
    assert_eq!(unchanged.commit.opstamp, cold.commit.opstamp);
}

#[test]
fn metadata_only_same_content_churn_is_a_logical_noop() {
    let (temp, _provider_root, transcript, registry) = qwen_route_fixture();
    let index_root = temp.path().join("index");
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();

    let mut permissions = fs::metadata(&transcript).unwrap().permissions();
    permissions.set_mode(permissions.mode() ^ 0o100);
    fs::set_permissions(&transcript, permissions).unwrap();

    super::super::reader::reset_provider_projection_count();
    let unchanged =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();

    assert_eq!(
        super::super::reader::provider_projection_count(),
        0,
        "metadata-only logical no-op must not project provider records"
    );
    assert_eq!(unchanged.sources, cold.sources);
    assert_eq!(unchanged.commit.generation_id, cold.commit.generation_id);
    assert_eq!(unchanged.commit.opstamp, cold.commit.opstamp);
}

#[test]
fn active_source_family_contract_direct_jsonl_freezes_discovery_and_terminal_boundaries() {
    let (temp, provider_root, transcript, _registry) = qwen_route_fixture();
    let adapter = super::super::qwen_code_source_backed_adapter();
    let inventory = adapter.discover(&provider_root).unwrap();
    let leaf = inventory.leaves()[0].clone();
    OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap()
        .write_all(
            concat!(
                "{\"uuid\":\"qwen-event-3\",\"sessionId\":\"qwen-life\",",
                "\"timestamp\":\"2026-07-25T12:00:03Z\",\"type\":\"assistant\",",
                "\"cwd\":\"/workspace/qwen\",\"message\":{\"role\":\"assistant\",",
                "\"content\":[{\"type\":\"text\",\"text\":\"active append\"}]},",
                "\"model\":\"qwen3-coder\"}\n"
            )
            .as_bytes(),
        )
        .unwrap();

    let mut reader = adapter
        .open_leaf(&leaf, chrono::DateTime::<chrono::Utc>::UNIX_EPOCH, None)
        .unwrap();
    let mut documents = Vec::new();
    reader
        .visit_documents(&mut |document| {
            documents.push(document);
            Ok(())
        })
        .unwrap();
    let receipt = reader.finish().unwrap();
    assert_eq!(documents.len(), 3);

    let evidence = DirectJsonlTerminalEvidenceSet::default();
    evidence.record(receipt.terminal_evidence()).unwrap();
    OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap()
        .write_all(
            concat!(
                "{\"uuid\":\"qwen-event-4\",\"sessionId\":\"qwen-life\",",
                "\"timestamp\":\"2026-07-25T12:00:04Z\",\"type\":\"assistant\",",
                "\"cwd\":\"/workspace/qwen\",\"message\":{\"role\":\"assistant\",",
                "\"content\":[{\"type\":\"text\",\"text\":\"next refresh\"}]},",
                "\"model\":\"qwen3-coder\"}\n"
            )
            .as_bytes(),
        )
        .unwrap();
    assert!(adapter
        .revalidate_certificate(&evidence, receipt.certificate())
        .unwrap());

    let current = adapter.discover(&provider_root).unwrap();
    assert_eq!(
        inventory.observation, current.observation,
        "append-log inventory identity must describe membership, not mutable length"
    );

    let route_events = Arc::new(Mutex::new(Vec::new()));
    let observed_route_events = Arc::clone(&route_events);
    let registry = qwen_registry_with_test_observer(
        &provider_root,
        Arc::new(move |event| observed_route_events.lock().unwrap().push(event)),
    );
    let index_root = temp.path().join("append-route-index");
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    route_events.lock().unwrap().clear();

    OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap()
        .write_all(
            concat!(
                "{\"uuid\":\"qwen-event-5\",\"sessionId\":\"qwen-life\",",
                "\"timestamp\":\"2026-07-25T12:00:05Z\",\"type\":\"assistant\",",
                "\"cwd\":\"/workspace/qwen\",\"message\":{\"role\":\"assistant\",",
                "\"content\":[{\"type\":\"text\",\"text\":\"optimizedrouteonlymarker\"}]},",
                "\"model\":\"qwen3-coder\"}\n"
            )
            .as_bytes(),
        )
        .unwrap();
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();

    assert_eq!(
        *route_events.lock().unwrap(),
        vec![
            registration::DirectJsonlRegistrationTestEvent::BeginSourceAppend,
            registration::DirectJsonlRegistrationTestEvent::SourceRevalidated,
            registration::DirectJsonlRegistrationTestEvent::CompleteInventoryAccepted,
        ],
        "the direct append refresh must stage and certify against its retained base"
    );
    assert_eq!(
        VerifiedIndex::open(&index_root)
            .unwrap()
            .search_event_candidates("optimizedrouteonlymarker", 8)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn final_inventory_rejects_rewrite_after_per_source_terminal_revalidation() {
    let (temp, provider_root, transcript, _registry) = qwen_route_fixture();
    let index_root = temp.path().join("terminal-race-index");
    let route_events = Arc::new(Mutex::new(Vec::new()));
    let observed_route_events = Arc::clone(&route_events);
    let rewrite_after_source_revalidation = Arc::new(AtomicBool::new(false));
    let rewrite_is_armed = Arc::clone(&rewrite_after_source_revalidation);
    let transcript_to_rewrite = transcript.clone();
    let rewritten = QWEN_LIFECYCLE_TRANSCRIPT.replace("second sentinel", "rewritten value");
    assert_eq!(rewritten.len(), QWEN_LIFECYCLE_TRANSCRIPT.len());
    let registry = qwen_registry_with_test_observer(
        &provider_root,
        Arc::new(move |event| {
            observed_route_events.lock().unwrap().push(event);
            if event == registration::DirectJsonlRegistrationTestEvent::SourceRevalidated
                && rewrite_is_armed.swap(false, Ordering::SeqCst)
            {
                fs::write(&transcript_to_rewrite, &rewritten).unwrap();
            }
        }),
    );

    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let before = VerifiedIndex::open(&index_root).unwrap();
    let before_generation = before.generation_id().to_owned();
    let before_documents = before.document_count();
    drop(before);
    route_events.lock().unwrap().clear();
    rewrite_after_source_revalidation.store(true, Ordering::SeqCst);

    let error =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap_err();
    assert!(matches!(
        error,
        SourceBackedCoordinatorError::Index(IndexError::CompleteInventoryInvalidated { .. })
    ));
    assert_eq!(
        *route_events.lock().unwrap(),
        vec![
            registration::DirectJsonlRegistrationTestEvent::BeginSourceAppend,
            registration::DirectJsonlRegistrationTestEvent::SourceRevalidated,
            registration::DirectJsonlRegistrationTestEvent::CompleteInventoryRejected,
        ],
        "the rewrite must occur after the source callback and fail the later evidence-aware inventory callback"
    );
    assert!(!rewrite_after_source_revalidation.load(Ordering::SeqCst));

    let after = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(after.generation_id(), before_generation);
    assert_eq!(after.document_count(), before_documents);
}

#[test]
fn grouped_checkpoint_hydration_binds_and_opens_once_without_header_projection() {
    let (temp, provider_root, _transcript, registry) = qwen_route_fixture();
    let index_root = temp.path().join("index");
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let certificate = &cold.sources[0];
    let source = certificate.observation().source();
    let index = VerifiedIndex::open(&index_root).unwrap();
    let mut events = index.source_event_page(source, None, 10).unwrap().items;
    events.sort_by_key(|event| event.event_sequence);
    let requests = events
        .iter()
        .rev()
        .map(|event| EventHydrationRequest::new(event.event_id, event.locator.clone()).unwrap())
        .collect::<Vec<_>>();

    let adapter = super::super::qwen_code_source_backed_adapter();
    reset_hydration_work();
    let mut catalog = adapter.open_hydration_catalog(&provider_root).unwrap();
    super::super::reader::reset_provider_projection_count();
    let hydrated = catalog.hydrate_group(certificate, &requests).unwrap();

    assert_eq!(
        hydrated
            .iter()
            .map(|record| record.event_id)
            .collect::<Vec<_>>(),
        requests
            .iter()
            .map(EventHydrationRequest::event_id)
            .collect::<Vec<_>>()
    );
    assert_eq!(hydrated[0].provider_bytes, b"second sentinel");
    assert_eq!(hydrated[1].provider_bytes, b"lifecycle sentinel");
    assert_eq!(
        super::super::reader::provider_projection_count(),
        2,
        "grouped hydration must verify the provider-native session owner before and after reads"
    );
    assert_eq!(
        hydration_work(),
        DirectJsonlHydrationWork {
            inventory_scans: 1,
            source_binds: 1,
            leaf_opens: 1,
        }
    );
}

#[test]
fn route_batch_discovers_and_binds_once_instead_of_per_event() {
    let (temp, _provider_root, _transcript, registry) = qwen_route_fixture();
    let index_root = temp.path().join("index");
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let source = cold.sources[0].observation().source();
    let index = VerifiedIndex::open(&index_root).unwrap();
    let mut events = index.source_event_page(source, None, 10).unwrap().items;
    events.sort_by_key(|event| event.event_sequence);
    let requests = events
        .iter()
        .rev()
        .map(|event| EventHydrationRequest::new(event.event_id, event.locator.clone()).unwrap())
        .collect::<Vec<_>>();
    let batch = BatchHydrationRequest::new(requests.clone()).unwrap();

    reset_hydration_work();
    super::super::reader::reset_provider_projection_count();
    let hydrated = registry
        .resolver_registry()
        .hydrate_batch(&batch)
        .unwrap()
        .into_records();

    assert_eq!(
        hydrated
            .iter()
            .map(|record| record.event_id)
            .collect::<Vec<_>>(),
        requests
            .iter()
            .map(EventHydrationRequest::event_id)
            .collect::<Vec<_>>()
    );
    assert_eq!(hydrated[0].provider_bytes, b"second sentinel");
    assert_eq!(hydrated[1].provider_bytes, b"lifecycle sentinel");
    assert_eq!(
        hydration_work(),
        DirectJsonlHydrationWork {
            inventory_scans: 1,
            source_binds: 1,
            leaf_opens: 1,
        }
    );
    assert_eq!(
        super::super::reader::provider_projection_count(),
        2,
        "locator-bound grouped hydration must verify the provider identity before and after reads"
    );
}

#[test]
fn repeated_single_hydration_reuses_one_resident_inventory_and_source_binding() {
    let (temp, _provider_root, transcript, registry) = qwen_route_fixture();
    let index_root = temp.path().join("index");
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let source = cold.sources[0].observation().source();
    let index = VerifiedIndex::open(&index_root).unwrap();
    let event = index
        .source_event_page(source, None, 1)
        .unwrap()
        .items
        .pop()
        .unwrap();
    let request = EventHydrationRequest::new(event.event_id, event.locator).unwrap();
    let resolver = registry.resolver_registry();

    reset_hydration_work();
    super::super::reader::reset_provider_projection_count();
    let first = resolver.hydrate_event(&request).unwrap();
    OpenOptions::new()
        .append(true)
        .open(transcript)
        .unwrap()
        .write_all(
            concat!(
                "{\"uuid\":\"qwen-hydration-append\",\"sessionId\":\"qwen-life\",",
                "\"timestamp\":\"2026-07-25T12:00:05Z\",\"type\":\"assistant\",",
                "\"cwd\":\"/workspace/qwen\",\"message\":{\"role\":\"assistant\",",
                "\"content\":[{\"type\":\"text\",\"text\":\"deferred hydration append\"}]},",
                "\"model\":\"qwen3-coder\"}\n"
            )
            .as_bytes(),
        )
        .unwrap();
    let second = resolver.hydrate_event(&request).unwrap();

    assert_eq!(first, second);
    assert_eq!(first.provider_bytes, b"lifecycle sentinel");
    assert_eq!(
        hydration_work(),
        DirectJsonlHydrationWork {
            inventory_scans: 1,
            source_binds: 1,
            leaf_opens: 2,
        }
    );
    assert_eq!(
        super::super::reader::provider_projection_count(),
        4,
        "each single hydration must verify the provider-native session owner before and after reads"
    );
}

#[test]
fn active_source_family_contract_direct_jsonl_hydration_rejects_identity_rewrite_with_append() {
    let (temp, _provider_root, transcript, registry) = qwen_route_fixture();
    let index_root = temp.path().join("index");
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let source = cold.sources[0].observation().source();
    let index = VerifiedIndex::open(&index_root).unwrap();
    let event = index
        .source_event_page(source, None, 10)
        .unwrap()
        .items
        .into_iter()
        .find(|event| event.event_sequence == 1)
        .unwrap();
    let request = EventHydrationRequest::new(event.event_id, event.locator).unwrap();
    let rewrite_path = transcript.clone();
    crate::provider::source_backed::family::jsonl::set_after_jsonl_hydration_observation_hook(
        move || {
            let mut bytes = fs::read(&rewrite_path).unwrap();
            let identity_offset = bytes
                .windows(b"qwen-life".len())
                .position(|window| window == b"qwen-life")
                .unwrap();
            bytes[identity_offset..identity_offset + b"qwen-life".len()]
                .copy_from_slice(b"qwen-evil");
            bytes.extend_from_slice(
                b"{\"uuid\":\"late\",\"sessionId\":\"qwen-evil\",\"type\":\"assistant\"}\n",
            );
            fs::write(&rewrite_path, bytes).unwrap();
        },
    );

    let error = registry
        .resolver_registry()
        .hydrate_event(&request)
        .unwrap_err();
    assert_eq!(error.kind, HydrationFailureKind::StaleRecordEvidence);
}

#[test]
fn terminal_inventory_and_deletion_proofs_each_traverse_once() {
    let (temp, provider_root, transcript, registry) = qwen_route_fixture();
    let index_root = temp.path().join("index");
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let adapter = super::super::qwen_code_source_backed_adapter();
    let sources = cold
        .sources
        .iter()
        .map(|certificate| certificate.observation().source().clone())
        .collect::<Vec<_>>();

    let opening = adapter.discover(&provider_root).unwrap();
    let closing = adapter.discover(&provider_root).unwrap();
    let inventory = opening.certify_against(&closing, sources.clone()).unwrap();
    reset_inventory_traversals();
    assert!(adapter
        .revalidate_inventory(&provider_root, &inventory)
        .unwrap());
    assert_eq!(inventory_traversals(), 1);

    fs::remove_file(transcript).unwrap();
    let opening = adapter.discover(&provider_root).unwrap();
    let closing = adapter.discover(&provider_root).unwrap();
    let empty_inventory = opening.certify_against(&closing, Vec::new()).unwrap();
    let deletion = ctx_history_core::CertifiedSourceDeletion::from_inventory(
        sources[0].clone(),
        &empty_inventory,
    )
    .unwrap();
    reset_inventory_traversals();
    assert!(adapter
        .revalidate_deletion(&provider_root, &deletion)
        .unwrap());
    assert_eq!(inventory_traversals(), 1);
}

#[test]
fn resident_single_hydration_preserves_stale_and_deleted_failure_kinds() {
    let (temp, provider_root, transcript, registry) = qwen_route_fixture();
    let index_root = temp.path().join("index");
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let source = cold.sources[0].observation().source();
    let index = VerifiedIndex::open(&index_root).unwrap();
    let event = index
        .source_event_page(source, None, 1)
        .unwrap()
        .items
        .pop()
        .unwrap();
    let request = EventHydrationRequest::new(event.event_id, event.locator).unwrap();
    let resolver = registry.resolver_registry();
    reset_hydration_work();
    resolver.hydrate_event(&request).unwrap();

    fs::write(&transcript, b"{\"sessionId\":\"rewritten\"}\n").unwrap();
    let stale = resolver.hydrate_event(&request).unwrap_err();
    assert_eq!(stale.kind, HydrationFailureKind::StaleRecordEvidence);

    fs::write(&transcript, QWEN_LIFECYCLE_TRANSCRIPT).unwrap();
    assert_eq!(
        resolver.hydrate_event(&request).unwrap().provider_bytes,
        b"lifecycle sentinel"
    );
    assert_eq!(
        hydration_work(),
        DirectJsonlHydrationWork {
            inventory_scans: 2,
            source_binds: 2,
            leaf_opens: 3,
        },
        "a stale resident catalog must be discarded so refresh retry can recatalog"
    );

    fs::remove_file(transcript).unwrap();
    let deleted = qwen_registry(&provider_root)
        .resolver_registry()
        .hydrate_event(&request)
        .unwrap_err();
    assert_eq!(deleted.kind, HydrationFailureKind::ConfirmedDeleted);
}

#[cfg(target_os = "linux")]
#[test]
fn two_thousand_leaf_inventory_and_catalog_retain_constant_provider_fds() {
    fn retained_provider_fds(root: &Path) -> usize {
        fs::read_dir("/proc/self/fd")
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|entry| fs::read_link(entry.path()).ok())
            .filter(|target| target.starts_with(root))
            .count()
    }

    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("windsurf");
    fs::create_dir_all(&root).unwrap();
    for ordinal in 0..2_000 {
        fs::write(
            root.join(format!("session-{ordinal:04}.jsonl")),
            b"{\"type\":\"user\"}\n",
        )
        .unwrap();
    }
    let adapter = super::super::windsurf_source_backed_adapter();

    let inventory = adapter.discover(&root).unwrap();
    assert_eq!(inventory.leaves().len(), 2_000);
    assert!(
        retained_provider_fds(&root) <= 4,
        "inventory retained one descriptor per discovered leaf"
    );
    drop(inventory);

    let catalog = adapter.open_hydration_catalog(&root).unwrap();
    assert!(
        retained_provider_fds(&root) <= 4,
        "resident hydration catalog retained one descriptor per discovered leaf"
    );
    drop(catalog);
    assert_eq!(retained_provider_fds(&root), 0);
}
