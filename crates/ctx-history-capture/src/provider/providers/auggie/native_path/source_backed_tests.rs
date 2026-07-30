use std::{
    fs,
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    BatchHydrationRequest, CaptureProvider, ContentSourceResolver, EventHydrationRequest,
    HydrationFailureKind,
};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use serde_json::json;

use super::*;
use crate::{
    provider::source_backed::{
        refresh_source_backed_generation, SourceBackedProviderRegistry, SourceBackedRouteSelection,
    },
    test_support_paths::tempdir,
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceStatus,
};

fn write_session(path: &Path, request_text: &str, response_text: &str) {
    write_history(
        path,
        "auggie-source-session",
        &[("request-stable-id", request_text, response_text)],
    );
}

fn write_history(path: &Path, session_id: &str, exchanges: &[(&str, &str, &str)]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let chat_history = exchanges
        .iter()
        .enumerate()
        .map(|(index, (request_id, request_text, response_text))| {
            json!({
                "exchange": {
                    "request_id": request_id,
                    "request_message": request_text,
                    "response_text": response_text,
                },
                "finishedAt": format!("2026-07-28T11:{:02}:00Z", index + 1),
            })
        })
        .collect::<Vec<_>>();
    fs::write(
        path,
        serde_json::to_vec(&json!({
            "sessionId": session_id,
            "created": "2026-07-28T11:00:00Z",
            "workspaceRoot": "/workspace/auggie",
            "chatHistory": chat_history,
        }))
        .unwrap(),
    )
    .unwrap();
}

fn route_source(path: &Path) -> ProviderSource {
    ProviderSource {
        provider: CaptureProvider::Auggie,
        path: path.to_path_buf(),
        exists: true,
        source_format: AUGGIE_SESSION_JSON_SOURCE_FORMAT,
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
    }
}

fn registry(path: &Path) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    registration::register(
        &mut registry,
        route_source(path),
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    registry
}

