use super::*;
use crate::ProviderSourceStatus;
use ctx_history_core::{CaptureProvider, CoreRecord};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use ctx_history_provider_docproj::OPENHANDS_FILE_EVENTS_SOURCE_FORMAT;
use std::{fs, path::Path};

#[test]
fn disjoint_current_root_disappearance_deletes_and_restores_only_its_route() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    fs::create_dir_all(&cwd).unwrap();
    let legacy_root = temp.path().join("legacy-profile");
    let current_profile = temp.path().join("current-profile");
    let conversations = current_profile.join("conversations");
    write_message(
        &legacy_root,
        "conversation-legacy",
        "legacy-event",
        "legacy body",
    );
    write_current_message(
        &current_profile,
        "conversation-current",
        1,
        "current-event",
        "current body",
        "2026-07-28T12:00:00Z",
    );
    let context = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    )
    .with_env("OH_PERSISTENCE_DIR", legacy_root.as_os_str().to_owned())
    .with_env(
        "OPENHANDS_CONVERSATIONS_DIR",
        conversations.as_os_str().to_owned(),
    );
    let data_root = temp.path().join("ctx-data");
    let index = temp.path().join("root-disappearance-index");

    let cold_registry = discovered_openhands_registry(&context, &data_root);
    let cold_routes = openhands_route_facts(&cold_registry);
    let current_route_id = cold_routes
        .iter()
        .find(|(_, format, _)| *format == OPENHANDS_CURRENT_CLI_SOURCE_FORMAT)
        .unwrap()
        .0
        .clone();
    let legacy_route_id = cold_routes
        .iter()
        .find(|(_, format, _)| *format == OPENHANDS_FILE_EVENTS_SOURCE_FORMAT)
        .unwrap()
        .0
        .clone();
    let cold =
        refresh_source_backed_generation(&index, &cold_registry, WriterOptions::default()).unwrap();
    assert_eq!(cold.sources.len(), 2);
    assert_eq!(cold.commit.indexed_documents, 2);
    let cold_current = indexed_events(&index, &cold)
        .into_iter()
        .find(|record| record.content.meaningful_text() == "current body")
        .unwrap();

    fs::remove_dir_all(&conversations).unwrap();
    let mut deleted = None;
    for observation in 1..=AUTOMATIC_ROUTE_DELETION_MISSING_OBSERVATIONS {
        let missing_registry = discovered_openhands_registry(&context, &data_root);
        assert_eq!(openhands_route_facts(&missing_registry), cold_routes);
        let current = missing_registry
            .routes()
            .find(|route| route.source.source_format == OPENHANDS_CURRENT_CLI_SOURCE_FORMAT)
            .unwrap();
        assert_eq!(current.source.status, ProviderSourceStatus::Missing);
        let receipt =
            refresh_source_backed_generation(&index, &missing_registry, WriterOptions::default())
                .unwrap();
        if observation < AUTOMATIC_ROUTE_DELETION_MISSING_OBSERVATIONS {
            assert_eq!(
                indexed_bodies(&index, &receipt),
                vec!["current body", "legacy body"]
            );
        } else {
            deleted = Some(receipt);
        }
    }
    let deleted = deleted.unwrap();
    assert_eq!(indexed_bodies(&index, &deleted), vec!["legacy body"]);
    assert_eq!(deleted.sources.len(), 1);
    assert_eq!(deleted.commit.indexed_documents, 1);
    assert!(deleted
        .commit
        .manifest()
        .source_route(&current_route_id)
        .is_none());
    assert!(deleted
        .commit
        .manifest()
        .source_route(&legacy_route_id)
        .is_some());

    write_current_message(
        &current_profile,
        "conversation-current",
        1,
        "current-event",
        "current body",
        "2026-07-28T12:00:00Z",
    );
    let restored_registry = discovered_openhands_registry(&context, &data_root);
    assert_eq!(openhands_route_facts(&restored_registry), cold_routes);
    let restored =
        refresh_source_backed_generation(&index, &restored_registry, WriterOptions::default())
            .unwrap();
    assert_eq!(restored.sources.len(), 2);
    assert_eq!(restored.commit.indexed_documents, 2);
    let restored_current = indexed_events(&index, &restored)
        .into_iter()
        .find(|record| record.content.meaningful_text() == "current body")
        .unwrap();
    assert_eq!(restored_current.source, cold_current.source);
    assert_eq!(restored_current.session_id, cold_current.session_id);
    assert_eq!(restored_current.event_id, cold_current.event_id);
}

