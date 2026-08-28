use super::*;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use ctx_history_capture_model::{
    provider_source_config_digest, ProviderRootDefinition, ProviderRouteRole,
    ProviderSourceRouteProvenance, SourceRouteIdentity,
};
use ctx_history_core::{
    derive_event_id, AgentScope, CaptureProvider, CertifiedSource, CertifiedSourceInventory,
    CoreRecord, EventIdentityInput, NativeItemKey, ScannedSourceCounts, SourceAnchor,
    SourceInventoryObservation, SourceKey, SourceObservation, TypedKey,
};
use ctx_history_index::{AppliedProviderRoot, IndexError, VerifiedIndex, WriterOptions};
use ctx_history_provider_gemini::GEMINI_CLI_SOURCE_FORMAT;
use tempfile::tempdir;

fn fixture_route(
    provider: CaptureProvider,
    source_format: &'static str,
    lineage: u8,
) -> SourceBackedRoute {
    fixture_route_with_body(
        provider,
        source_format,
        lineage,
        format!("{} body", provider.as_str()),
    )
}

fn fixture_route_with_body(
    provider: CaptureProvider,
    source_format: &'static str,
    lineage: u8,
    body: String,
) -> SourceBackedRoute {
    fixture_route_with_body_and_rejections(provider, source_format, lineage, body, 0)
}

fn fixture_route_with_body_and_rejections(
    provider: CaptureProvider,
    source_format: &'static str,
    lineage: u8,
    body: String,
    rejected_records: u64,
) -> SourceBackedRoute {
    let source = SourceKey::derive(
        provider.as_str(),
        source_format,
        "coordinator-test-v1",
        1,
        SourceAnchor::CatalogLineage([lineage; 32]),
    )
    .unwrap();
    let session_id = fixture_session_id(&source);
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &NativeItemKey::native_id("message", TypedKey::U64(1)).unwrap(),
        subrecord_selector: None,
    })
    .unwrap();
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        source.clone(),
        1,
        "message",
        "coordinator-test-v1",
        body,
    )
    .unwrap();
    record.provider_session_id = Some("session".to_owned());
    record.native_event_id = Some(TypedKey::U64(1));
    record.occurred_at_unix_ms = Some(1);
    record.role = Some("user".to_owned());
    record.agent_scope = Some(AgentScope::Primary);
    let revision_digest = [lineage.saturating_add(10); 32];
    let observation =
        SourceObservation::new(source.clone(), "fixture-revision", vec![lineage]).unwrap();
    let certificate = CertifiedSource::certify(
        observation.clone(),
        observation,
        "coordinator-test-v1",
        revision_digest,
        ScannedSourceCounts {
            complete_records: 1 + rejected_records,
            retained_records: 1,
            rejected_records,
            indexed_documents: 1,
            certified_bytes: 1,
            ..ScannedSourceCounts::default()
        },
    )
    .unwrap();
    let scan_certificate = certificate.clone();
    let revalidation_certificate = certificate;
    let owned_source = source;
    let driver = SourceBackedRouteDriver::new(
        move |sink| {
            sink.report_completed_bytes(1)
                .map_err(route_coordinator_error)?;
            sink.replace_source(scan_certificate.clone(), [record.clone()])
                .map_err(route_coordinator_error)
        },
        move |candidate| candidate.exact_descriptor_eq(&owned_source),
        move |target| match target {
            SourceBackedRevalidationTarget::Source(source) => source == &revalidation_certificate,
            SourceBackedRevalidationTarget::Deletion(_) => false,
        },
    );
    fixture_executable_route(provider, source_format, driver)
}

fn fail_route_before_scan(
    mut route: SourceBackedRoute,
    kind: SourceBackedRouteErrorKind,
) -> SourceBackedRoute {
    let original = route.take_driver_for_test().unwrap();
    let owns = Arc::clone(&original.owns_source);
    route.set_driver_for_test(Some(SourceBackedRouteDriver::new_fallible(
        move |_| Err(SourceBackedRouteError::new(kind, "fixture route failure")),
        move |source| owns(source),
        |_| Ok(false),
    )));
    route
}

fn empty_route(mut route: SourceBackedRoute) -> SourceBackedRoute {
    let original = route.take_driver_for_test().unwrap();
    let owns = Arc::clone(&original.owns_source);
    let revalidate = Arc::clone(&original.revalidate);
    route.set_driver_for_test(Some(SourceBackedRouteDriver::new_fallible(
        |_| Ok(()),
        move |source| owns(source),
        move |target| revalidate(target),
    )));
    route
}

fn explicit_route_at(mut route: SourceBackedRoute, path: PathBuf) -> SourceBackedRoute {
    let mut source = route.metadata().source.clone();
    source.path = path;
    SourceBackedRoute::explicit_manual(
        source,
        SourceBackedSelectorAuthority::ExplicitPath,
        route.take_driver_for_test().unwrap(),
    )
    .unwrap()
}

fn fail_route_at_final_revalidation(mut route: SourceBackedRoute) -> SourceBackedRoute {
    let mut driver = route.take_driver_for_test().unwrap();
    driver.revalidate = Arc::new(|_| Ok(false));
    route.set_driver_for_test(Some(driver));
    route
}

fn revisioned_receipt_route(revision: u8) -> (SourceBackedRoute, CertifiedSource) {
    let source = SourceKey::derive(
        CaptureProvider::Gemini.as_str(),
        GEMINI_CLI_SOURCE_FORMAT,
        "ordered-batch-test-v1",
        1,
        SourceAnchor::CatalogLineage([91; 32]),
    )
    .unwrap();
    let session_id = fixture_session_id(&source);
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &NativeItemKey::native_id("message", TypedKey::U64(1)).unwrap(),
        subrecord_selector: None,
    })
    .unwrap();
    let mut document = CoreRecord::new_selected(
        event_id,
        session_id,
        source.clone(),
        1,
        "message",
        "coordinator-test-v1",
        format!("receipt revision {revision}"),
    )
    .unwrap();
    document.provider_session_id = Some("receipt-race".to_owned());
    document.native_event_id = Some(TypedKey::U64(1));
    document.occurred_at_unix_ms = Some(i64::from(revision));
    document.role = Some("user".to_owned());
    document.agent_scope = Some(AgentScope::Primary);
    let observation =
        SourceObservation::new(source.clone(), "fixture-revision", vec![revision]).unwrap();
    let certificate = CertifiedSource::certify(
        observation.clone(),
        observation,
        "coordinator-test-v1",
        [revision; 32],
        ScannedSourceCounts {
            complete_records: 1,
            retained_records: 1,
            indexed_documents: 1,
            certified_bytes: 1,
            ..ScannedSourceCounts::default()
        },
    )
    .unwrap();
    let scan_certificate = certificate.clone();
    let revalidation_certificate = certificate.clone();
    let owned_source = source;
    let driver = SourceBackedRouteDriver::new(
        move |sink| {
            sink.replace_source(scan_certificate.clone(), [document.clone()])
                .map_err(route_coordinator_error)
        },
        move |candidate| candidate.exact_descriptor_eq(&owned_source),
        move |target| {
            matches!(
                target,
                SourceBackedRevalidationTarget::Source(source)
                    if source == &revalidation_certificate
            )
        },
    );
    (
        fixture_executable_route(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, driver),
        certificate,
    )
}

