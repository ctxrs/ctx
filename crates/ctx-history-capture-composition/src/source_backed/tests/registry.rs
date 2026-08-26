use super::*;
use ctx_history_providers_task_docs::{
    CLINE_SDK_SOURCE_FORMAT, CLINE_TASK_JSON_SOURCE_FORMAT, CONTINUE_CLI_SOURCE_FORMAT,
};

mod inventory_replay;
mod progress;

use inventory_replay::{inventory_replay_registry, revisioned_receipt_route};

#[test]
fn heterogeneous_routes_publish_one_core_generation() {
    let gemini = fixture_route(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 1);
    let mux = fixture_route(CaptureProvider::Mux, "mux_session_jsonl_tree", 2);
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(gemini);
    registry.register(mux);

    let temp = tempdir().unwrap();
    let mut progress = Vec::new();
    let receipt = refresh_source_backed_generation_with_progress(
        temp.path(),
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
        |update| {
            progress.push(update);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(receipt.scanned_routes, 2);
    assert_eq!(receipt.commit.indexed_documents, 2);
    assert_eq!(receipt.commit.certified_sources, 2);
    assert_eq!(receipt.certified_source_count, 2);
    assert_eq!(receipt.certified_source_bytes, 2);
    assert_eq!(receipt.sources.len(), 2);
    assert_eq!(receipt.successful_route_outcomes.len(), 2);
    assert!(receipt
        .successful_route_outcomes
        .iter()
        .all(|outcome| outcome.changed));
    assert!(receipt.removals.is_empty());
    assert!(receipt.scan_stage_duration > Duration::ZERO);
    assert!(receipt.commit_duration > Duration::ZERO);
    assert!(progress
        .windows(2)
        .all(|pair| pair[0].elapsed <= pair[1].elapsed));
    let committed = progress.last().unwrap();
    assert_eq!(committed.phase, "committed");
    assert_eq!(committed.certified_source_count, Some(2));
    assert_eq!(committed.certified_source_bytes, Some(2));
    assert!(committed.stage_duration > Duration::ZERO);
}

#[test]
fn automatic_identity_preserves_discovered_replacement_and_distinguishes_catalogs() {
    let driver = fixture_route(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 3)
        .driver
        .unwrap();
    let automatic_route = |provider: CaptureProvider,
                           source_format: &'static str,
                           authority: SourceBackedSelectorAuthority,
                           path: &'static str| {
        SourceBackedRoute::automatic(
            fixture_provider_source_at(
                provider,
                source_format,
                ProviderImportSupport::Native,
                path,
            ),
            authority,
            driver.clone(),
        )
        .unwrap()
    };

    let discovered_first = automatic_route(
        CaptureProvider::Gemini,
        GEMINI_CLI_SOURCE_FORMAT,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        "/fixture/gemini-first",
    );
    let discovered_second = automatic_route(
        CaptureProvider::Gemini,
        GEMINI_CLI_SOURCE_FORMAT,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        "/fixture/gemini-second",
    );
    assert_eq!(
        discovered_first.metadata.route_identity, discovered_second.metadata.route_identity,
        "generic discovered-winner path changes must retain replacement identity"
    );
    let mut discovered_registry = SourceBackedProviderRegistry::new();
    discovered_registry.register(discovered_first);
    discovered_registry.register(discovered_second);
    assert_eq!(discovered_registry.executable_route_count(), 1);

    let nanoclaw_first = automatic_route(
        CaptureProvider::NanoClaw,
        "nanoclaw_project",
        SourceBackedSelectorAuthority::CatalogLineage,
        "/fixture/nanoclaw-first",
    );
    let nanoclaw_second = automatic_route(
        CaptureProvider::NanoClaw,
        "nanoclaw_project",
        SourceBackedSelectorAuthority::CatalogLineage,
        "/fixture/nanoclaw-second",
    );
    assert_ne!(
        nanoclaw_first.metadata.route_identity,
        nanoclaw_second.metadata.route_identity
    );
    let mut nanoclaw_registry = SourceBackedProviderRegistry::new();
    nanoclaw_registry.register(nanoclaw_first);
    nanoclaw_registry.register(nanoclaw_second);
    assert_eq!(nanoclaw_registry.executable_route_count(), 2);
}

#[test]
fn parallel_leaf_capability_respects_exact_route_scope() {
    let serial = fixture_route(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 7);
    let serial_id = serial.metadata.route_identity.clone().unwrap();
    let mut parallel = fixture_route(CaptureProvider::Mux, "mux_session_jsonl_tree", 8);
    let parallel_id = parallel.metadata.route_identity.clone().unwrap();
    parallel.driver = parallel
        .driver
        .take()
        .map(SourceBackedRouteDriver::with_parallel_leaf_workers);

    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(serial);
    registry.register(parallel);

    assert!(!registry
        .selected_routes_use_parallel_leaf_workers(&SourceBackedRefreshScope::exact([serial_id])));
    assert!(
        registry.selected_routes_use_parallel_leaf_workers(&SourceBackedRefreshScope::exact([
            parallel_id
        ]))
    );
    assert!(registry.selected_routes_use_parallel_leaf_workers(&SourceBackedRefreshScope::All));
}

#[test]
fn production_route_families_advertise_parallel_leaf_capability() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("ctx-data");
    fs::create_dir_all(&data_root).unwrap();

    let sources = [
        (
            CaptureProvider::Gemini,
            GEMINI_CLI_SOURCE_FORMAT,
            temp.path().join("gemini.jsonl"),
            true,
            false,
        ),
        (
            CaptureProvider::Cline,
            CLINE_SDK_SOURCE_FORMAT,
            temp.path().join("cline-sdk-data"),
            true,
            true,
        ),
        (
            CaptureProvider::Cline,
            CLINE_TASK_JSON_SOURCE_FORMAT,
            temp.path().join("cline"),
            true,
            false,
        ),
        (
            CaptureProvider::Continue,
            CONTINUE_CLI_SOURCE_FORMAT,
            temp.path().join("continue"),
            false,
            false,
        ),
    ];

    let mut registry = SourceBackedProviderRegistry::new();
    for (provider, source_format, path, _, sqlite) in &sources {
        if *sqlite {
            register_landed_source_backed_route_with_data_root(
                &mut registry,
                fixture_provider_source_at(
                    *provider,
                    source_format,
                    ProviderImportSupport::Native,
                    path,
                ),
                SourceBackedRouteSelection::Automatic,
                &data_root,
            )
            .unwrap();
        } else {
            register_landed_source_backed_route(
                &mut registry,
                fixture_provider_source_at(
                    *provider,
                    source_format,
                    ProviderImportSupport::Native,
                    path,
                ),
                SourceBackedRouteSelection::Automatic,
            )
            .unwrap();
        }
    }

    for (provider, _, _, expected_parallel, _) in sources {
        let route_id = registry
            .routes()
            .find(|route| route.source.provider == provider)
            .and_then(|route| route.route_identity.clone())
            .unwrap();
        assert_eq!(
            registry.selected_routes_use_parallel_leaf_workers(&SourceBackedRefreshScope::exact([
                route_id
            ])),
            expected_parallel,
            "unexpected production leaf-worker capability for {provider:?}"
        );
    }
}