#[test]
fn default_current_only_root_disappearance_ages_out_and_restores_exactly() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    fs::create_dir_all(&cwd).unwrap();
    let profile = home.join(".openhands");
    write_current_message(
        &profile,
        "conversation-current",
        1,
        "current-event",
        "current body",
        "2026-07-28T12:00:00Z",
    );
    let context = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    );
    let data_root = temp.path().join("ctx-data");
    let index = temp.path().join("default-root-disappearance-index");

    let cold_registry = discovered_openhands_registry(&context, &data_root);
    let cold_routes = openhands_route_facts(&cold_registry);
    assert_eq!(cold_routes.len(), 1);
    assert_eq!(cold_routes[0].1, OPENHANDS_CURRENT_CLI_SOURCE_FORMAT);
    let current_route_id = cold_routes[0].0.clone();
    let cold =
        refresh_source_backed_generation(&index, &cold_registry, WriterOptions::default()).unwrap();
    let cold_current = indexed_events(&index, &cold).remove(0);

    fs::remove_dir_all(&profile).unwrap();
    let exact_registry = discovered_openhands_registry(&context, &data_root);
    let missing_routes = openhands_route_facts(&exact_registry);
    assert_eq!(missing_routes.len(), 2);
    let legacy_route_id = missing_routes
        .iter()
        .find(|(_, format, _)| *format == OPENHANDS_FILE_EVENTS_SOURCE_FORMAT)
        .unwrap()
        .0
        .clone();
    let missing_current = exact_registry
        .routes()
        .find(|route| route.source.source_format == OPENHANDS_CURRENT_CLI_SOURCE_FORMAT)
        .unwrap();
    assert_eq!(
        missing_current.route_identity.as_ref(),
        Some(&current_route_id)
    );
    assert_eq!(missing_current.source.status, ProviderSourceStatus::Missing);

    let exact = refresh_source_backed_generation_for_routes(
        &index,
        &exact_registry,
        WriterOptions::default(),
        [legacy_route_id],
    )
    .unwrap();
    assert_eq!(indexed_bodies(&index, &exact), vec!["current body"]);
    assert!(exact
        .commit
        .manifest()
        .source_route(&current_route_id)
        .is_some());

    let mut deleted = None;
    for observation in 1..=AUTOMATIC_ROUTE_DELETION_MISSING_OBSERVATIONS {
        let missing_registry = discovered_openhands_registry(&context, &data_root);
        let current = missing_registry
            .routes()
            .find(|route| route.source.source_format == OPENHANDS_CURRENT_CLI_SOURCE_FORMAT)
            .unwrap();
        assert_eq!(current.route_identity.as_ref(), Some(&current_route_id));
        assert_eq!(current.source.status, ProviderSourceStatus::Missing);
        let receipt =
            refresh_source_backed_generation(&index, &missing_registry, WriterOptions::default())
                .unwrap();
        if observation < AUTOMATIC_ROUTE_DELETION_MISSING_OBSERVATIONS {
            assert_eq!(indexed_bodies(&index, &receipt), vec!["current body"]);
        } else {
            deleted = Some(receipt);
        }
    }
    let deleted = deleted.unwrap();
    assert!(deleted.sources.is_empty());
    assert_eq!(deleted.commit.indexed_documents, 0);
    assert!(deleted
        .commit
        .manifest()
        .source_route(&current_route_id)
        .is_none());

    write_current_message(
        &profile,
        "conversation-current",
        1,
        "current-event",
        "current body",
        "2026-07-28T12:00:00Z",
    );
    let restored_registry = discovered_openhands_registry(&context, &data_root);
    assert_eq!(openhands_route_facts(&restored_registry), cold_routes);
    let restored =
        refresh_source_backed_generation(&index, &restored_registry, WriterOptions::default())
            .unwrap();
    let restored_current = indexed_events(&index, &restored).remove(0);
    assert_eq!(restored_current.source, cold_current.source);
    assert_eq!(restored_current.session_id, cold_current.session_id);
    assert_eq!(restored_current.event_id, cold_current.event_id);
}