fn empty_inventory_registry() -> SourceBackedProviderRegistry {
    let observation = SourceInventoryObservation::new(
        CaptureProvider::Gemini.as_str(),
        "inventory-replay-root-v1",
        TypedKey::utf8("root").unwrap(),
        "inventory-replay-membership-v1",
        vec![0],
    )
    .unwrap();
    let inventory = CertifiedSourceInventory::certify(
        observation.clone(),
        observation,
        "inventory-replay-discovery-v1",
        Vec::new(),
    )
    .unwrap();
    let scan_inventory = inventory.clone();
    let revalidation_inventory = inventory;
    let driver = SourceBackedRouteDriver::new(
        move |sink| {
            sink.certify_complete_inventory(scan_inventory.clone())
                .map_err(route_coordinator_error)
        },
        |_| false,
        |_| false,
    )
    .with_complete_inventory_revalidation(move |candidate| candidate == &revalidation_inventory);
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(fixture_executable_route(
        CaptureProvider::Gemini,
        GEMINI_CLI_SOURCE_FORMAT,
        driver,
    ));
    registry
}

fn mark_fixture_route_as_configured_root(
    mut route: SourceBackedRoute,
    root_path: &Path,
    role: &'static str,
) -> SourceBackedRoute {
    let provenance = ProviderSourceRouteProvenance::ConfiguredRoot {
        root_id: "work".to_owned(),
        root_path: root_path.to_path_buf(),
        route_role: ProviderRouteRole::from_static(role),
        automatic_route_role: None,
    };
    route.metadata_for_test_mut().source.route_provenance = provenance.clone();
    for source in route.registration_sources_for_test_mut() {
        source.route_provenance = provenance.clone();
    }
    route
}

#[test]
fn terminal_failure_on_either_side_of_success_keeps_replacement_atomic() {
    for fail_first in [true, false] {
        let temp = tempdir().unwrap();
        let predecessor_definition = ProviderRootDefinition {
            id: "work".to_owned(),
            provider: CaptureProvider::Claude,
            path: temp.path().join("claude-predecessor"),
            group: Some("work".to_owned()),
            kind: None,
        };
        let predecessor = explicit_route_at(
            fixture_route_with_body(
                CaptureProvider::Claude,
                "claude_projects_jsonl_tree",
                90,
                "atomicterminalpredecessor".to_owned(),
            ),
            predecessor_definition.path.join("projects"),
        );
        let predecessor_id = predecessor.metadata().route_identity.clone().unwrap();
        let mut initial_registry = SourceBackedProviderRegistry::new();
        initial_registry.register(predecessor);
        initial_registry
            .set_applied_provider_roots(
                false,
                provider_source_config_digest(false, std::slice::from_ref(&predecessor_definition)),
                vec![AppliedProviderRoot::new(
                    predecessor_definition.clone(),
                    vec![predecessor_id.clone()],
                )
                .unwrap()],
            )
            .unwrap();
        refresh_source_backed_generation(temp.path(), &initial_registry, WriterOptions::default())
            .unwrap();

        let successor_definition = ProviderRootDefinition {
            id: "work".to_owned(),
            provider: CaptureProvider::Codex,
            path: temp.path().join("codex-successor"),
            group: Some("work".to_owned()),
            kind: None,
        };
        let first = mark_fixture_route_as_configured_root(
            fixture_route_with_body(
                CaptureProvider::Codex,
                "codex_session_jsonl_tree",
                91,
                "atomicterminalfirst".to_owned(),
            ),
            &successor_definition.path,
            "codex-sessions",
        );
        let second = mark_fixture_route_as_configured_root(
            fixture_route_with_body(
                CaptureProvider::Codex,
                "codex_history_jsonl",
                92,
                "atomicterminalsecond".to_owned(),
            ),
            &successor_definition.path,
            "codex-history",
        );
        let first_id = first.metadata().route_identity.clone().unwrap();
        let second_id = second.metadata().route_identity.clone().unwrap();
        let mut failed_registry = SourceBackedProviderRegistry::new();
        failed_registry.register(if fail_first {
            fail_route_at_final_revalidation(first.clone())
        } else {
            first.clone()
        });
        failed_registry.register(if fail_first {
            second.clone()
        } else {
            fail_route_at_final_revalidation(second.clone())
        });
        failed_registry
            .set_applied_provider_roots(
                false,
                provider_source_config_digest(false, std::slice::from_ref(&successor_definition)),
                vec![AppliedProviderRoot::new(
                    successor_definition.clone(),
                    vec![first_id.clone(), second_id.clone()],
                )
                .unwrap()],
            )
            .unwrap();
        failed_registry
            .retire_routes_after_success(&second_id, [predecessor_id.clone()])
            .unwrap();

        let failed = refresh_source_backed_generation(
            temp.path(),
            &failed_registry,
            WriterOptions::default(),
        )
        .unwrap();
        assert_eq!(failed.failed_routes.len(), 2);
        assert_eq!(
            failed
                .commit
                .manifest()
                .provider_root("work")
                .unwrap()
                .definition(),
            &predecessor_definition
        );
        assert!(failed
            .commit
            .manifest()
            .source_route(&predecessor_id)
            .is_some());
        assert!(failed.commit.manifest().source_route(&first_id).is_none());
        assert!(failed.commit.manifest().source_route(&second_id).is_none());
        let failed_index = ctx_history_index::VerifiedIndex::open_pinned(temp.path()).unwrap();
        assert!(search_event_candidates(&failed_index, "atomicterminalfirst", 8).is_empty());
        assert!(search_event_candidates(&failed_index, "atomicterminalsecond", 8).is_empty());
        drop(failed_index);

        let mut succeeded_registry = SourceBackedProviderRegistry::new();
        succeeded_registry.register(first);
        succeeded_registry.register(second);
        succeeded_registry
            .set_applied_provider_roots(
                false,
                provider_source_config_digest(false, std::slice::from_ref(&successor_definition)),
                vec![AppliedProviderRoot::new(
                    successor_definition.clone(),
                    vec![first_id, second_id.clone()],
                )
                .unwrap()],
            )
            .unwrap();
        succeeded_registry
            .retire_routes_after_success(&second_id, [predecessor_id.clone()])
            .unwrap();
        let succeeded = refresh_source_backed_generation(
            temp.path(),
            &succeeded_registry,
            WriterOptions::default(),
        )
        .unwrap();
        assert!(succeeded.failed_routes.is_empty());
        assert_eq!(
            succeeded
                .commit
                .manifest()
                .provider_root("work")
                .unwrap()
                .definition(),
            &successor_definition
        );
        assert!(succeeded
            .commit
            .manifest()
            .source_route(&predecessor_id)
            .is_none());
    }
}

