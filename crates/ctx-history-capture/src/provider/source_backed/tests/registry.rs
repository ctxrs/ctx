use super::*;

mod inventory_replay;
mod progress;

use inventory_replay::{inventory_replay_registry, revisioned_receipt_route};

#[test]
fn heterogeneous_routes_publish_one_core_generation() {
    let gemini = fixture_route(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 1);
    let hermes = fixture_route(CaptureProvider::Hermes, "hermes_state_sqlite", 2);
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(gemini);
    registry.register(hermes);

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

fn fail_route_after_scan(
    mut route: SourceBackedRoute,
    kind: SourceBackedRouteErrorKind,
) -> SourceBackedRoute {
    let original = route.driver.take().unwrap();
    let scan = Arc::clone(&original.scan);
    let owns = Arc::clone(&original.owns_source);
    let revalidate = Arc::clone(&original.revalidate);
    route.driver = Some(SourceBackedRouteDriver::new(
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
    route.driver = Some(SourceBackedRouteDriver::new(
        move |_| Err(SourceBackedRouteError::new(kind, "fixture route failure")),
        move |source| owns(source),
        |_| false,
    ));
    route
}

fn empty_route(mut route: SourceBackedRoute) -> SourceBackedRoute {
    let original = route.driver.take().unwrap();
    let owns = Arc::clone(&original.owns_source);
    let revalidate = Arc::clone(&original.revalidate);
    route.driver = Some(SourceBackedRouteDriver::new(
        |_| Ok(()),
        move |source| owns(source),
        move |target| revalidate(target),
    ));
    route
}

fn explicit_route_at(mut route: SourceBackedRoute, path: PathBuf) -> SourceBackedRoute {
    let mut source = route.metadata.source.clone();
    source.path = path;
    SourceBackedRoute::explicit_manual(
        source,
        SourceBackedSelectorAuthority::ExplicitPath,
        route.driver.take().unwrap(),
    )
    .unwrap()
}

fn fail_route_at_final_revalidation(mut route: SourceBackedRoute) -> SourceBackedRoute {
    let mut driver = route.driver.take().unwrap();
    driver.revalidate = Arc::new(|_| false);
    route.driver = Some(driver);
    route
}

fn fail_route_at_final_inventory_revalidation(mut route: SourceBackedRoute) -> SourceBackedRoute {
    let mut driver = route.driver.take().unwrap();
    driver.revalidate_complete_inventory = Some(Arc::new(|_| false));
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
    route.driver = Some(SourceBackedRouteDriver::new(
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
    let second = fixture_route(CaptureProvider::Hermes, "hermes_state_sqlite", 2);
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
fn warm_success_advances_while_failed_route_is_carried_exactly() {
    let (first_v1, _) = revisioned_receipt_route(1);
    let second = fixture_route(CaptureProvider::Hermes, "hermes_state_sqlite", 9);
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
    let second = fixture_route(CaptureProvider::Hermes, "hermes_state_sqlite", 9);
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
    let failing = fixture_route(CaptureProvider::Hermes, "hermes_state_sqlite", 32);
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
    let second_source = fixture_source(CaptureProvider::Hermes, "hermes_state_sqlite", 12);
    let second = fail_route_with_systemic_writer_error(
        fixture_route(CaptureProvider::Hermes, "hermes_state_sqlite", 12),
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
fn real_shared_resource_exhaustion_aborts_warm_refresh_and_retains_complete_prior_generation() {
    let (first_v1, _) = revisioned_receipt_route(51);
    let second = fixture_route(CaptureProvider::Hermes, "hermes_state_sqlite", 52);
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
        CaptureProvider::Hermes,
        "hermes_state_sqlite",
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
            CaptureProvider::Hermes,
            "hermes_state_sqlite",
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
        fixture_route(CaptureProvider::Hermes, "hermes_state_sqlite", 16),
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
    let second = fixture_route(CaptureProvider::Hermes, "hermes_state_sqlite", 16);
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

#[test]
fn warm_missing_route_in_grace_remains_usable_when_a_new_cold_route_fails() {
    let provider = CaptureProvider::Gemini;
    let format = GEMINI_CLI_SOURCE_FORMAT;
    let present = fixture_route(provider, format, 16);
    let route_id = present.metadata.route_identity.clone().unwrap();
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
    assert_eq!(missing.metadata.route_identity.as_ref(), Some(&route_id));
    let failed = fail_route_before_scan(
        fixture_route(CaptureProvider::Hermes, "hermes_state_sqlite", 17),
        SourceBackedRouteErrorKind::Unavailable,
    );
    let failed_id = failed.metadata.route_identity.clone().unwrap();
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
    let second = fixture_route(CaptureProvider::Hermes, "hermes_state_sqlite", 22);
    let first_id = first.metadata.route_identity.clone().unwrap();
    let second_id = second.metadata.route_identity.clone().unwrap();
    let second_scans = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let original_second = second.driver.clone().unwrap();
    let scans = Arc::clone(&second_scans);
    let second = SourceBackedRoute {
        driver: Some(SourceBackedRouteDriver::new(
            move |sink| {
                scans.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                (original_second.scan)(sink)
            },
            {
                let owns = Arc::clone(&second.driver.as_ref().unwrap().owns_source);
                move |source| owns(source)
            },
            {
                let revalidate = Arc::clone(&second.driver.as_ref().unwrap().revalidate);
                move |target| revalidate(target)
            },
        )),
        ..second
    };
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
    let retired_id = retired.metadata.route_identity.clone().unwrap();
    let mut initial_registry = SourceBackedProviderRegistry::new();
    initial_registry.register(retired);
    refresh_source_backed_generation(temp.path(), &initial_registry, WriterOptions::default())
        .unwrap();

    let replacement = empty_route(explicit_route_at(
        fixture_route(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 72),
        temp.path().join("replacement.jsonl"),
    ));
    let replacement_id = replacement.metadata.route_identity.clone().unwrap();
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
fn empty_replacement_cannot_hide_a_cold_route_failure_behind_retired_content() {
    let temp = tempdir().unwrap();
    let retired = explicit_route_at(
        fixture_route(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 73),
        temp.path().join("retired.jsonl"),
    );
    let retired_id = retired.metadata.route_identity.clone().unwrap();
    let mut initial_registry = SourceBackedProviderRegistry::new();
    initial_registry.register(retired);
    let initial =
        refresh_source_backed_generation(temp.path(), &initial_registry, WriterOptions::default())
            .unwrap();

    let replacement = empty_route(explicit_route_at(
        fixture_route(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 74),
        temp.path().join("replacement.jsonl"),
    ));
    let replacement_id = replacement.metadata.route_identity.clone().unwrap();
    let failed = fail_route_before_scan(
        fixture_route(CaptureProvider::Hermes, "hermes_state_sqlite", 75),
        SourceBackedRouteErrorKind::SourceChanged,
    );
    let failed_id = failed.metadata.route_identity.clone().unwrap();
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
        VerifiedIndex::open(temp.path()).unwrap().generation_id(),
        initial.commit.generation_id
    );
}

#[test]
fn selected_clean_route_completion_ignores_carried_unselected_rejections() {
    let clean = fixture_route(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 61);
    let rejected = fixture_route_with_body_and_rejections(
        CaptureProvider::Hermes,
        "hermes_state_sqlite",
        62,
        "retained peer".to_owned(),
        1,
    );
    let clean_id = clean.metadata.route_identity.clone().unwrap();
    let rejected_id = rejected.metadata.route_identity.clone().unwrap();
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
    let second = fixture_route(CaptureProvider::Hermes, "hermes_state_sqlite", 24);
    let first_id = first.metadata.route_identity.clone().unwrap();
    let second_id = second.metadata.route_identity.clone().unwrap();
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

    for expected in 1..AUTOMATIC_ROUTE_DELETION_MISSING_OBSERVATIONS {
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

    let retained_generation = VerifiedIndex::open(temp.path())
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
        VerifiedIndex::open(temp.path()).unwrap().generation_id(),
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

    for expected in 1..AUTOMATIC_ROUTE_DELETION_MISSING_OBSERVATIONS {
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
        VerifiedIndex::open(temp.path()).unwrap().document_count(),
        0
    );
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

    for _ in 1..AUTOMATIC_ROUTE_DELETION_MISSING_OBSERVATIONS {
        refresh_source_backed_generation(
            temp.path(),
            &missing_registry(),
            WriterOptions::default(),
        )
        .unwrap();
    }
    let retained_generation = VerifiedIndex::open(temp.path())
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

    let retained = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(retained.generation_id(), retained_generation);
    assert!(retained.manifest().source_route(&route_id).is_some());
    assert_eq!(retained.document_count(), 1);
}

#[test]
fn relocated_route_rechecks_old_path_absence_at_terminal_publication() {
    let temp = tempdir().unwrap();
    let old_path = temp.path().join("relocation-old.jsonl");
    let new_path = temp.path().join("relocation-new.jsonl");
    let fixture = fixture_route(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 63);
    let mut old_source = fixture.metadata.source.clone();
    old_source.path = old_path.clone();
    let old_route = SourceBackedRoute::explicit_manual(
        old_source,
        SourceBackedSelectorAuthority::ExplicitPath,
        fixture.driver.clone().unwrap(),
    )
    .unwrap();
    let preserved = old_route.metadata.route_identity.clone().unwrap();
    let mut initial_registry = SourceBackedProviderRegistry::new();
    initial_registry.register(old_route);
    let initial =
        refresh_source_backed_generation(temp.path(), &initial_registry, WriterOptions::default())
            .unwrap();

    let mut relocated_source = fixture.metadata.source.clone();
    relocated_source.path = new_path;
    let relocated_route = SourceBackedRoute::explicit_manual(
        relocated_source,
        SourceBackedSelectorAuthority::ExplicitPath,
        fixture.driver.unwrap(),
    )
    .unwrap();
    let constructed = relocated_route.metadata.route_identity.clone().unwrap();
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
    let retained = VerifiedIndex::open(temp.path()).unwrap();
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
    let error = refresh_source_backed_generation(
        temp.path(),
        &incomplete_registry,
        WriterOptions::default(),
    )
    .unwrap_err();

    assert!(matches!(
        error,
        SourceBackedCoordinatorError::UnclaimedBaseSource { ref source_id }
            if source_id == &initial_source.identity().to_string()
    ));
    let retained = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(retained.generation_id(), initial_generation);
    assert_eq!(retained.manifest().sources, initial.sources);
}

#[test]
fn cross_route_duplicate_source_ownership_remains_rejected() {
    let mut registry = SourceBackedProviderRegistry::new();
    let automatic = fixture_route(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 42);
    let explicit = SourceBackedRoute::explicit_manual(
        automatic.metadata.source.clone(),
        SourceBackedSelectorAuthority::ExplicitPath,
        automatic.driver.clone().unwrap(),
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
    assert_eq!(
        g1.commit.manifest().generation_id().unwrap(),
        g1.commit.generation_id
    );
    assert_eq!(
        g2.commit.manifest().generation_id().unwrap(),
        g2.commit.generation_id
    );
    assert!(g1.removals.is_empty());
    assert!(g2.removals.is_empty());
    assert_eq!(
        VerifiedIndex::open(root).unwrap().generation_id(),
        g2.commit.generation_id
    );
}