fn registry_with_parse_count(path: &Path) -> (SourceBackedProviderRegistry, Arc<AtomicUsize>) {
    let source = route_source(path);
    let context = ProviderAdapterContext {
        machine_id: "source-backed-auggie-test".to_owned(),
        source_path: Some(source.path.clone()),
        source_root: Some(source.path.clone()),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let parse_count = Arc::new(AtomicUsize::new(0));
    let adapter = AuggieDocumentTreeAdapter::new(
        AuggieSourceBackedRoot::explicit(source.path.clone()),
        context,
    )
    .with_parse_count(Arc::clone(&parse_count));
    let mut registry = SourceBackedProviderRegistry::new();
    register_replacement_document_tree_route(
        &mut registry,
        source,
        SourceBackedRouteSelection::Automatic,
        adapter,
    )
    .unwrap();
    (registry, parse_count)
}

fn writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

#[test]
fn cold_route_is_an_exact_semantic_oracle_with_full_bodies_and_locators() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("home/.augment/sessions");
    let path = sessions.join("session.json");
    let request_text = format!("full-prefix-{}-auggie-tail", "x".repeat(3_000));
    write_session(&path, &request_text, "bounded response");
    let registry = registry(&sessions);
    let index_root = temp.path().join("index");

    let receipt =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(receipt.sources.len(), 1);
    let certificate = &receipt.sources[0];
    assert_eq!(certificate.parser_revision(), AUGGIE_PARSER_REVISION);
    assert_eq!(certificate.counts().complete_records, 2);
    assert_eq!(certificate.counts().retained_records, 2);
    assert_eq!(certificate.counts().indexed_documents, 2);
    assert_eq!(
        certificate.counts().certified_bytes,
        fs::metadata(&path).unwrap().len()
    );
    assert_eq!(
        certificate.observation().revision_kind(),
        AUGGIE_SOURCE_REVISION_KIND
    );
    assert!(certificate.frontier().is_some());

    let source = auggie_source_key("auggie-source-session").unwrap();
    assert!(certificate
        .observation()
        .source()
        .exact_descriptor_eq(&source));
    let expected_session = auggie_session_id(&source, "auggie-source-session").unwrap();
    let mut events = VerifiedIndex::open(&index_root)
        .unwrap()
        .source_event_page(&source, None, 8)
        .unwrap()
        .items;
    events.sort_by_key(|event| event.event_sequence);
    assert_eq!(events.len(), 2);
    assert!(events
        .iter()
        .all(|event| event.session_id == expected_session));
    assert!(events
        .iter()
        .all(|event| event.root_session_id == expected_session));
    assert!(events.iter().all(|event| event.parent_session_id.is_none()));
    assert!(events.iter().all(|event| {
        event.provider_session_id.as_deref() == Some("auggie-source-session")
            && event.agent_type == "primary"
            && event.is_primary
            && event.source_path.as_deref() == path.to_str()
            && event.workspace.as_deref() == Some("/workspace/auggie")
            && event.cwd.as_deref() == Some("/workspace/auggie")
            && event.event_type == "message"
            && event.touched_files.is_empty()
    }));
    assert_eq!(events[0].event_sequence, 0);
    assert_eq!(events[0].role.as_deref(), Some("user"));
    assert_eq!(events[1].event_sequence, 1);
    assert_eq!(events[1].role.as_deref(), Some("assistant"));
    assert_eq!(
        events[1].occurred_at_unix_ms,
        events[0].occurred_at_unix_ms.map(|time| time + 1)
    );
    for (event, message_kind) in events.iter().zip(["request", "response"]) {
        assert_eq!(
            event.locator.revision_policy(),
            LocatorRevisionPolicy::ExactSourceRevision
        );
        assert!(event.locator.source().exact_descriptor_eq(&source));
        assert_eq!(
            event.locator.certified_source_revision_digest(),
            Some(certificate.content_digest())
        );
        assert_eq!(event.locator.record_digest(), certificate.content_digest());
        let NativeRecordCoordinate::Document {
            object_key: TypedKey::Composite(parts),
            json_pointer: Some(pointer),
        } = event.locator.coordinate()
        else {
            panic!("Auggie oracle locator lost its typed document coordinate");
        };
        assert_eq!(
            parts.as_slice(),
            &[
                TypedKey::utf8(format!("request-stable-id:{message_kind}")).unwrap(),
                TypedKey::U64(0),
                TypedKey::utf8(message_kind).unwrap(),
            ]
        );
        assert_eq!(pointer, "/chatHistory/0/exchange");
    }

    let resolver = registry.resolver_registry();
    let request =
        EventHydrationRequest::new(events[0].event_id, events[0].locator.clone()).unwrap();
    let response =
        EventHydrationRequest::new(events[1].event_id, events[1].locator.clone()).unwrap();
    assert_eq!(
        resolver.hydrate_event(&request).unwrap().provider_bytes,
        request_text.as_bytes()
    );
    assert_eq!(
        resolver.hydrate_event(&response).unwrap().provider_bytes,
        b"bounded response"
    );
}