fn fail_route_after_scan(
    mut route: SourceBackedRoute,
    kind: SourceBackedRouteErrorKind,
) -> SourceBackedRoute {
    let original = route.driver.take().unwrap();
    let scan = Arc::clone(&original.scan);
    let owns = Arc::clone(&original.owns_source);
    let revalidate = Arc::clone(&original.revalidate);
    route.driver = Some(SourceBackedRouteDriver::new_fallible(
        move |sink| {
            scan(sink)?;
            Err(SourceBackedRouteError::new(kind, "fixture route failure"))
        },
        move |source| owns(source),
        move |target| revalidate(target),
    ));
    route
}

fn fail_route_before_scan(
    mut route: SourceBackedRoute,
    kind: SourceBackedRouteErrorKind,
) -> SourceBackedRoute {
    let original = route.driver.take().unwrap();
    let owns = Arc::clone(&original.owns_source);
    route.driver = Some(SourceBackedRouteDriver::new_fallible(
        move |_| Err(SourceBackedRouteError::new(kind, "fixture route failure")),
        move |source| owns(source),
        |_| Ok(false),
    ));
    route
}

fn fail_route_at_final_revalidation(mut route: SourceBackedRoute) -> SourceBackedRoute {
    let mut driver = route.driver.take().unwrap();
    driver.revalidate = Arc::new(|_| Ok(false));
    route.driver = Some(driver);
    route
}

