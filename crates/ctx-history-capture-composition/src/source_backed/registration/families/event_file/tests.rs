use super::test_helpers::{indexed_bodies, indexed_events};
use super::*;
use crate::{
    ProviderCatalogSupport, ProviderImportSupport, ProviderSourceKind, ProviderSourceStatus,
};
use ctx_history_capture_model::{ProviderRootDefinition, ProviderRootKind};
use ctx_history_core::{CaptureProvider, CertifiedSource};
use ctx_history_index::{
    GenerationWriter, RevalidationTarget, SourceRouteSnapshot, VerifiedIndex, WriterOptions,
};
use std::{collections::BTreeMap, fs, path::Path};

#[test]
fn valid_malformed_valid_openhands_conversation_projects_both_valid_events() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let selected = temp.path().join("openhands");
    write_message(&selected, "conversation", "event-a", "first valid");
    let malformed = write_message(&selected, "conversation", "event-b", "unused");
    write_message(&selected, "conversation", "event-c", "second valid");
    fs::write(&malformed, b"{not-json").unwrap();
    let index = temp.path().join("index");
    let receipt =
        refresh_source_backed_generation(&index, &registry(&selected), WriterOptions::default())
            .unwrap();

    assert_eq!(receipt.commit.indexed_documents, 2);
    assert!(receipt.logical_source_failures.is_empty());
    assert_eq!(receipt.sources.len(), 1);
    assert_eq!(receipt.sources[0].counts().complete_records, 3);
    assert_eq!(receipt.sources[0].counts().retained_records, 2);
    assert_eq!(receipt.sources[0].counts().rejected_records, 1);
    assert_eq!(
        receipt.record_completion(),
        SourceBackedRecordCompletion::CompletedWithRejections
    );
    assert_eq!(receipt.record_rejections.total(), 1);
    let rejection = &receipt.record_rejections.rejections()[0];
    assert_eq!(
        rejection.route_identity,
        receipt.successful_route_outcomes[0].route_identity
    );
    assert_eq!(
        rejection.source,
        receipt.sources[0].observation().source().clone()
    );
    assert_eq!(rejection.provider, CaptureProvider::OpenHands);
    assert!(rejection.source_selector.ends_with("event-b.json"));
    assert_eq!(rejection.line_number, 1);
    assert_eq!(rejection.payload_type, None);
    assert_eq!(
        rejection.class,
        SourceBackedRecordRejectionClass::MalformedRecord
    );
    let events = indexed_events(&index, &receipt);
    assert_eq!(
        events
            .iter()
            .map(|record| (
                record.event_sequence,
                record.content.meaningful_text().to_owned()
            ))
            .collect::<Vec<_>>(),
        vec![
            (0, "first valid".to_owned()),
            (2, "second valid".to_owned())
        ]
    );
}

#[test]
fn openhands_record_rejection_diagnostics_are_bounded_before_publication() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let selected = temp.path().join("openhands");
    write_message(&selected, "conversation", "valid", "healthy");
    for index in 0..70 {
        let malformed = write_message(
            &selected,
            "conversation",
            &format!("malformed-{index:02}"),
            "unused",
        );
        fs::write(malformed, b"{not-json").unwrap();
    }
    let receipt = refresh_source_backed_generation(
        temp.path().join("index"),
        &registry(&selected),
        WriterOptions::default(),
    )
    .unwrap();

    assert_eq!(receipt.commit.indexed_documents, 1);
    assert_eq!(receipt.sources[0].counts().rejected_records, 70);
    assert_eq!(receipt.record_rejections.rejections().len(), 64);
    assert_eq!(receipt.record_rejections.omitted(), 6);
    assert_eq!(receipt.record_rejections.total(), 70);
}

#[test]
fn cold_all_malformed_openhands_source_fails_narrowly_and_publishes_valid_peer() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let selected = temp.path().join("openhands");
    let malformed = write_message(&selected, "conversation-a", "event-a", "unused");
    fs::write(malformed, b"{not-json").unwrap();
    write_message(&selected, "conversation-b", "event-b", "healthy");
    let index = temp.path().join("index");

    let receipt =
        refresh_source_backed_generation(&index, &registry(&selected), WriterOptions::default())
            .unwrap();
    assert_eq!(receipt.commit.indexed_documents, 1);
    assert_eq!(receipt.sources.len(), 1);
    assert_eq!(receipt.logical_source_failures.failures().len(), 1);
    let failure = &receipt.logical_source_failures.failures()[0];
    assert!(!failure.carried_forward);
    assert_eq!(
        failure.route_identity,
        receipt.successful_route_outcomes[0].route_identity
    );
    assert_eq!(
        receipt.successful_route_outcomes[0].logical_source_failure_total,
        1
    );
    assert_eq!(receipt.record_rejections.total(), 1);
    assert_eq!(indexed_bodies(&index, &receipt), vec!["healthy"]);
}