#[test]
fn grouped_hydration_opens_and_parses_once_preserves_order_and_fails_atomically() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let path = sessions.join("session.json");
    write_history(
        &path,
        "auggie-source-session",
        &[
            ("request-1", "first request", "first response"),
            ("request-2", "second request", "second response"),
        ],
    );
    write_history(
        &sessions.join("00-other.json"),
        "other-session",
        &[("other-request", "other request", "other response")],
    );
    let registry = registry(&sessions);
    let index_root = temp.path().join("index");
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let source = auggie_source_key("auggie-source-session").unwrap();
    let mut events = VerifiedIndex::open(&index_root)
        .unwrap()
        .source_event_page(&source, None, 8)
        .unwrap()
        .items;
    events.sort_by_key(|event| event.event_sequence);
    let requests = [3_usize, 0, 2]
        .into_iter()
        .map(|index| {
            EventHydrationRequest::new(events[index].event_id, events[index].locator.clone())
                .unwrap()
        })
        .collect::<Vec<_>>();
    let batch = BatchHydrationRequest::new(requests.clone()).unwrap();
    let open_tracker = Arc::new(AuggieLeafOpenTracker::default());
    let hydration_root =
        AuggieSourceBackedRoot::explicit(&sessions).with_open_tracker(Arc::clone(&open_tracker));
    let mut parse_count = 0;
    let hydrated =
        hydrate_auggie_group_with_observer(&hydration_root, &batch, || parse_count += 1).unwrap();
    assert_eq!(parse_count, 2);
    assert_eq!(open_tracker.total(), 6);
    assert_eq!(open_tracker.peak(), 1);
    assert_eq!(open_tracker.active(), 0);
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
    assert_eq!(
        hydrated
            .records()
            .iter()
            .map(|record| record.provider_bytes.as_slice())
            .collect::<Vec<_>>(),
        vec![
            b"second response".as_slice(),
            b"first request".as_slice(),
            b"second request".as_slice(),
        ]
    );

    let missing_locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::Document {
            object_key: TypedKey::composite(vec![
                TypedKey::utf8("missing:request").unwrap(),
                TypedKey::U64(99),
                TypedKey::utf8("request").unwrap(),
            ])
            .unwrap(),
            json_pointer: Some("/chatHistory/99/exchange".to_owned()),
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        events[1]
            .locator
            .certified_source_revision_digest()
            .copied(),
        *events[1].locator.record_digest(),
    )
    .unwrap();
    let missing = EventHydrationRequest::new(events[1].event_id, missing_locator).unwrap();
    let partly_valid = BatchHydrationRequest::new(vec![requests[0].clone(), missing]).unwrap();
    parse_count = 0;
    let error =
        hydrate_auggie_group_with_observer(&hydration_root, &partly_valid, || parse_count += 1)
            .unwrap_err();
    assert_eq!(error.kind, HydrationFailureKind::MissingRecord);
    assert_eq!(parse_count, 2);
    assert_eq!(open_tracker.total(), 10);
    assert_eq!(open_tracker.active(), 0);

    write_history(
        &path,
        "auggie-source-session",
        &[("request-1", "replacement", "replacement response")],
    );
    parse_count = 0;
    let error = hydrate_auggie_group_with_observer(&hydration_root, &batch, || parse_count += 1)
        .unwrap_err();
    assert_eq!(error.kind, HydrationFailureKind::StaleRecordEvidence);
    assert_eq!(parse_count, 2);
    assert_eq!(open_tracker.total(), 14);
    assert_eq!(open_tracker.active(), 0);
}

#[test]
fn route_replays_noop_replaces_changes_certifies_delete_and_retains_on_unavailable() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let path = sessions.join("session.json");
    let index_root = temp.path().join("index");
    write_session(&path, "before replacement", "stable response");
    let (registry, parse_count) = registry_with_parse_count(&sessions);

    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(parse_count.load(Ordering::Relaxed), 1);
    let unchanged =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(unchanged.commit.generation_id, cold.commit.generation_id);
    assert_eq!(unchanged.sources, cold.sources);
    assert_eq!(parse_count.load(Ordering::Relaxed), 1);

    let old_request = VerifiedIndex::open(&index_root)
        .unwrap()
        .source_event_page(cold.sources[0].observation().source(), None, 8)
        .unwrap()
        .items
        .into_iter()
        .find(|event| event.event_sequence == 0)
        .unwrap();
    write_session(&path, "after replacement", "stable response");
    let changed =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(parse_count.load(Ordering::Relaxed), 2);
    assert_ne!(
        changed.sources[0].content_digest(),
        cold.sources[0].content_digest()
    );
    let changed_request = VerifiedIndex::open(&index_root)
        .unwrap()
        .source_event_page(changed.sources[0].observation().source(), None, 8)
        .unwrap()
        .items
        .into_iter()
        .find(|event| event.event_sequence == 0)
        .unwrap();
    let changed_hydration =
        EventHydrationRequest::new(changed_request.event_id, changed_request.locator).unwrap();
    assert_eq!(
        registry
            .resolver_registry()
            .hydrate_event(&changed_hydration)
            .unwrap()
            .provider_bytes,
        b"after replacement"
    );
    assert!(matches!(
        hydrate_auggie_source_backed(&path, &old_request.locator),
        Err(AuggieSourceBackedError::SourceRevisionChanged)
            | Err(AuggieSourceBackedError::LocatorDigestMismatch)
    ));

    fs::remove_file(&path).unwrap();
    let deleted =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(deleted.sources.is_empty());
    assert_eq!(deleted.removals.len(), 1);
    assert_eq!(parse_count.load(Ordering::Relaxed), 2);

    write_session(&path, "restored", "restored response");
    let restored =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(parse_count.load(Ordering::Relaxed), 3);
    let retained_generation = restored.commit.generation_id;
    fs::remove_file(&path).unwrap();
    fs::remove_dir(&sessions).unwrap();
    assert!(refresh_source_backed_generation(&index_root, &registry, writer_options()).is_err());
    assert_eq!(parse_count.load(Ordering::Relaxed), 3);
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().generation_id(),
        retained_generation
    );
}