fn fail_route_at_final_inventory_revalidation(mut route: SourceBackedRoute) -> SourceBackedRoute {
    let mut driver = route.driver.take().unwrap();
    driver.revalidate_complete_inventory = Some(Arc::new(|_| Ok(false)));
    route.driver = Some(driver);
    route
}

fn fail_route_with_terminal_callback_error(
    mut route: SourceBackedRoute,
    inventory: bool,
    kind: SourceBackedRouteErrorKind,
) -> SourceBackedRoute {
    let mut driver = route.driver.take().unwrap();
    if inventory {
        driver.revalidate_complete_inventory = Some(Arc::new(move |_| {
            Err(SourceBackedRouteError::new(
                kind,
                "injected terminal inventory callback failure",
            ))
        }));
    } else {
        driver.revalidate = Arc::new(move |_| {
            Err(SourceBackedRouteError::new(
                kind,
                "injected terminal source callback failure",
            ))
        });
    }
    route.driver = Some(driver);
    route
}

fn count_route_scans(
    mut route: SourceBackedRoute,
    scans: Arc<std::sync::atomic::AtomicUsize>,
) -> SourceBackedRoute {
    let mut driver = route.driver.take().unwrap();
    let scan = Arc::clone(&driver.scan);
    driver.scan = Arc::new(move |sink| {
        scans.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        scan(sink)
    });
    route.driver = Some(driver);
    route
}

fn fail_route_with_systemic_writer_error(
    mut route: SourceBackedRoute,
    source: SourceKey,
) -> SourceBackedRoute {
    let original = route.driver.take().unwrap();
    let scan = Arc::clone(&original.scan);
    let owns = Arc::clone(&original.owns_source);
    let revalidate = Arc::clone(&original.revalidate);
    route.driver = Some(SourceBackedRouteDriver::new_fallible(
        move |sink| {
            scan(sink)?;
            sink.begin_source(source.clone())
                .map_err(route_coordinator_error)
        },
        move |source| owns(source),
        move |target| revalidate(target),
    ));
    route
}

#[test]
fn cold_second_route_failure_after_output_publishes_first_without_partial_records() {
    let first_scans = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let first = count_route_scans(
        fixture_route(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 1),
        Arc::clone(&first_scans),
    );
    let second = fixture_route(CaptureProvider::Mux, "mux_session_jsonl_tree", 2);
    let first_id = first.metadata.route_identity.clone().unwrap();
    let second_id = second.metadata.route_identity.clone().unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(first);
    registry.register(fail_route_after_scan(
        second,
        SourceBackedRouteErrorKind::SourceChanged,
    ));
    let temp = tempdir().unwrap();

    let receipt =
        refresh_source_backed_generation(temp.path(), &registry, WriterOptions::default()).unwrap();
    assert_eq!(receipt.successful_route_ids, vec![first_id.clone()]);
    assert_eq!(receipt.failed_routes.len(), 1);
    assert_eq!(receipt.failed_routes[0].route_identity, second_id.clone());
    assert_eq!(
        receipt.failed_routes[0].class,
        SourceBackedSourceFailureClass::SourceChanged
    );
    assert!(!receipt.failed_routes[0].carried_forward);
    assert!(receipt.commit.manifest().source_route(&first_id).is_some());
    assert!(receipt.commit.manifest().source_route(&second_id).is_none());
    assert_eq!(receipt.commit.indexed_documents, 1);
    assert_eq!(
        VerifiedIndex::open(temp.path()).unwrap().document_count(),
        1
    );
    assert_eq!(
        first_scans.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a later scan failure must not repeat an earlier successful route"
    );
}