#[test]
fn cold_only_malformed_openhands_source_remains_fail_closed() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let selected = temp.path().join("openhands");
    let malformed = write_message(&selected, "conversation-a", "event-a", "unused");
    fs::write(malformed, b"{not-json").unwrap();

    let error = refresh_source_backed_generation(
        temp.path().join("index"),
        &registry(&selected),
        WriterOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        SourceBackedCoordinatorError::NoUsableLogicalSources { .. }
    ));
}

#[test]
fn warm_malformed_openhands_peer_retains_prior_while_successful_peer_publishes() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let selected = temp.path().join("openhands");
    let first = write_message(&selected, "conversation-a", "event-a", "prior healthy");
    let second = write_message(&selected, "conversation-b", "event-b", "peer old");
    let index = temp.path().join("index");
    let registry = registry(&selected);
    refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();

    fs::write(&first, b"{not-json").unwrap();
    fs::write(
        &second,
        serde_json::to_vec(&message("event-b", "peer new")).unwrap(),
    )
    .unwrap();
    let refreshed =
        refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();

    assert_eq!(refreshed.commit.indexed_documents, 2);
    assert_eq!(refreshed.logical_source_failures.failures().len(), 1);
    let failure = &refreshed.logical_source_failures.failures()[0];
    assert!(failure.carried_forward);
    assert_eq!(
        failure.route_identity,
        refreshed.successful_route_outcomes[0].route_identity
    );
    assert_eq!(
        refreshed.successful_route_outcomes[0].logical_source_failure_total,
        1
    );
    assert_eq!(refreshed.record_rejections.total(), 1);
    assert_eq!(
        indexed_bodies(&index, &refreshed),
        vec!["peer new", "prior healthy"]
    );
}

#[test]
fn successful_route_reports_exact_logical_failure_total_beyond_detail_bound() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let selected = temp.path().join("openhands");
    write_message(&selected, "healthy", "valid", "healthy");
    for index in 0..70 {
        let malformed = write_message(
            &selected,
            &format!("malformed-{index:02}"),
            "event",
            "unused",
        );
        fs::write(malformed, b"{not-json").unwrap();
    }
    let receipt = refresh_source_backed_generation(
        temp.path().join("index"),
        &registry(&selected),
        WriterOptions::default(),
    )
    .unwrap();

    assert_eq!(receipt.commit.indexed_documents, 1);
    assert_eq!(receipt.logical_source_failures.failures().len(), 64);
    assert_eq!(receipt.logical_source_failures.omitted(), 6);
    assert_eq!(receipt.logical_source_failures.total(), 70);
    let outcome = &receipt.successful_route_outcomes[0];
    assert_eq!(outcome.logical_source_failure_total, 70);
    assert_eq!(outcome.logical_source_retryable_failure_total, 0);
    assert!(receipt
        .logical_source_failures
        .failures()
        .iter()
        .all(|failure| failure.route_identity == outcome.route_identity));
}