#[test]
fn overlapping_default_layout_transfers_route_ownership_in_both_directions() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    fs::create_dir_all(&cwd).unwrap();
    let profile = home.join(".openhands");
    let legacy_event = write_message(
        &profile,
        "conversation-legacy",
        "legacy-event",
        "legacy body",
    );
    write_current_message(
        &profile,
        "conversation-current",
        1,
        "current-event",
        "current body",
        "2026-07-28T12:00:00Z",
    );
    let context = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    );
    let data_root = temp.path().join("ctx-data");
    let index = temp.path().join("layout-transition-index");

    let umbrella_registry = discovered_openhands_registry(&context, &data_root);
    let umbrella_routes = openhands_route_facts(&umbrella_registry);
    assert_eq!(umbrella_routes.len(), 1);
    assert_eq!(umbrella_routes[0].1, OPENHANDS_FILE_EVENTS_SOURCE_FORMAT);
    let umbrella_route_id = umbrella_routes[0].0.clone();
    let cold =
        refresh_source_backed_generation(&index, &umbrella_registry, WriterOptions::default())
            .unwrap();
    assert_eq!(cold.sources.len(), 2);
    assert_eq!(cold.commit.indexed_documents, 2);
    let cold_events = indexed_events(&index, &cold);
    let cold_current = cold_events
        .iter()
        .find(|record| record.content.meaningful_text() == "current body")
        .unwrap();
    let cold_legacy = cold_events
        .iter()
        .find(|record| record.content.meaningful_text() == "legacy body")
        .unwrap();

    fs::remove_file(&legacy_event).unwrap();
    let current_registry = discovered_openhands_registry(&context, &data_root);
    let current_routes = openhands_route_facts(&current_registry);
    assert_eq!(current_routes.len(), 1);
    assert_eq!(current_routes[0].1, OPENHANDS_CURRENT_CLI_SOURCE_FORMAT);
    let current_route_id = current_routes[0].0.clone();
    assert_ne!(current_route_id, umbrella_route_id);
    let transitioned =
        refresh_source_backed_generation(&index, &current_registry, WriterOptions::default())
            .unwrap();
    assert_eq!(transitioned.sources.len(), 1);
    assert_eq!(transitioned.commit.indexed_documents, 1);
    assert_eq!(indexed_bodies(&index, &transitioned), vec!["current body"]);
    assert!(transitioned
        .commit
        .manifest()
        .source_route(&umbrella_route_id)
        .is_none());
    let transitioned_current = indexed_events(&index, &transitioned).remove(0);
    assert_eq!(transitioned_current.source, cold_current.source);
    assert_eq!(transitioned_current.session_id, cold_current.session_id);
    assert_eq!(transitioned_current.event_id, cold_current.event_id);

    fs::write(
        &legacy_event,
        serde_json::to_vec(&message("legacy-event", "legacy body")).unwrap(),
    )
    .unwrap();
    let restored_registry = discovered_openhands_registry(&context, &data_root);
    assert_eq!(openhands_route_facts(&restored_registry), umbrella_routes);
    let restored =
        refresh_source_backed_generation(&index, &restored_registry, WriterOptions::default())
            .unwrap();
    assert_eq!(restored.sources.len(), 2);
    assert_eq!(restored.commit.indexed_documents, 2);
    assert!(restored
        .commit
        .manifest()
        .source_route(&current_route_id)
        .is_none());
    let restored_events = indexed_events(&index, &restored);
    let restored_current = restored_events
        .iter()
        .find(|record| record.content.meaningful_text() == "current body")
        .unwrap();
    let restored_legacy = restored_events
        .iter()
        .find(|record| record.content.meaningful_text() == "legacy body")
        .unwrap();
    assert_eq!(restored_current.source, cold_current.source);
    assert_eq!(restored_current.session_id, cold_current.session_id);
    assert_eq!(restored_current.event_id, cold_current.event_id);
    assert_eq!(restored_legacy.source, cold_legacy.source);
    assert_eq!(restored_legacy.session_id, cold_legacy.session_id);
    assert_eq!(restored_legacy.event_id, cold_legacy.event_id);
}