#[test]
fn cold_opencode_capacity_failure_publishes_healthy_peer() {
    let healthy = fixture_route(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 41);
    let failing = fixture_route(CaptureProvider::OpenCode, "opencode_sqlite", 42);
    let healthy_id = healthy.metadata.route_identity.clone().unwrap();
    let failing_id = failing.metadata.route_identity.clone().unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(healthy);
    registry.register(fail_route_before_scan(
        failing,
        SourceBackedRouteErrorKind::Unavailable,
    ));
    let temp = tempdir().unwrap();

    let receipt =
        refresh_source_backed_generation(temp.path(), &registry, WriterOptions::default()).unwrap();

    assert_eq!(receipt.successful_route_ids, vec![healthy_id.clone()]);
    assert_eq!(receipt.failed_routes.len(), 1);
    assert_eq!(receipt.failed_routes[0].route_identity, failing_id.clone());
    assert_eq!(
        receipt.failed_routes[0].class,
        SourceBackedSourceFailureClass::Unavailable
    );
    assert!(!receipt.failed_routes[0].carried_forward);
    assert!(receipt
        .commit
        .manifest()
        .source_route(&healthy_id)
        .is_some());
    assert!(receipt
        .commit
        .manifest()
        .source_route(&failing_id)
        .is_none());
    assert_eq!(receipt.commit.indexed_documents, 1);
}

#[test]
fn warm_success_advances_while_failed_route_is_carried_exactly() {
    let (first_v1, _) = revisioned_receipt_route(1);
    let second = fixture_route(CaptureProvider::Mux, "mux_session_jsonl_tree", 9);
    let second_id = second.metadata.route_identity.clone().unwrap();
    let mut initial_registry = SourceBackedProviderRegistry::new();
    initial_registry.register(first_v1);
    initial_registry.register(second.clone());
    let temp = tempdir().unwrap();
    let initial =
        refresh_source_backed_generation(temp.path(), &initial_registry, WriterOptions::default())
            .unwrap();
    let retained_route = initial
        .commit
        .manifest()
        .source_route(&second_id)
        .unwrap()
        .clone();
    let retained_sources = retained_route
        .sources()
        .iter()
        .filter_map(|source| {
            initial
                .sources
                .iter()
                .find(|certificate| {
                    certificate
                        .observation()
                        .source()
                        .exact_descriptor_eq(source)
                })
                .cloned()
        })
        .collect::<Vec<_>>();

    let (first_v2, first_v2_certificate) = revisioned_receipt_route(2);
    let first_id = first_v2.metadata.route_identity.clone().unwrap();
    let mut warm_registry = SourceBackedProviderRegistry::new();
    warm_registry.register(first_v2);
    warm_registry.register(fail_route_before_scan(
        second,
        SourceBackedRouteErrorKind::Unavailable,
    ));
    let warm =
        refresh_source_backed_generation(temp.path(), &warm_registry, WriterOptions::default())
            .unwrap();

    assert!(warm.successful_route_ids.contains(&first_id));
    assert_eq!(warm.failed_routes.len(), 1);
    assert!(warm.failed_routes[0].carried_forward);
    assert_eq!(
        warm.commit.manifest().source_route(&second_id),
        Some(&retained_route)
    );
    for retained in retained_sources {
        assert!(warm.sources.contains(&retained));
    }
    assert!(warm.sources.contains(&first_v2_certificate));
    assert_eq!(warm.commit.indexed_documents, 2);
}