#[test]
fn openhands_warm_append_and_rewrite_keep_valid_event_ids_and_order() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let selected = temp.path().join("openhands");
    write_message(&selected, "conversation", "event-1", "one");
    let index = temp.path().join("index");
    let registry = registry(&selected);
    let cold =
        refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
    let cold_id = indexed_events(&index, &cold)[0].event_id;

    let malformed = write_message(&selected, "conversation", "event-2", "unused");
    fs::write(&malformed, b"{not-json").unwrap();
    write_message(&selected, "conversation", "event-3", "three");
    let appended =
        refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
    assert_eq!(appended.sources[0].counts().rejected_records, 1);
    let appended_events = indexed_events(&index, &appended);
    assert_eq!(
        appended_events
            .iter()
            .map(|record| record.event_sequence)
            .collect::<Vec<_>>(),
        vec![0, 2]
    );
    assert_eq!(appended_events[0].event_id, cold_id);

    let appended_replay =
        refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
    assert_eq!(
        appended_replay.commit.generation_id,
        appended.commit.generation_id
    );
    assert_eq!(
        appended_replay.record_completion(),
        SourceBackedRecordCompletion::CompletedWithRejections
    );

    fs::write(
        &malformed,
        serde_json::to_vec(&message("event-2", "two repaired")).unwrap(),
    )
    .unwrap();
    let rewritten =
        refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
    assert_eq!(rewritten.sources[0].counts().rejected_records, 0);
    let rewritten_events = indexed_events(&index, &rewritten);
    assert_eq!(
        rewritten_events
            .iter()
            .map(|record| record.event_sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(rewritten_events[0].event_id, cold_id);
    let unique_ids = rewritten_events
        .iter()
        .map(|record| record.event_id)
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(unique_ids.len(), rewritten_events.len());

    let replay =
        refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
    assert_eq!(replay.commit.generation_id, rewritten.commit.generation_id);
    assert_eq!(indexed_events(&index, &replay), rewritten_events);
}

#[test]
fn openhands_one_vs_four_workers_have_cold_and_unchanged_parity() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let selected = temp.path().join("openhands");
    for index in 0..8 {
        write_message(
            &selected,
            &format!("conversation-{index}"),
            "event-a",
            &format!("body {index}"),
        );
    }
    let malformed = write_message(&selected, "conversation-3", "event-b", "not retained");
    fs::write(malformed, b"{not-json").unwrap();
    let registry = registry(&selected);
    let serial_index = temp.path().join("serial-index");
    let parallel_index = temp.path().join("parallel-index");

    let serial = refresh_source_backed_generation_with_work_budget_for_test(
        &serial_index,
        &registry,
        WriterOptions::default(),
        1,
    )
    .unwrap();
    let parallel = refresh_source_backed_generation_with_work_budget_for_test(
        &parallel_index,
        &registry,
        WriterOptions::default(),
        4,
    )
    .unwrap();
    assert_eq!(parallel.commit.generation_id, serial.commit.generation_id);
    assert_eq!(parallel.sources, serial.sources);
    assert_eq!(parallel.record_rejections, serial.record_rejections);
    assert_eq!(
        indexed_events(&parallel_index, &parallel),
        indexed_events(&serial_index, &serial)
    );
    assert_eq!(
        parallel
            .sources
            .iter()
            .map(|source| source.counts().rejected_records)
            .sum::<u64>(),
        1
    );

    let serial_replay = refresh_source_backed_generation_with_work_budget_for_test(
        &serial_index,
        &registry,
        WriterOptions::default(),
        1,
    )
    .unwrap();
    let parallel_replay = refresh_source_backed_generation_with_work_budget_for_test(
        &parallel_index,
        &registry,
        WriterOptions::default(),
        4,
    )
    .unwrap();
    assert_eq!(
        serial_replay.commit.generation_id,
        serial.commit.generation_id
    );
    assert_eq!(
        parallel_replay.commit.generation_id,
        parallel.commit.generation_id
    );
}