#[test]
fn certified_missing_route_certifies_a_complete_empty_inventory() {
    let temp = tempdir().unwrap();
    let mut source = fixture_provider_source_at(
        CaptureProvider::Gemini,
        GEMINI_CLI_SOURCE_FORMAT,
        ProviderImportSupport::Native,
        temp.path().join("missing-history.jsonl"),
    );
    source.status = ProviderSourceStatus::Missing;
    source.exists = false;
    let route = SourceBackedRoute::certified_missing(
        source,
        SourceBackedSelectorAuthority::DiscoveredWinner,
    )
    .unwrap();
    let route_identity = route.metadata().route_identity.clone().unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(route);

    let refresh =
        refresh_source_backed_generation(temp.path(), &registry, WriterOptions::default()).unwrap();

    assert!(refresh.sources.is_empty());
    assert_eq!(refresh.successful_route_outcomes.len(), 1);
    assert_eq!(
        refresh.successful_route_outcomes[0].route_identity,
        route_identity
    );
    assert_eq!(refresh.complete_inventory_route_ids, vec![route_identity]);
}

#[test]
fn warm_missing_route_in_grace_remains_usable_when_a_new_cold_route_fails() {
    let provider = CaptureProvider::Gemini;
    let format = GEMINI_CLI_SOURCE_FORMAT;
    let present = fixture_route(provider, format, 16);
    let route_id = present.metadata().route_identity.clone().unwrap();
    let mut initial_registry = SourceBackedProviderRegistry::new();
    initial_registry.register(present);
    let temp = tempdir().unwrap();
    let initial =
        refresh_source_backed_generation(temp.path(), &initial_registry, WriterOptions::default())
            .unwrap();

    let mut missing_source =
        fixture_provider_source(provider, format, ProviderImportSupport::Native);
    missing_source.status = ProviderSourceStatus::Missing;
    missing_source.exists = false;
    let missing = SourceBackedRoute::certified_missing(
        missing_source,
        SourceBackedSelectorAuthority::DiscoveredWinner,
    )
    .unwrap();
    assert_eq!(missing.metadata().route_identity.as_ref(), Some(&route_id));
    let failed = fail_route_before_scan(
        fixture_route(CaptureProvider::Mux, "mux_session_jsonl_tree", 17),
        SourceBackedRouteErrorKind::Unavailable,
    );
    let failed_id = failed.metadata().route_identity.clone().unwrap();
    let mut refresh_registry = SourceBackedProviderRegistry::new();
    refresh_registry.register(missing);
    refresh_registry.register(failed);

    let refresh =
        refresh_source_backed_generation(temp.path(), &refresh_registry, WriterOptions::default())
            .unwrap();
    assert_eq!(refresh.failed_routes.len(), 1);
    assert_eq!(refresh.failed_routes[0].route_identity, failed_id);
    assert!(!refresh.failed_routes[0].carried_forward);
    assert_eq!(refresh.sources, initial.sources);
    let retained_route = refresh.commit.manifest().source_route(&route_id).unwrap();
    assert_eq!(
        retained_route.sources(),
        initial
            .commit
            .manifest()
            .source_route(&route_id)
            .unwrap()
            .sources()
    );
    assert_eq!(
        retained_route
            .missing_state()
            .unwrap()
            .consecutive_missing()
            .get(),
        1
    );
}