#[test]
fn successful_route_outcomes_distinguish_changed_and_unchanged_routes() {
    let (first_v1, _) = revisioned_receipt_route(1);
    let second = fixture_route(CaptureProvider::Mux, "mux_session_jsonl_tree", 9);
    let first_id = first_v1.metadata.route_identity.clone().unwrap();
    let second_id = second.metadata.route_identity.clone().unwrap();
    let mut initial_registry = SourceBackedProviderRegistry::new();
    initial_registry.register(first_v1);
    initial_registry.register(second.clone());
    let temp = tempdir().unwrap();
    refresh_source_backed_generation(temp.path(), &initial_registry, WriterOptions::default())
        .unwrap();

    let (first_v2, _) = revisioned_receipt_route(2);
    let mut warm_registry = SourceBackedProviderRegistry::new();
    warm_registry.register(first_v2);
    warm_registry.register(second);
    let warm =
        refresh_source_backed_generation(temp.path(), &warm_registry, WriterOptions::default())
            .unwrap();

    let changed = warm
        .successful_route_outcomes
        .iter()
        .map(|outcome| (&outcome.route_identity, outcome.changed))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(changed.get(&first_id), Some(&true));
    assert_eq!(changed.get(&second_id), Some(&false));
}

#[test]
fn authoritative_executor_publishes_valid_route_and_receipts_carried_failure() {
    let (valid_v1, _) = revisioned_receipt_route(31);
    let failing = fixture_route(CaptureProvider::Mux, "mux_session_jsonl_tree", 32);
    let failing_id = failing.metadata.route_identity.clone().unwrap();
    let mut initial_registry = SourceBackedProviderRegistry::new();
    initial_registry.register(valid_v1);
    initial_registry.register(failing.clone());
    let temp = tempdir().unwrap();
    let initial = SourceBackedRefreshExecutor::new(initial_registry, WriterOptions::default())
        .refresh_scope(temp.path(), SourceBackedRefreshScope::All, |_| Ok(()))
        .unwrap();
    let retained_failing_route = initial
        .commit
        .manifest()
        .source_route(&failing_id)
        .unwrap()
        .clone();

    let (valid_v2, valid_v2_certificate) = revisioned_receipt_route(33);
    let valid_id = valid_v2.metadata.route_identity.clone().unwrap();
    let mut refresh_registry = SourceBackedProviderRegistry::new();
    refresh_registry.register(valid_v2);
    refresh_registry.register(fail_route_before_scan(
        failing,
        SourceBackedRouteErrorKind::Unavailable,
    ));
    let receipt = SourceBackedRefreshExecutor::new(refresh_registry, WriterOptions::default())
        .refresh_scope(temp.path(), SourceBackedRefreshScope::All, |_| Ok(()))
        .unwrap();

    assert_eq!(receipt.successful_route_ids, vec![valid_id]);
    assert_eq!(receipt.failed_routes.len(), 1);
    assert_eq!(receipt.failed_routes[0].route_identity, failing_id.clone());
    assert_eq!(
        receipt.failed_routes[0].class,
        SourceBackedSourceFailureClass::Unavailable
    );
    assert!(receipt.failed_routes[0].carried_forward);
    assert_eq!(receipt.carried_failed_route_ids, vec![failing_id.clone()]);
    assert_eq!(
        receipt.commit.manifest().source_route(&failing_id),
        Some(&retained_failing_route)
    );
    assert!(receipt.sources.contains(&valid_v2_certificate));
}