#[test]
fn openhands_v3_to_current_migration_preserves_ids_order_and_uniqueness() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let selected = temp.path().join("openhands");
    for index in 0..3 {
        write_message(
            &selected,
            "conversation",
            &format!("event-{index}"),
            &format!("body {index}"),
        );
    }
    let registry = registry(&selected);
    let fixture_index = temp.path().join("current-fixture-index");
    let fixture =
        refresh_source_backed_generation(&fixture_index, &registry, WriterOptions::default())
            .unwrap();
    let source = fixture.sources[0].clone();
    let mut v3_records = indexed_events(&fixture_index, &fixture);
    let expected_ids = v3_records
        .iter()
        .map(|record| record.event_id)
        .collect::<Vec<_>>();
    for record in &mut v3_records {
        record.parser_revision = "openhands-source-backed-v3".to_owned();
    }
    let v3_certificate = CertifiedSource::certify(
        source.observation().clone(),
        source.observation().clone(),
        "openhands-source-backed-v3",
        *source.content_digest(),
        source.counts(),
    )
    .unwrap();
    let migration_index = temp.path().join("migration-index");
    let mut writer = GenerationWriter::open(&migration_index, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer
        .begin_source(source.observation().source().clone())
        .unwrap();
    for record in v3_records {
        writer.add_core_record(record).unwrap();
    }
    writer.certify_source(v3_certificate.clone()).unwrap();
    writer
        .set_present_source_routes(vec![SourceRouteSnapshot::present(
            fixture.selected_route_ids[0].clone(),
            vec![source.observation().source().clone()],
        )
        .unwrap()])
        .unwrap();
    let seeded = writer
            .commit(|target| {
                matches!(target, RevalidationTarget::Source(source) if source == &v3_certificate)
            })
            .unwrap();

    let migrated =
        refresh_source_backed_generation(&migration_index, &registry, WriterOptions::default())
            .unwrap();
    assert_ne!(migrated.commit.generation_id, seeded.generation_id);
    assert_eq!(
        migrated.sources[0].parser_revision(),
        "openhands-source-backed-v7-naive-time"
    );
    let migrated_events = indexed_events(&migration_index, &migrated);
    assert_eq!(
        migrated_events
            .iter()
            .map(|record| record.event_id)
            .collect::<Vec<_>>(),
        expected_ids
    );
    assert_eq!(
        migrated_events
            .iter()
            .map(|record| record.event_sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(
        migrated_events
            .iter()
            .map(|record| record.event_id)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        migrated_events.len()
    );
}

#[test]
fn openhands_v6_warm_upgrade_replays_naive_timestamp_projection() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let selected = temp.path().join("profile");
    write_current_message(
        &selected,
        "conversation",
        1,
        "event-naive",
        "naive body",
        "2026-11-01T01:30:00",
    );
    let registry = registry(&selected);
    let fixture_index = temp.path().join("v7-fixture-index");
    let fixture =
        refresh_source_backed_generation(&fixture_index, &registry, WriterOptions::default())
            .unwrap();
    let source = fixture.sources[0].clone();
    let mut v6_records = indexed_events(&fixture_index, &fixture);
    let expected_event_id = v6_records[0].event_id;
    v6_records[0].parser_revision = "openhands-source-backed-v6-closed-facts".to_owned();
    v6_records[0].occurred_at_unix_ms = Some(
        "2026-11-01T01:30:00Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap()
            .timestamp_millis(),
    );
    let v6_certificate = CertifiedSource::certify(
        source.observation().clone(),
        source.observation().clone(),
        "openhands-source-backed-v6-closed-facts",
        *source.content_digest(),
        source.counts(),
    )
    .unwrap();

    let migration_index = temp.path().join("v6-migration-index");
    let mut writer = GenerationWriter::open(&migration_index, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer
        .begin_source(source.observation().source().clone())
        .unwrap();
    writer.add_core_record(v6_records.remove(0)).unwrap();
    writer.certify_source(v6_certificate.clone()).unwrap();
    writer
        .set_present_source_routes(vec![SourceRouteSnapshot::present(
            fixture.selected_route_ids[0].clone(),
            vec![source.observation().source().clone()],
        )
        .unwrap()])
        .unwrap();
    let seeded = writer
            .commit(|target| {
                matches!(target, RevalidationTarget::Source(source) if source == &v6_certificate)
            })
            .unwrap();

    let upgraded =
        refresh_source_backed_generation(&migration_index, &registry, WriterOptions::default())
            .unwrap();
    assert_ne!(upgraded.commit.generation_id, seeded.generation_id);
    assert_eq!(
        upgraded.sources[0].parser_revision(),
        "openhands-source-backed-v7-naive-time"
    );
    let upgraded_events = indexed_events(&migration_index, &upgraded);
    assert_eq!(upgraded_events[0].event_id, expected_event_id);
    assert_eq!(upgraded_events[0].occurred_at_unix_ms, None);

    let replay =
        refresh_source_backed_generation(&migration_index, &registry, WriterOptions::default())
            .unwrap();
    assert_eq!(replay.commit.generation_id, upgraded.commit.generation_id);
}

#[test]
fn current_cli_exact_discovery_imports_an_event_leaf() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let selected = temp.path().join("profile");
    let event = write_current_message(
        &selected,
        "conversation-exact",
        1,
        "event-exact",
        "exact current body",
        "2026-07-28T12:00:00Z",
    );
    let source = crate::provider_source_for_path(CaptureProvider::OpenHands, event);
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.source_format, OPENHANDS_CURRENT_CLI_SOURCE_FORMAT);

    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        source,
        SourceBackedRouteSelection::ExplicitManual,
    )
    .unwrap();
    assert_eq!(
        registry.routes().next().unwrap().certified_source_format,
        OPENHANDS_FILE_EVENTS_SOURCE_FORMAT
    );
    let index = temp.path().join("exact-index");
    let receipt =
        refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
    assert_eq!(indexed_bodies(&index, &receipt), vec!["exact current body"]);
}