#[test]
fn nested_arbitrary_current_root_transfers_its_exact_route_identity() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    fs::create_dir_all(&cwd).unwrap();
    let profile = home.join(".openhands");
    let current_root = profile.join("nested/official-direct-root");
    let legacy_event = write_message(
        &profile,
        "conversation-legacy",
        "legacy-event",
        "legacy body",
    );
    write_current_message_at_root(
        &current_root,
        "conversation-current",
        1,
        "current-event",
        "current body",
        "2026-07-28T12:00:00Z",
    );
    let context = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    )
    .with_env(
        "OPENHANDS_CONVERSATIONS_DIR",
        current_root.as_os_str().to_owned(),
    );
    let data_root = temp.path().join("ctx-data");
    let index = temp.path().join("nested-layout-transition-index");

    let umbrella_registry = discovered_openhands_registry(&context, &data_root);
    let umbrella_routes = openhands_route_facts(&umbrella_registry);
    assert_eq!(umbrella_routes.len(), 1);
    assert_eq!(umbrella_routes[0].1, OPENHANDS_FILE_EVENTS_SOURCE_FORMAT);
    let umbrella_route_id = umbrella_routes[0].0.clone();
    let cold =
        refresh_source_backed_generation(&index, &umbrella_registry, WriterOptions::default())
            .unwrap();
    assert_eq!(
        indexed_bodies(&index, &cold),
        vec!["current body", "legacy body"]
    );
    let cold_events = indexed_events(&index, &cold);
    let cold_current = cold_events
        .iter()
        .find(|record| record.content.meaningful_text() == "current body")
        .unwrap();
    let cold_legacy = cold_events
        .iter()
        .find(|record| record.content.meaningful_text() == "legacy body")
        .unwrap();

    fs::remove_file(&legacy_event).unwrap();
    let current_registry = discovered_openhands_registry(&context, &data_root);
    let current_route = current_registry.routes().next().unwrap();
    assert_eq!(current_route.source.path, current_root);
    assert_eq!(
        current_route.source.source_format,
        OPENHANDS_CURRENT_CLI_SOURCE_FORMAT
    );
    let current_route_id = current_route.route_identity.clone().unwrap();
    let transitioned =
        refresh_source_backed_generation(&index, &current_registry, WriterOptions::default())
            .unwrap();
    assert_eq!(indexed_bodies(&index, &transitioned), vec!["current body"]);
    assert!(transitioned
        .commit
        .manifest()
        .source_route(&umbrella_route_id)
        .is_none());
    let transitioned_current = indexed_events(&index, &transitioned).remove(0);
    assert_eq!(transitioned_current.source, cold_current.source);
    assert_eq!(transitioned_current.session_id, cold_current.session_id);
    assert_eq!(transitioned_current.event_id, cold_current.event_id);

    fs::write(
        &legacy_event,
        serde_json::to_vec(&message("legacy-event", "legacy body")).unwrap(),
    )
    .unwrap();
    let restored_registry = discovered_openhands_registry(&context, &data_root);
    assert_eq!(openhands_route_facts(&restored_registry), umbrella_routes);
    let restored =
        refresh_source_backed_generation(&index, &restored_registry, WriterOptions::default())
            .unwrap();
    assert!(restored
        .commit
        .manifest()
        .source_route(&current_route_id)
        .is_none());
    assert_eq!(
        indexed_bodies(&index, &restored),
        vec!["current body", "legacy body"]
    );
    let restored_events = indexed_events(&index, &restored);
    let restored_current = restored_events
        .iter()
        .find(|record| record.content.meaningful_text() == "current body")
        .unwrap();
    let restored_legacy = restored_events
        .iter()
        .find(|record| record.content.meaningful_text() == "legacy body")
        .unwrap();
    assert_eq!(restored_current.source, cold_current.source);
    assert_eq!(restored_current.session_id, cold_current.session_id);
    assert_eq!(restored_current.event_id, cold_current.event_id);
    assert_eq!(restored_legacy.source, cold_legacy.source);
    assert_eq!(restored_legacy.session_id, cold_legacy.session_id);
    assert_eq!(restored_legacy.event_id, cold_legacy.event_id);
}