#[test]
fn internal_route_failure_aborts_the_whole_cold_refresh() {
    let first_scans = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let first = count_route_scans(
        fixture_route(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 11),
        Arc::clone(&first_scans),
    );
    let second_source = fixture_source(CaptureProvider::Mux, "mux_session_jsonl_tree", 12);
    let second = fail_route_with_systemic_writer_error(
        fixture_route(CaptureProvider::Mux, "mux_session_jsonl_tree", 12),
        second_source,
    );
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(first);
    registry.register(second);
    let temp = tempdir().unwrap();

    assert!(matches!(
        refresh_source_backed_generation(temp.path(), &registry, WriterOptions::default()),
        Err(SourceBackedCoordinatorError::RouteScan {
            source: SourceBackedRouteError {
                kind: SourceBackedRouteErrorKind::Internal,
                ..
            },
            ..
        })
    ));
    assert!(matches!(
        VerifiedIndex::open(temp.path()),
        Err(IndexError::MissingActiveGenerationPointer)
    ));
    assert_eq!(
        first_scans.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a systemic abort must not restart already completed route work"
    );
}

#[test]
fn terminal_callback_errors_are_route_fatal_not_source_changed() {
    let source_route = fail_route_with_terminal_callback_error(
        fixture_route(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 41),
        false,
        SourceBackedRouteErrorKind::Internal,
    );
    let mut source_registry = SourceBackedProviderRegistry::new();
    source_registry.register(source_route);
    let source_root = tempdir().unwrap();
    assert!(matches!(
        refresh_source_backed_generation(
            source_root.path(),
            &source_registry,
            WriterOptions::default(),
        ),
        Err(SourceBackedCoordinatorError::RouteScan {
            source: SourceBackedRouteError {
                kind: SourceBackedRouteErrorKind::Internal,
                ..
            },
            ..
        })
    ));

    let source = fixture_source(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 42);
    let mut inventory_registry = inventory_replay_registry(Arc::new(Mutex::new(vec![source])));
    let inventory_route = fail_route_with_terminal_callback_error(
        inventory_registry.routes.pop().unwrap(),
        true,
        SourceBackedRouteErrorKind::ResourceUnavailable,
    );
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(inventory_route);
    let inventory_root = tempdir().unwrap();
    assert!(matches!(
        refresh_source_backed_generation(
            inventory_root.path(),
            &registry,
            WriterOptions::default(),
        ),
        Err(SourceBackedCoordinatorError::RouteScan {
            source: SourceBackedRouteError {
                kind: SourceBackedRouteErrorKind::ResourceUnavailable,
                ..
            },
            ..
        })
    ));
}

#[test]
fn real_shared_resource_exhaustion_aborts_warm_refresh_and_retains_complete_prior_generation() {
    let (first_v1, _) = revisioned_receipt_route(51);
    let second = fixture_route(CaptureProvider::Mux, "mux_session_jsonl_tree", 52);
    let mut initial_registry = SourceBackedProviderRegistry::new();
    initial_registry.register(first_v1);
    initial_registry.register(second.clone());
    let temp = tempdir().unwrap();
    let initial =
        refresh_source_backed_generation(temp.path(), &initial_registry, WriterOptions::default())
            .unwrap();
    let initial_generation = initial.commit.generation_id.clone();
    let initial_sources = initial.sources.clone();

    let first_v2 = fixture_route(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 51);
    let mut warm_registry = SourceBackedProviderRegistry::new();
    warm_registry.register(first_v2);
    warm_registry.register(fixture_route_with_body(
        CaptureProvider::Mux,
        "mux_session_jsonl_tree",
        52,
        "x".repeat(8 * 1024),
    ));

    let error = refresh_source_backed_generation_with_resource_limits_for_test(
        temp.path(),
        &warm_registry,
        WriterOptions::default(),
        4 * 1024,
        u64::MAX,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        SourceBackedCoordinatorError::RouteScan {
            source: SourceBackedRouteError {
                kind: SourceBackedRouteErrorKind::ResourceUnavailable,
                ..
            },
            ..
        }
    ));

    let retained = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(retained.generation_id(), initial_generation);
    assert_eq!(retained.document_count(), 2);
    assert_eq!(retained.manifest().sources, initial_sources);
}