#[test]
fn discovery_retains_selection_and_rejects_replacement_and_symlink_attacks() {
    let temp = tempdir().unwrap();
    let cache_root = temp.path().join("one-shot-augment-cache");
    let cache_sessions = cache_root.join("sessions");
    write_session(
        &cache_sessions.join("nested/ignored.json"),
        "nested request",
        "nested response",
    );
    write_session(
        &cache_sessions.join("explicit.json"),
        "explicit request",
        "explicit response",
    );
    let inventory =
        discover_auggie_source_backed(&AuggieSourceBackedRoot::explicit(&cache_root)).unwrap();
    assert!(inventory.is_complete());
    assert_eq!(
        inventory.paths(),
        vec![cache_sessions.join("explicit.json")]
    );

    let tree = inventory.into_complete_tree().unwrap();
    let moved = temp.path().join("moved-sessions");
    fs::rename(&cache_sessions, &moved).unwrap();
    fs::create_dir(&cache_sessions).unwrap();
    write_session(
        &cache_sessions.join("explicit.json"),
        "replacement request",
        "replacement response",
    );
    assert!(revalidate_auggie_tree(&tree).is_err());

    let missing = discover_auggie_source_backed(&AuggieSourceBackedRoot::explicit(
        temp.path().join("missing"),
    ))
    .unwrap();
    assert_eq!(
        missing.status,
        AuggieSourceBackedInventoryStatus::Unavailable
    );

    let sessions_file_root = temp.path().join("sessions-is-a-file");
    write_session(
        &sessions_file_root.join("must-not-be-selected.json"),
        "wrong-tree request",
        "wrong-tree response",
    );
    fs::write(sessions_file_root.join("sessions"), b"not a directory").unwrap();
    assert!(
        discover_auggie_source_backed(&AuggieSourceBackedRoot::explicit(&sessions_file_root))
            .is_err()
    );

    let explicit_non_json = temp.path().join("session.txt");
    fs::write(&explicit_non_json, b"{}").unwrap();
    assert!(
        discover_auggie_source_backed(&AuggieSourceBackedRoot::explicit(&explicit_non_json))
            .is_err()
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let outside = tempdir().unwrap();
        write_session(
            &outside.path().join("outside.json"),
            "outside request",
            "outside response",
        );
        let attack_root = temp.path().join("attack");
        fs::create_dir(&attack_root).unwrap();
        symlink(outside.path(), attack_root.join("sessions")).unwrap();
        assert!(
            discover_auggie_source_backed(&AuggieSourceBackedRoot::explicit(&attack_root)).is_err()
        );

        let hardlink_root = temp.path().join("hardlinks");
        let original = hardlink_root.join("original.json");
        let alias = hardlink_root.join("alias.json");
        write_session(&original, "hardlink request", "hardlink response");
        fs::hard_link(&original, &alias).unwrap();
        let aliased =
            discover_auggie_source_backed(&AuggieSourceBackedRoot::explicit(&hardlink_root))
                .unwrap();
        assert_eq!(aliased.paths(), vec![alias.clone(), original.clone()]);
        let aliased_tree = aliased.into_complete_tree().unwrap();
        assert_eq!(aliased_tree.leaves.len(), 1);
        assert_eq!(aliased_tree.leaves[0].provider_leaf.canonical_path, alias);
        let aliased_tree_fingerprint = aliased_tree.tree_fingerprint;
        let physical_fingerprint = aliased_tree.leaves[0].fingerprint;

        fs::remove_file(&alias).unwrap();
        assert!(revalidate_auggie_tree(&aliased_tree).is_err());
        let unaliased =
            discover_auggie_source_backed(&AuggieSourceBackedRoot::explicit(&hardlink_root))
                .unwrap()
                .into_complete_tree()
                .unwrap();
        assert_ne!(unaliased.tree_fingerprint, aliased_tree_fingerprint);
        assert_eq!(unaliased.leaves.len(), 1);
        assert_eq!(unaliased.leaves[0].fingerprint, physical_fingerprint);
    }
}