fn discovered_openhands_registry(
    context: &DiscoveryContext,
    data_root: &Path,
) -> SourceBackedProviderRegistry {
    let report = ctx_history_source_discovery::discover_provider_sources_for_provider_with_context(
        &crate::test_provider_probes(),
        context,
        CaptureProvider::OpenHands,
    );
    let build = build_automatic_source_backed_registry_from_report_with_probes(
        &crate::test_provider_probes(),
        context,
        data_root,
        report,
    );
    assert!(build.issues.is_empty(), "{:?}", build.issues);
    build.registry
}

fn openhands_route_facts(
    registry: &SourceBackedProviderRegistry,
) -> Vec<(
    SourceRouteIdentity,
    &'static str,
    SourceBackedSelectorAuthority,
)> {
    let mut facts = registry
        .routes()
        .filter(|route| route.source.provider == CaptureProvider::OpenHands)
        .map(|route| {
            (
                route.route_identity.clone().unwrap(),
                route.source.source_format,
                route.selector_authority,
            )
        })
        .collect::<Vec<_>>();
    facts.sort_by(|left, right| left.1.cmp(right.1));
    facts
}

fn indexed_bodies(index: &Path, receipt: &SourceBackedRefreshReceipt) -> Vec<String> {
    let mut bodies = indexed_events(index, receipt)
        .into_iter()
        .map(|record| record.content.meaningful_text().to_owned())
        .collect::<Vec<_>>();
    bodies.sort();
    bodies
}

fn indexed_events(index: &Path, receipt: &SourceBackedRefreshReceipt) -> Vec<CoreRecord> {
    let verified = VerifiedIndex::open_pinned(index).unwrap();
    let mut events = receipt
        .sources
        .iter()
        .flat_map(|source| {
            verified
                .core_source_event_page(source.observation().source(), None, 64)
                .unwrap()
                .items
                .into_iter()
                .map(|event| event.core_record)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    events.sort_by(|left, right| {
        left.source
            .exact_descriptor_digest()
            .cmp(&right.source.exact_descriptor_digest())
            .then_with(|| left.event_sequence.cmp(&right.event_sequence))
    });
    events
}

fn write_message(root: &Path, conversation: &str, id: &str, body: &str) -> std::path::PathBuf {
    let path = root
        .join("v1_conversations")
        .join(conversation)
        .join(format!("{id}.json"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, serde_json::to_vec(&message(id, body)).unwrap()).unwrap();
    path
}

fn write_current_message(
    root: &Path,
    conversation: &str,
    ordinal: usize,
    id: &str,
    body: &str,
    timestamp: &str,
) -> std::path::PathBuf {
    write_current_message_at_root(
        &root.join("conversations"),
        conversation,
        ordinal,
        id,
        body,
        timestamp,
    )
}

fn write_current_message_at_root(
    root: &Path,
    conversation: &str,
    ordinal: usize,
    id: &str,
    body: &str,
    timestamp: &str,
) -> std::path::PathBuf {
    let path = root
        .join(conversation)
        .join("events")
        .join(format!("event-{ordinal:05}-{id}.json"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut event = message(id, body);
    event["timestamp"] = serde_json::Value::String(timestamp.to_owned());
    fs::write(&path, serde_json::to_vec(&event).unwrap()).unwrap();
    path
}

fn message(id: &str, body: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "timestamp": "2026-07-28T12:00:00Z",
        "kind": "MessageEvent",
        "source": "agent",
        "llm_message": { "role": "assistant", "content": body },
    })
}