#[test]
fn cold_final_revalidation_failures_scan_each_route_once_and_publish_only_successes() {
    let first_scans = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let second_scans = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let third_scans = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let first = count_route_scans(
        fixture_route(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 13),
        Arc::clone(&first_scans),
    );
    let second = count_route_scans(
        fail_route_at_final_revalidation(fixture_route(
            CaptureProvider::Mux,
            "mux_session_jsonl_tree",
            14,
        )),
        Arc::clone(&second_scans),
    );
    let third = count_route_scans(
        fail_route_at_final_revalidation(fixture_route(
            CaptureProvider::Tabnine,
            "tabnine_cli_chat_recording_jsonl",
            15,
        )),
        Arc::clone(&third_scans),
    );
    let first_id = first.metadata.route_identity.clone().unwrap();
    let second_id = second.metadata.route_identity.clone().unwrap();
    let third_id = third.metadata.route_identity.clone().unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(first);
    registry.register(second);
    registry.register(third);
    let temp = tempdir().unwrap();

    let mut progress = Vec::new();
    let receipt = refresh_source_backed_generation_with_progress(
        temp.path(),
        &registry,
        WriterOptions::default(),
        |update| {
            progress.push(update);
            Ok(())
        },
    )
    .unwrap();
    let completed_sources = progress
        .iter()
        .map(|update| update.completed_sources)
        .collect::<Vec<_>>();
    assert!(
        completed_sources
            .windows(2)
            .all(|window| window[0] <= window[1]),
        "route-attempt progress must be monotonic: {completed_sources:?}"
    );
    let committed = progress.last().unwrap();
    assert_eq!(committed.phase, "committed");
    assert_eq!(committed.completed_sources, 3);
    assert_eq!(committed.total_sources, 3);
    assert_eq!(receipt.successful_route_ids, vec![first_id.clone()]);
    assert_eq!(receipt.failed_routes.len(), 2);
    assert_eq!(
        receipt
            .failed_routes
            .iter()
            .map(|failure| failure.route_identity.clone())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([second_id.clone(), third_id.clone()])
    );
    assert!(receipt.failed_routes.iter().all(|failure| {
        failure.class == SourceBackedSourceFailureClass::SourceChanged && !failure.carried_forward
    }));
    assert!(receipt.commit.manifest().source_route(&first_id).is_some());
    assert!(receipt.commit.manifest().source_route(&second_id).is_none());
    assert!(receipt.commit.manifest().source_route(&third_id).is_none());
    assert_eq!(receipt.commit.indexed_documents, 1);
    assert_eq!(
        first_scans.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "successive final failures must not rescan a successful route"
    );
    assert_eq!(
        second_scans.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a terminally failed route must not be scanned again"
    );
    assert_eq!(
        third_scans.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a terminally failed route must not be scanned again"
    );
}