#[test]
fn many_leaf_discovery_closes_every_leaf_and_retains_only_bounded_tree_authority() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    for index in 0..2_000 {
        write_history(
            &sessions.join(format!("{index:04}.json")),
            &format!("many-leaf-{index}"),
            &[("request", "request", "response")],
        );
    }
    let tracker = Arc::new(AuggieLeafOpenTracker::default());
    let root = AuggieSourceBackedRoot::explicit(&sessions).with_open_tracker(Arc::clone(&tracker));
    let inventory = discover_auggie_source_backed(&root).unwrap();

    assert_eq!(inventory.paths().len(), 2_000);
    assert_eq!(tracker.total(), 4_000);
    assert_eq!(tracker.peak(), 1);
    assert_eq!(tracker.active(), 0);
    assert!(inventory.is_complete());
    assert_eq!(tracker.active(), 0);
}

#[test]
fn production_document_route_has_no_captured_driver_collector_or_spool() {
    let source_backed = include_str!("source_backed.rs");
    let family = include_str!("../../../source_backed/family/document.rs");
    let hydration = include_str!("source_backed/hydration.rs");
    let source = include_str!("source.rs");
    for (name, production) in [
        ("source_backed", source_backed),
        ("document_family", family),
        ("hydration", hydration),
        ("source", source),
    ] {
        assert!(
            production.lines().count() < 1_000,
            "{name} production file exceeded the 1,000-line bound"
        );
    }
    assert!(!source.contains("Arc<OpenedProviderSourceFile>"));
    assert!(!source_backed.contains("CompleteDocumentTree<AuggieFileStamp"));
    let parser_call = ["parse_opened_auggie_source", "("].concat();
    assert_eq!(
        source_backed.matches(&parser_call).count(),
        1,
        "changed Auggie documents must have exactly one parser call site"
    );
    let forbidden_captured_driver = ["captured_route", "_driver"].concat();
    for forbidden in [
        forbidden_captured_driver.as_str(),
        "AuggieSourceBackedSource",
        "project_auggie_source_backed_inventory",
        "NamedTempFile",
        "tempfile",
        "Vec<LexicalDocument>",
    ] {
        assert!(
            !source_backed.contains(forbidden)
                && !family.contains(forbidden)
                && !hydration.contains(forbidden),
            "production Auggie document route restored forbidden {forbidden}"
        );
    }
}

#[test]
fn provider_b_source_backed_body_architecture_has_no_preview_or_store_contract() {
    let forbidden_preview_cap = ["MAX_BODY_PREVIEW", "_CHARS"].concat();
    let forbidden_legacy_field = ["lexical_", "preview"].concat();
    let forbidden_store = ["ctx_history_", "store::Store"].concat();
    let sources = [
        ("auggie", include_str!("source_backed.rs")),
        (
            "codebuddy",
            include_str!("../../codebuddy/native_path/source_backed.rs"),
        ),
        (
            "continue_cli",
            include_str!("../../continue_cli/native_path/source_backed.rs"),
        ),
        (
            "crush",
            include_str!("../../crush/native_path/source_backed.rs"),
        ),
        ("cursor", include_str!("../../cursor/source_backed.rs")),
        (
            "deepagents",
            include_str!("../../deepagents/native_path/source_backed.rs"),
        ),
        (
            "firebender",
            include_str!("../../firebender/native_path/source_backed.rs"),
        ),
        ("goose", include_str!("../../goose/source_backed.rs")),
        ("hermes", include_str!("../../hermes/source_backed.rs")),
        (
            "kimi",
            include_str!("../../kimi/native_path/source_backed.rs"),
        ),
        ("kiro", include_str!("../../kiro/source_backed.rs")),
    ];
    for (provider, source) in sources {
        assert!(
            !source.contains(&forbidden_preview_cap),
            "{provider} restored the index preview cap"
        );
        assert!(
            !source.contains(&forbidden_legacy_field),
            "{provider} restored lexical-preview construction"
        );
        assert!(
            !source.contains(&forbidden_store),
            "{provider} restored the legacy Store path"
        );
    }
}