#[test]
fn configured_openhands_roots_scope_same_native_ids_preserve_moves_and_isolate_layouts() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let current = temp.path().join("current-conversations");
    let legacy = temp.path().join("legacy-persistence");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    write_current_message_at_root(
        &current,
        "same-native-session",
        1,
        "same-native-event",
        "current body",
        "2026-07-28T12:00:00Z",
    );
    write_message(
        &legacy,
        "same-native-session",
        "same-native-event",
        "legacy body",
    );
    write_message(
        &current,
        "nested-legacy-session",
        "nested-legacy-event",
        "must stay isolated",
    );
    let definition = |id: &str, path: &Path, kind| ProviderRootDefinition {
        id: id.to_owned(),
        provider: CaptureProvider::OpenHands,
        path: path.to_path_buf(),
        group: None,
        kind: Some(kind),
    };
    let legacy_definition = definition(
        "legacy",
        &legacy,
        ProviderRootKind::OpenHandsLegacyPersistence,
    );
    let context = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    )
    .with_automatic_provider_discovery(false)
    .with_configured_provider_roots(vec![
        definition(
            "current",
            &current,
            ProviderRootKind::OpenHandsCurrentConversations,
        ),
        legacy_definition.clone(),
    ]);
    let data_root = temp.path().join("ctx-data");
    let index = temp.path().join("index");
    let cold = refresh_source_backed_generation(
        &index,
        &discovered_openhands_registry(&context, &data_root),
        WriterOptions::default(),
    )
    .unwrap();
    let cold = indexed_events(&index, &cold)
        .into_iter()
        .map(|record| (record.content.meaningful_text().to_owned(), record))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(cold.len(), 2);
    assert!(!cold.contains_key("must stay isolated"));
    let current_record = &cold["current body"];
    let legacy_record = &cold["legacy body"];
    assert_eq!(
        current_record.provider_session_id,
        legacy_record.provider_session_id
    );
    assert_eq!(
        current_record.native_event_id,
        legacy_record.native_event_id
    );
    assert_ne!(
        current_record.source.identity(),
        legacy_record.source.identity()
    );
    assert_ne!(current_record.session_id, legacy_record.session_id);
    assert_ne!(current_record.event_id, legacy_record.event_id);

    let moved_current = temp.path().join("moved-current-conversations");
    write_current_message_at_root(
        &moved_current,
        "same-native-session",
        1,
        "same-native-event",
        "current body",
        "2026-07-28T12:00:00Z",
    );
    let moved_context = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    )
    .with_automatic_provider_discovery(false)
    .with_configured_provider_roots(vec![
        definition(
            "current",
            &moved_current,
            ProviderRootKind::OpenHandsCurrentConversations,
        ),
        legacy_definition,
    ]);
    let moved = refresh_source_backed_generation(
        &index,
        &discovered_openhands_registry(&moved_context, &data_root),
        WriterOptions::default(),
    )
    .unwrap();
    let moved = indexed_events(&index, &moved)
        .into_iter()
        .map(|record| (record.content.meaningful_text().to_owned(), record))
        .collect::<BTreeMap<_, _>>();
    let moved_current_record = &moved["current body"];
    assert!(moved_current_record
        .source
        .exact_descriptor_eq(&current_record.source));
    assert_eq!(moved_current_record.session_id, current_record.session_id);
    assert_eq!(moved_current_record.event_id, current_record.event_id);
}

mod automatic;
fn provider_source(path: &Path) -> ProviderSource {
    ProviderSource {
        provider: CaptureProvider::OpenHands,
        path: path.to_path_buf(),
        exists: true,
        source_format: OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
        route_provenance: Default::default(),
    }
}

fn registry(path: &Path) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    register_openhands_route(
        &mut registry,
        provider_source(path),
        SourceBackedRouteSelection::Automatic,
        None,
    )
    .unwrap();
    registry
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
    ctx_history_capture_model::SourceRouteIdentity,
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