#[test]
fn final_inventory_failure_scans_each_route_once_and_stays_route_local() {
    let successful_scans = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let failed_scans = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let successful = count_route_scans(
        fixture_route(CaptureProvider::Mux, "mux_session_jsonl_tree", 16),
        Arc::clone(&successful_scans),
    );
    let successful_id = successful.metadata.route_identity.clone().unwrap();
    let source = fixture_source(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 17);
    let mut inventory_registry = inventory_replay_registry(Arc::new(Mutex::new(vec![source])));
    let failed = count_route_scans(
        fail_route_at_final_inventory_revalidation(inventory_registry.routes.pop().unwrap()),
        Arc::clone(&failed_scans),
    );
    let failed_id = failed.metadata.route_identity.clone().unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(successful);
    registry.register(failed);
    let temp = tempdir().unwrap();

    let receipt =
        refresh_source_backed_generation(temp.path(), &registry, WriterOptions::default()).unwrap();
    assert_eq!(receipt.successful_route_ids, vec![successful_id.clone()]);
    assert_eq!(receipt.failed_routes.len(), 1);
    assert_eq!(receipt.failed_routes[0].route_identity, failed_id.clone());
    assert_eq!(
        receipt.failed_routes[0].class,
        SourceBackedSourceFailureClass::SourceChanged
    );
    assert!(!receipt.failed_routes[0].carried_forward);
    assert!(receipt
        .commit
        .manifest()
        .source_route(&successful_id)
        .is_some());
    assert!(receipt.commit.manifest().source_route(&failed_id).is_none());
    assert_eq!(receipt.commit.indexed_documents, 1);
    assert_eq!(
        successful_scans.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_eq!(failed_scans.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn warm_final_revalidation_failure_scans_once_and_carries_the_exact_route() {
    let (first_v1, _) = revisioned_receipt_route(1);
    let second = fixture_route(CaptureProvider::Mux, "mux_session_jsonl_tree", 16);
    let second_id = second.metadata.route_identity.clone().unwrap();
    let mut initial_registry = SourceBackedProviderRegistry::new();
    initial_registry.register(first_v1);
    initial_registry.register(second.clone());
    let temp = tempdir().unwrap();
    let initial =
        refresh_source_backed_generation(temp.path(), &initial_registry, WriterOptions::default())
            .unwrap();
    let retained_second = initial
        .commit
        .manifest()
        .source_route(&second_id)
        .unwrap()
        .clone();

    let first_scans = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let second_scans = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (first_v2, first_v2_certificate) = revisioned_receipt_route(2);
    let first_v2 = count_route_scans(first_v2, Arc::clone(&first_scans));
    let first_id = first_v2.metadata.route_identity.clone().unwrap();
    let second = count_route_scans(
        fail_route_at_final_revalidation(second),
        Arc::clone(&second_scans),
    );
    let mut warm_registry = SourceBackedProviderRegistry::new();
    warm_registry.register(first_v2);
    warm_registry.register(second);

    let warm =
        refresh_source_backed_generation(temp.path(), &warm_registry, WriterOptions::default())
            .unwrap();
    assert_eq!(warm.successful_route_ids, vec![first_id]);
    assert_eq!(warm.failed_routes.len(), 1);
    assert_eq!(warm.failed_routes[0].route_identity, second_id.clone());
    assert_eq!(
        warm.failed_routes[0].class,
        SourceBackedSourceFailureClass::SourceChanged
    );
    assert!(warm.failed_routes[0].carried_forward);
    assert_eq!(warm.carried_failed_route_ids, vec![second_id.clone()]);
    assert_eq!(
        warm.commit.manifest().source_route(&second_id),
        Some(&retained_second)
    );
    assert!(warm.sources.contains(&first_v2_certificate));
    assert_eq!(warm.commit.indexed_documents, 2);
    assert_eq!(
        first_scans.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a warm successful route must retain its one staged scan"
    );
    assert_eq!(
        second_scans.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a warm failed route must be excluded from its existing stage"
    );
}

#[test]
fn cold_refresh_with_only_failed_routes_does_not_publish_ready_data() {
    let route = fail_route_before_scan(
        fixture_route(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 15),
        SourceBackedRouteErrorKind::Unavailable,
    );
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(route);
    let temp = tempdir().unwrap();

    let error = refresh_source_backed_generation(temp.path(), &registry, WriterOptions::default())
        .unwrap_err();
    assert!(matches!(
        error,
        SourceBackedCoordinatorError::NoUsableSourceRoutes { failed_routes }
            if failed_routes.len() == 1
                && failed_routes[0].class == SourceBackedSourceFailureClass::Unavailable
    ));
    assert!(matches!(
        VerifiedIndex::open(temp.path()),
        Err(IndexError::MissingActiveGenerationPointer)
    ));
}