#[test]
fn selected_route_refresh_carries_unselected_route_and_reports_exact_noop_success() {
    let first = fixture_route(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 21);
    let second = fixture_route(CaptureProvider::Mux, "mux_session_jsonl_tree", 22);
    let first_id = first.metadata().route_identity.clone().unwrap();
    let second_id = second.metadata().route_identity.clone().unwrap();
    let second_scans = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let original_second = second.driver_for_test().unwrap().clone();
    let owns_second = Arc::clone(&second.driver_for_test().unwrap().owns_source);
    let revalidate_second = Arc::clone(&second.driver_for_test().unwrap().revalidate);
    let scans = Arc::clone(&second_scans);
    let mut second = second;
    second.set_driver_for_test(Some(SourceBackedRouteDriver::new_fallible(
        move |sink| {
            scans.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            (original_second.scan)(sink)
        },
        move |source| owns_second(source),
        move |target| revalidate_second(target),
    )));
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(first.clone());
    registry.register(second);
    let temp = tempdir().unwrap();
    let initial =
        refresh_source_backed_generation(temp.path(), &registry, WriterOptions::default()).unwrap();
    assert_eq!(second_scans.load(std::sync::atomic::Ordering::SeqCst), 1);
    let retained_second = initial
        .commit
        .manifest()
        .source_route(&second_id)
        .unwrap()
        .clone();

    let mut selected_registry = SourceBackedProviderRegistry::new();
    selected_registry.register(first);
    let selected = refresh_source_backed_generation_for_routes(
        temp.path(),
        &selected_registry,
        WriterOptions::default(),
        [first_id.clone()],
    )
    .unwrap();
    assert_eq!(selected.commit.generation_id, initial.commit.generation_id);
    assert_eq!(selected.successful_route_ids, vec![first_id]);
    assert!(selected.failed_routes.is_empty());
    assert_eq!(
        selected.carried_unselected_route_ids,
        vec![second_id.clone()]
    );
    assert_eq!(
        selected.commit.manifest().source_route(&second_id),
        Some(&retained_second)
    );
    assert_eq!(second_scans.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn successful_replacement_does_not_report_the_retired_route_as_carried() {
    let temp = tempdir().unwrap();
    let retired = explicit_route_at(
        fixture_route(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 71),
        temp.path().join("retired.jsonl"),
    );
    let retired_id = retired.metadata().route_identity.clone().unwrap();
    let mut initial_registry = SourceBackedProviderRegistry::new();
    initial_registry.register(retired);
    refresh_source_backed_generation(temp.path(), &initial_registry, WriterOptions::default())
        .unwrap();

    let replacement = empty_route(explicit_route_at(
        fixture_route(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 72),
        temp.path().join("replacement.jsonl"),
    ));
    let replacement_id = replacement.metadata().route_identity.clone().unwrap();
    let mut replacement_registry = SourceBackedProviderRegistry::new();
    replacement_registry.register(replacement);
    replacement_registry
        .retire_routes_after_success(&replacement_id, [retired_id.clone()])
        .unwrap();

    let receipt = refresh_source_backed_generation_for_routes(
        temp.path(),
        &replacement_registry,
        WriterOptions::default(),
        [replacement_id.clone()],
    )
    .unwrap();

    assert!(receipt.carried_unselected_route_ids.is_empty());
    assert!(receipt
        .commit
        .manifest()
        .source_route(&retired_id)
        .is_none());
    assert!(receipt
        .commit
        .manifest()
        .source_route(&replacement_id)
        .is_some());
}

#[test]
fn automatic_retirement_candidates_activate_only_for_exhaustive_refresh() {
    let temp = tempdir().unwrap();
    let retired = fixture_route(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 76);
    let replacement = fixture_route(CaptureProvider::Mux, "mux_session_jsonl_tree", 77);
    let retired_id = retired.metadata().route_identity.clone().unwrap();
    let replacement_id = replacement.metadata().route_identity.clone().unwrap();
    let mut initial_registry = SourceBackedProviderRegistry::new();
    initial_registry.register(retired);
    initial_registry.register(replacement.clone());
    refresh_source_backed_generation(temp.path(), &initial_registry, WriterOptions::default())
        .unwrap();

    let mut replacement_registry = SourceBackedProviderRegistry::new();
    replacement_registry.register(replacement);
    replacement_registry
        .retire_automatic_routes_after_success(&replacement_id, [retired_id.clone()])
        .unwrap();

    let exact = refresh_source_backed_generation_for_routes(
        temp.path(),
        &replacement_registry,
        WriterOptions::default(),
        [replacement_id.clone()],
    )
    .unwrap();
    assert_eq!(exact.carried_unselected_route_ids, vec![retired_id.clone()]);
    assert!(exact.commit.manifest().source_route(&retired_id).is_some());

    let exhaustive = refresh_source_backed_generation(
        temp.path(),
        &replacement_registry,
        WriterOptions::default(),
    )
    .unwrap();
    assert!(exhaustive.carried_unselected_route_ids.is_empty());
    assert!(exhaustive
        .commit
        .manifest()
        .source_route(&retired_id)
        .is_none());
    assert!(exhaustive
        .commit
        .manifest()
        .source_route(&replacement_id)
        .is_some());
}

#[test]
fn logical_all_publication_withdraws_removed_root_without_deleting_history() {
    let temp = tempdir().unwrap();
    let personal = explicit_route_at(
        fixture_route_with_body(
            CaptureProvider::Claude,
            "claude_projects_jsonl_tree",
            78,
            "personalonly".to_owned(),
        ),
        temp.path().join("claude-personal/projects"),
    );
    let work = explicit_route_at(
        fixture_route_with_body(
            CaptureProvider::Claude,
            "claude_projects_jsonl_tree",
            79,
            "workonly".to_owned(),
        ),
        temp.path().join("claude-work/projects"),
    );
    let personal_id = personal.metadata().route_identity.clone().unwrap();
    let work_id = work.metadata().route_identity.clone().unwrap();
    let personal_definition = ctx_history_capture_model::ProviderRootDefinition {
        id: "personal".to_owned(),
        provider: CaptureProvider::Claude,
        path: temp.path().join("claude-personal"),
        group: Some("personal".to_owned()),
        kind: None,
    };
    let work_definition = ctx_history_capture_model::ProviderRootDefinition {
        id: "work".to_owned(),
        provider: CaptureProvider::Claude,
        path: temp.path().join("claude-work"),
        group: Some("work".to_owned()),
        kind: None,
    };

    let mut initial_registry = SourceBackedProviderRegistry::new();
    initial_registry.register(personal.clone());
    initial_registry.register(work);
    let initial_definitions = vec![personal_definition.clone(), work_definition.clone()];
    initial_registry
        .set_applied_provider_roots(
            true,
            provider_source_config_digest(true, &initial_definitions),
            vec![
                AppliedProviderRoot::new(personal_definition.clone(), vec![personal_id.clone()])
                    .unwrap(),
                AppliedProviderRoot::new(work_definition, vec![work_id.clone()]).unwrap(),
            ],
        )
        .unwrap();
    refresh_source_backed_generation(temp.path(), &initial_registry, WriterOptions::default())
        .unwrap();
    assert_eq!(
        VerifiedIndex::open_pinned(temp.path())
            .unwrap()
            .manifest()
            .sources
            .len(),
        2
    );

    let mut current_registry = SourceBackedProviderRegistry::new();
    current_registry.register(personal);
    current_registry
        .set_applied_provider_roots(
            true,
            provider_source_config_digest(true, std::slice::from_ref(&personal_definition)),
            vec![AppliedProviderRoot::new(personal_definition, vec![personal_id.clone()]).unwrap()],
        )
        .unwrap();
    current_registry.set_root_withdrawals([work_id.clone()]);

    let receipt = SourceBackedRefreshExecutor::new(current_registry, WriterOptions::default())
        .with_base_route_controls(BTreeMap::from([(work_id.clone(), b"work".to_vec())]))
        .refresh_physical_scope_with_detailed_progress_publication_metadata_reconciliation_and_worksets(
            temp.path(),
            SourceBackedRefreshScope::exact([personal_id.clone()]),
            SourceBackedRefreshScope::All,
            SourceBackedReconciliationDemand::Exhaustive,
            BTreeMap::new(),
            |_| Ok(()),
            |_| Ok(Vec::new()),
        )
        .unwrap();
    assert!(receipt.route_controls.is_empty());

    let published = VerifiedIndex::open_pinned(temp.path()).unwrap();
    assert_eq!(published.manifest().sources.len(), 2);
    assert!(published.manifest().source_route(&work_id).is_some());
    assert!(published.manifest().source_route(&personal_id).is_some());
    assert_eq!(search_event_candidates(&published, "workonly", 8).len(), 1);
    assert_eq!(published.manifest().provider_roots().len(), 1);
    assert_eq!(
        published.manifest().provider_roots()[0].definition().id,
        "personal"
    );
}

#[test]
fn cold_configured_root_scan_failure_does_not_block_healthy_peer_publication() {
    let temp = tempdir().unwrap();
    let personal = explicit_route_at(
        fixture_route_with_body(
            CaptureProvider::Claude,
            "claude_projects_jsonl_tree",
            80,
            "healthyconfiguredroot".to_owned(),
        ),
        temp.path().join("claude-personal/projects"),
    );
    let work = fail_route_before_scan(
        explicit_route_at(
            fixture_route_with_body(
                CaptureProvider::Claude,
                "claude_projects_jsonl_tree",
                81,
                "failedconfiguredroot".to_owned(),
            ),
            temp.path().join("claude-work/projects"),
        ),
        SourceBackedRouteErrorKind::SourceChanged,
    );
    let personal_id = personal.metadata().route_identity.clone().unwrap();
    let work_id = work.metadata().route_identity.clone().unwrap();
    let definitions = vec![
        ctx_history_capture_model::ProviderRootDefinition {
            id: "personal".to_owned(),
            provider: CaptureProvider::Claude,
            path: temp.path().join("claude-personal"),
            group: Some("personal".to_owned()),
            kind: None,
        },
        ctx_history_capture_model::ProviderRootDefinition {
            id: "work".to_owned(),
            provider: CaptureProvider::Claude,
            path: temp.path().join("claude-work"),
            group: Some("work".to_owned()),
            kind: None,
        },
    ];
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(personal);
    registry.register(work);
    registry
        .set_applied_provider_roots(
            false,
            provider_source_config_digest(false, &definitions),
            vec![
                AppliedProviderRoot::new(definitions[0].clone(), vec![personal_id.clone()])
                    .unwrap(),
                AppliedProviderRoot::new(definitions[1].clone(), vec![work_id.clone()]).unwrap(),
            ],
        )
        .unwrap();

    let receipt =
        refresh_source_backed_generation(temp.path(), &registry, WriterOptions::default()).unwrap();

    assert_eq!(receipt.successful_route_ids, vec![personal_id.clone()]);
    assert!(matches!(
        receipt.failed_routes.as_slice(),
        [failure]
            if failure.route_identity == work_id
                && failure.class == SourceBackedSourceFailureClass::SourceChanged
                && !failure.carried_forward
    ));
    assert!(receipt
        .commit
        .manifest()
        .source_route(&personal_id)
        .is_some());
    assert!(receipt.commit.manifest().source_route(&work_id).is_none());
    let personal_root = receipt.commit.manifest().provider_root("personal").unwrap();
    let work_root = receipt.commit.manifest().provider_root("work").unwrap();
    assert_eq!(personal_root.routes(), std::slice::from_ref(&personal_id));
    assert!(work_root.routes().is_empty());
    assert_eq!(receipt.commit.indexed_documents, 1);
}

#[test]
fn empty_replacement_cannot_hide_a_cold_route_failure_behind_retired_content() {
    let temp = tempdir().unwrap();
    let retired = explicit_route_at(
        fixture_route(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 73),
        temp.path().join("retired.jsonl"),
    );
    let retired_id = retired.metadata().route_identity.clone().unwrap();
    let mut initial_registry = SourceBackedProviderRegistry::new();
    initial_registry.register(retired);
    let initial =
        refresh_source_backed_generation(temp.path(), &initial_registry, WriterOptions::default())
            .unwrap();

    let replacement = empty_route(explicit_route_at(
        fixture_route(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 74),
        temp.path().join("replacement.jsonl"),
    ));
    let replacement_id = replacement.metadata().route_identity.clone().unwrap();
    let failed = fail_route_before_scan(
        fixture_route(CaptureProvider::Mux, "mux_session_jsonl_tree", 75),
        SourceBackedRouteErrorKind::SourceChanged,
    );
    let failed_id = failed.metadata().route_identity.clone().unwrap();
    let mut replacement_registry = SourceBackedProviderRegistry::new();
    replacement_registry.register(replacement);
    replacement_registry.register(failed);
    replacement_registry
        .retire_routes_after_success(&replacement_id, [retired_id])
        .unwrap();

    let error = refresh_source_backed_generation_for_routes(
        temp.path(),
        &replacement_registry,
        WriterOptions::default(),
        [replacement_id, failed_id.clone()],
    )
    .expect_err("retired content is not usable carried content");

    assert!(matches!(
        error,
        SourceBackedCoordinatorError::NoUsableSourceRoutes { failed_routes }
            if failed_routes.len() == 1 && failed_routes[0].route_identity == failed_id
    ));
    assert_eq!(
        VerifiedIndex::open_pinned(temp.path())
            .unwrap()
            .generation_id(),
        initial.commit.generation_id
    );
}

#[test]
fn selected_clean_route_completion_ignores_carried_unselected_rejections() {
    let clean = fixture_route(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 61);
    let rejected = fixture_route_with_body_and_rejections(
        CaptureProvider::Mux,
        "mux_session_jsonl_tree",
        62,
        "retained peer".to_owned(),
        1,
    );
    let clean_id = clean.metadata().route_identity.clone().unwrap();
    let rejected_id = rejected.metadata().route_identity.clone().unwrap();
    let mut initial_registry = SourceBackedProviderRegistry::new();
    initial_registry.register(clean.clone());
    initial_registry.register(rejected);
    let temp = tempdir().unwrap();
    let initial =
        refresh_source_backed_generation(temp.path(), &initial_registry, WriterOptions::default())
            .unwrap();
    assert_eq!(
        initial.record_completion(),
        SourceBackedRecordCompletion::CompletedWithRejections
    );

    let mut selected_registry = SourceBackedProviderRegistry::new();
    selected_registry.register(clean);
    let selected = refresh_source_backed_generation_for_routes(
        temp.path(),
        &selected_registry,
        WriterOptions::default(),
        [clean_id],
    )
    .unwrap();

    assert_eq!(
        selected.record_completion(),
        SourceBackedRecordCompletion::Completed
    );
    assert_eq!(selected.carried_unselected_route_ids, vec![rejected_id]);
}

#[test]
fn selected_failed_route_reports_exact_identity_and_carries_the_whole_base() {
    let first = fixture_route(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 23);
    let second = fixture_route(CaptureProvider::Mux, "mux_session_jsonl_tree", 24);
    let first_id = first.metadata().route_identity.clone().unwrap();
    let second_id = second.metadata().route_identity.clone().unwrap();
    let mut initial_registry = SourceBackedProviderRegistry::new();
    initial_registry.register(first);
    initial_registry.register(second.clone());
    let temp = tempdir().unwrap();
    let initial =
        refresh_source_backed_generation(temp.path(), &initial_registry, WriterOptions::default())
            .unwrap();

    let mut selected_registry = SourceBackedProviderRegistry::new();
    selected_registry.register(fail_route_before_scan(
        second,
        SourceBackedRouteErrorKind::SourceChanged,
    ));
    let selected = refresh_source_backed_generation_for_routes(
        temp.path(),
        &selected_registry,
        WriterOptions::default(),
        [second_id.clone()],
    )
    .unwrap();
    assert_eq!(selected.commit.generation_id, initial.commit.generation_id);
    assert!(selected.successful_route_ids.is_empty());
    assert_eq!(selected.failed_routes.len(), 1);
    assert_eq!(selected.failed_routes[0].route_identity, second_id.clone());
    assert!(selected.failed_routes[0].carried_forward);
    assert_eq!(
        selected.carried_unselected_route_ids,
        vec![first_id.clone()]
    );
    assert_eq!(selected.carried_failed_route_ids, vec![second_id]);
    assert_eq!(
        selected.commit.manifest().source_route(&first_id),
        initial.commit.manifest().source_route(&first_id)
    );
    assert_eq!(selected.sources, initial.sources);
    assert_eq!(
        selected.commit.manifest().source_routes(),
        initial.commit.manifest().source_routes()
    );
}

#[test]
fn automatic_whole_route_missing_grace_resets_and_unknown_aborts_atomically() {
    let temp = tempdir().unwrap();
    let provider = CaptureProvider::Gemini;
    let format = GEMINI_CLI_SOURCE_FORMAT;

    let mut present = SourceBackedProviderRegistry::new();
    present.register(fixture_route(provider, format, 61));
    let initial =
        refresh_source_backed_generation(temp.path(), &present, WriterOptions::default()).unwrap();
    let route_id = initial.commit.manifest().source_routes()[0]
        .route_identity()
        .clone();

    let missing_registry = || {
        let mut source = fixture_provider_source(provider, format, ProviderImportSupport::Native);
        source.status = ProviderSourceStatus::Missing;
        source.exists = false;
        let mut registry = SourceBackedProviderRegistry::new();
        registry.register(
            SourceBackedRoute::certified_missing(
                source,
                SourceBackedSelectorAuthority::DiscoveredWinner,
            )
            .unwrap(),
        );
        registry
    };

    for expected in 1..automatic_route_deletion_missing_observations_for_test() {
        let missing = refresh_source_backed_generation(
            temp.path(),
            &missing_registry(),
            WriterOptions::default(),
        )
        .unwrap();
        assert_eq!(missing.sources.len(), 1);
        assert_eq!(
            missing
                .commit
                .manifest()
                .source_route(&route_id)
                .unwrap()
                .missing_state()
                .unwrap()
                .consecutive_missing()
                .get(),
            expected
        );
    }

    let retained_generation = VerifiedIndex::open_pinned(temp.path())
        .unwrap()
        .generation_id()
        .to_owned();
    let mut unknown_source =
        fixture_provider_source(provider, format, ProviderImportSupport::Native);
    unknown_source.status = ProviderSourceStatus::Unknown;
    let mut unknown = SourceBackedProviderRegistry::new();
    unknown.register(SourceBackedRoute::unsupported(
        unknown_source,
        "unknown test route",
    ));
    assert!(matches!(
        refresh_source_backed_generation(temp.path(), &unknown, WriterOptions::default()),
        Err(SourceBackedCoordinatorError::UnavailableRoute { .. })
    ));
    assert_eq!(
        VerifiedIndex::open_pinned(temp.path())
            .unwrap()
            .generation_id(),
        retained_generation
    );

    let reappeared =
        refresh_source_backed_generation(temp.path(), &present, WriterOptions::default()).unwrap();
    assert!(reappeared
        .commit
        .manifest()
        .source_route(&route_id)
        .unwrap()
        .missing_state()
        .is_none());

    for expected in 1..automatic_route_deletion_missing_observations_for_test() {
        let missing = refresh_source_backed_generation(
            temp.path(),
            &missing_registry(),
            WriterOptions::default(),
        )
        .unwrap();
        assert_eq!(
            missing
                .commit
                .manifest()
                .source_route(&route_id)
                .unwrap()
                .missing_state()
                .unwrap()
                .consecutive_missing()
                .get(),
            expected
        );
    }
    let deleted = refresh_source_backed_generation(
        temp.path(),
        &missing_registry(),
        WriterOptions::default(),
    )
    .unwrap();
    assert!(deleted.sources.is_empty());
    assert!(deleted.commit.manifest().source_routes().is_empty());
    assert_eq!(
        VerifiedIndex::open_pinned(temp.path())
            .unwrap()
            .document_count(),
        0
    );
}

fn certified_missing_registry_at(
    path: impl Into<PathBuf>,
) -> (SourceBackedProviderRegistry, SourceRouteIdentity) {
    let mut source = fixture_provider_source_at(
        CaptureProvider::Gemini,
        GEMINI_CLI_SOURCE_FORMAT,
        ProviderImportSupport::Native,
        path,
    );
    source.status = ProviderSourceStatus::Missing;
    source.exists = false;
    let route = SourceBackedRoute::certified_missing(
        source,
        SourceBackedSelectorAuthority::DiscoveredWinner,
    )
    .unwrap();
    let route_identity = route.metadata().route_identity.clone().unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(route);
    (registry, route_identity)
}

#[test]
fn cold_certified_missing_route_reappearance_at_precommit_cannot_publish_empty() {
    let temp = tempdir().unwrap();
    let reappearing_path = temp.path().join("cold-reappearing-history.jsonl");
    let (registry, route_identity) = certified_missing_registry_at(reappearing_path.clone());

    install_before_source_backed_commit_hook_for_test(move || {
        fs::write(reappearing_path, b"reappeared before cold commit\n").unwrap();
    });
    let error = refresh_source_backed_generation(temp.path(), &registry, WriterOptions::default())
        .expect_err("cold missing route must be revalidated at the publication fence");

    assert!(matches!(
        error,
        SourceBackedCoordinatorError::Index(IndexError::SourceInvalidated(ref invalidated))
            if invalidated == route_identity.as_str()
    ));
    assert!(matches!(
        VerifiedIndex::open_pinned(temp.path()),
        Err(IndexError::MissingActiveGenerationPointer)
    ));
}

#[test]
fn previously_empty_certified_missing_route_reappearance_at_precommit_retains_base() {
    let temp = tempdir().unwrap();
    let empty_registry = empty_inventory_registry();
    let initial =
        refresh_source_backed_generation(temp.path(), &empty_registry, WriterOptions::default())
            .unwrap();
    assert!(initial.sources.is_empty());
    let [empty_route] = initial.commit.manifest().source_routes() else {
        panic!("one previously empty route expected");
    };
    assert!(empty_route.sources().is_empty());
    let route_identity = empty_route.route_identity().clone();

    let reappearing_path = temp.path().join("empty-reappearing-history.jsonl");
    let (missing_registry, missing_identity) =
        certified_missing_registry_at(reappearing_path.clone());
    assert_eq!(missing_identity, route_identity);
    install_before_source_backed_commit_hook_for_test(move || {
        fs::write(reappearing_path, b"reappeared before empty commit\n").unwrap();
    });
    let error =
        refresh_source_backed_generation(temp.path(), &missing_registry, WriterOptions::default())
            .expect_err(
                "previously empty missing route must be revalidated at the publication fence",
            );

    assert!(matches!(
        error,
        SourceBackedCoordinatorError::Index(IndexError::SourceInvalidated(ref invalidated))
            if invalidated == route_identity.as_str()
    ));
    let retained = VerifiedIndex::open_pinned(temp.path()).unwrap();
    assert_eq!(retained.generation_id(), initial.commit.generation_id);
    let retained_route = retained.manifest().source_route(&route_identity).unwrap();
    assert!(retained_route.sources().is_empty());
    assert_eq!(retained.document_count(), 0);
}

#[test]
fn certified_missing_route_reappearance_at_precommit_cannot_delete_the_route() {
    let temp = tempdir().unwrap();
    let provider = CaptureProvider::Gemini;
    let format = GEMINI_CLI_SOURCE_FORMAT;
    let reappearing_path = temp.path().join("reappearing-history.jsonl");

    let mut present = SourceBackedProviderRegistry::new();
    present.register(fixture_route(provider, format, 62));
    let initial =
        refresh_source_backed_generation(temp.path(), &present, WriterOptions::default()).unwrap();
    let initial_generation = initial.commit.generation_id.clone();
    let route_id = initial.commit.manifest().source_routes()[0]
        .route_identity()
        .clone();

    let missing_registry = || {
        let mut source = fixture_provider_source_at(
            provider,
            format,
            ProviderImportSupport::Native,
            reappearing_path.clone(),
        );
        source.status = ProviderSourceStatus::Missing;
        source.exists = false;
        let mut registry = SourceBackedProviderRegistry::new();
        registry.register(
            SourceBackedRoute::certified_missing(
                source,
                SourceBackedSelectorAuthority::DiscoveredWinner,
            )
            .unwrap(),
        );
        registry
    };

    for _ in 1..automatic_route_deletion_missing_observations_for_test() {
        refresh_source_backed_generation(
            temp.path(),
            &missing_registry(),
            WriterOptions::default(),
        )
        .unwrap();
    }
    let retained_generation = VerifiedIndex::open_pinned(temp.path())
        .unwrap()
        .generation_id()
        .to_owned();
    assert_ne!(retained_generation, initial_generation);

    let hook_path = reappearing_path.clone();
    install_before_source_backed_commit_hook_for_test(move || {
        fs::write(hook_path, b"reappeared before commit\n").unwrap();
    });
    let error = refresh_source_backed_generation(
        temp.path(),
        &missing_registry(),
        WriterOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        SourceBackedCoordinatorError::Index(IndexError::SourceInvalidated(ref invalidated))
            if invalidated == route_id.as_str()
    ));

    let retained = VerifiedIndex::open_pinned(temp.path()).unwrap();
    assert_eq!(retained.generation_id(), retained_generation);
    assert!(retained.manifest().source_route(&route_id).is_some());
    assert_eq!(retained.document_count(), 1);
}

#[test]
fn relocated_route_rechecks_old_path_absence_at_terminal_publication() {
    let temp = tempdir().unwrap();
    let old_path = temp.path().join("relocation-old.jsonl");
    let new_path = temp.path().join("relocation-new.jsonl");
    let mut fixture = fixture_route(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 63);
    let mut old_source = fixture.metadata().source.clone();
    old_source.path = old_path.clone();
    let old_route = SourceBackedRoute::explicit_manual(
        old_source,
        SourceBackedSelectorAuthority::ExplicitPath,
        fixture.driver_for_test().unwrap().clone(),
    )
    .unwrap();
    let preserved = old_route.metadata().route_identity.clone().unwrap();
    let mut initial_registry = SourceBackedProviderRegistry::new();
    initial_registry.register(old_route);
    let initial =
        refresh_source_backed_generation(temp.path(), &initial_registry, WriterOptions::default())
            .unwrap();

    let mut relocated_source = fixture.metadata().source.clone();
    relocated_source.path = new_path;
    let relocated_route = SourceBackedRoute::explicit_manual(
        relocated_source,
        SourceBackedSelectorAuthority::ExplicitPath,
        fixture.take_driver_for_test().unwrap(),
    )
    .unwrap();
    let constructed = relocated_route.metadata().route_identity.clone().unwrap();
    let mut relocated_registry = SourceBackedProviderRegistry::new();
    relocated_registry.register(relocated_route);
    relocated_registry
        .preserve_explicit_route_identity(&constructed, preserved.clone(), &old_path)
        .unwrap();

    let reappearing = old_path.clone();
    install_before_source_backed_commit_hook_for_test(move || {
        fs::write(reappearing, b"old authority reappeared\n").unwrap();
    });
    let error = refresh_source_backed_generation(
        temp.path(),
        &relocated_registry,
        WriterOptions::default(),
    )
    .expect_err("terminal relocation fence must reject old-path reappearance");
    assert!(matches!(
        error,
        SourceBackedCoordinatorError::Index(IndexError::SourceInvalidated(ref invalidated))
            if invalidated == preserved.as_str()
    ));
    let retained = VerifiedIndex::open_pinned(temp.path()).unwrap();
    assert_eq!(retained.generation_id(), initial.commit.generation_id);
    assert!(retained.manifest().source_route(&preserved).is_some());
    assert_eq!(retained.document_count(), 1);
}

#[test]
fn mutating_refresh_rejects_an_unclaimed_base_source_from_the_same_family() {
    let mut initial_registry = SourceBackedProviderRegistry::new();
    initial_registry.register(fixture_route(
        CaptureProvider::Gemini,
        GEMINI_CLI_SOURCE_FORMAT,
        40,
    ));
    let temp = tempdir().unwrap();
    let initial =
        refresh_source_backed_generation(temp.path(), &initial_registry, WriterOptions::default())
            .unwrap();
    let initial_generation = initial.commit.generation_id.clone();
    let initial_source = initial.sources[0].observation().source().clone();

    let mut incomplete_registry = SourceBackedProviderRegistry::new();
    incomplete_registry.register(fixture_route(
        CaptureProvider::Gemini,
        GEMINI_CLI_SOURCE_FORMAT,
        41,
    ));
    let incomplete_route = incomplete_registry
        .routes()
        .next()
        .unwrap()
        .route_identity
        .clone()
        .unwrap();
    let error = refresh_source_backed_generation(
        temp.path(),
        &incomplete_registry,
        WriterOptions::default(),
    )
    .unwrap_err();

    assert!(matches!(
        error,
        SourceBackedCoordinatorError::UnclaimedBaseSource {
            ref source_id,
            ref route_identity,
            ..
        } if source_id == &initial_source.identity().to_string()
            && route_identity == &incomplete_route
    ));
    let retained = VerifiedIndex::open_pinned(temp.path()).unwrap();
    assert_eq!(retained.generation_id(), initial_generation);
    assert_eq!(retained.manifest().sources, initial.sources);
}

#[test]
fn cross_route_duplicate_source_ownership_remains_rejected() {
    let mut registry = SourceBackedProviderRegistry::new();
    let automatic = fixture_route(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 42);
    let explicit = SourceBackedRoute::explicit_manual(
        automatic.metadata().source.clone(),
        SourceBackedSelectorAuthority::ExplicitPath,
        automatic.driver_for_test().unwrap().clone(),
    )
    .unwrap();
    registry.register(automatic);
    registry.register(explicit);
    let temp = tempdir().unwrap();

    assert!(matches!(
        refresh_source_backed_generation(temp.path(), &registry, WriterOptions::default()),
        Err(SourceBackedCoordinatorError::RouteScan {
            source: SourceBackedRouteError {
                kind: SourceBackedRouteErrorKind::Internal,
                detail,
            },
            ..
        }) if detail.contains("staged by more than one provider route")
    ));
}

#[test]
fn refresh_receipt_stays_bound_to_commit_when_current_generation_advances() {
    let (g1_route, g1_certificate) = revisioned_receipt_route(1);
    let (g2_route, g2_certificate) = revisioned_receipt_route(2);
    let mut g1_registry = SourceBackedProviderRegistry::new();
    g1_registry.register(g1_route);
    let mut g2_registry = SourceBackedProviderRegistry::new();
    g2_registry.register(g2_route);

    let temp = tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let (g2_sender, g2_receiver) = std::sync::mpsc::sync_channel(1);
    let (g1, g2) = std::thread::scope(|scope| {
        let g2_barrier = Arc::clone(&barrier);
        let g2_root = root.clone();
        scope.spawn(move || {
            g2_barrier.wait();
            let receipt =
                refresh_source_backed_generation(&g2_root, &g2_registry, WriterOptions::default())
                    .unwrap();
            g2_sender.send(receipt).unwrap();
        });

        let mut g2 = None;
        let g1 = refresh_source_backed_generation_with_progress(
            &root,
            &g1_registry,
            WriterOptions::default(),
            |progress| {
                if progress.phase == "committed" {
                    barrier.wait();
                    g2 = Some(
                        g2_receiver
                            .recv_timeout(Duration::from_secs(10))
                            .expect("G2 did not publish while G1 was between commit and receipt"),
                    );
                }
                Ok(())
            },
        )
        .unwrap();
        (g1, g2.expect("the committed progress barrier did not run"))
    });

    assert_ne!(g1.commit.generation_id, g2.commit.generation_id);
    assert_eq!(g1.commit.indexed_documents, g2.commit.indexed_documents);
    assert_eq!(g1.commit.certified_sources, g2.commit.certified_sources);
    assert_eq!(
        g1.commit.certified_source_bytes,
        g2.commit.certified_source_bytes
    );
    assert_eq!(g1.sources, vec![g1_certificate]);
    assert_eq!(g2.sources, vec![g2_certificate]);
    assert_eq!(g1.sources, g1.commit.manifest().sources);
    assert_eq!(g2.sources, g2.commit.manifest().sources);
    assert_ne!(
        g1.commit.manifest().generation_id().unwrap(),
        g2.commit.manifest().generation_id().unwrap(),
        "each receipt must retain its own logical manifest after a later publication"
    );
    assert!(g1.removals.is_empty());
    assert!(g2.removals.is_empty());
    assert_eq!(
        VerifiedIndex::open_pinned(root).unwrap().generation_id(),
        g2.commit.generation_id
    );
}
