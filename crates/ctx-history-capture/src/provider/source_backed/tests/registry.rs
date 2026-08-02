use super::*;

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

fn fail_route_at_final_revalidation(mut route: SourceBackedRoute) -> SourceBackedRoute {
    let original = route.driver.take().unwrap();
    let scan = Arc::clone(&original.scan);
    let owns = Arc::clone(&original.owns_source);
    route.driver = Some(SourceBackedRouteDriver::new(
        move |sink| scan(sink),
        move |source| owns(source),
        |_| false,
    ));
    route
}

fn count_route_scans(
    mut route: SourceBackedRoute,
    scans: Arc<std::sync::atomic::AtomicUsize>,
) -> SourceBackedRoute {
    let original = route.driver.take().unwrap();
    let scan = Arc::clone(&original.scan);
    let owns = Arc::clone(&original.owns_source);
    let revalidate = Arc::clone(&original.revalidate);
    route.driver = Some(SourceBackedRouteDriver::new(
        move |sink| {
            scans.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            scan(sink)
        },
        move |source| owns(source),
        move |target| revalidate(target),
    ));
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
fn internal_route_failure_aborts_the_whole_cold_refresh() {
    let first = fixture_route(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 11);
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
}

#[test]
fn final_revalidation_failure_retries_without_the_changed_route() {
    let first = fixture_route(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 13);
    let second = fail_route_at_final_revalidation(fixture_route(
        CaptureProvider::Hermes,
        "hermes_state_sqlite",
        14,
    ));
    let first_id = first.metadata.route_identity.clone().unwrap();
    let second_id = second.metadata.route_identity.clone().unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(first);
    registry.register(second);
    let temp = tempdir().unwrap();

    let receipt =
        refresh_source_backed_generation(temp.path(), &registry, WriterOptions::default()).unwrap();
    assert_eq!(receipt.successful_route_ids, vec![first_id]);
    assert_eq!(receipt.failed_routes.len(), 1);
    assert_eq!(receipt.failed_routes[0].route_identity, second_id.clone());
    assert_eq!(
        receipt.failed_routes[0].class,
        SourceBackedSourceFailureClass::SourceChanged
    );
    assert!(receipt.commit.manifest().source_route(&second_id).is_none());
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

#[test]
fn committed_progress_failure_does_not_hide_visible_publication() {
    let (route, certificate) = revisioned_receipt_route(7);
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(route);
    let temp = tempdir().unwrap();

    let receipt = refresh_source_backed_generation_with_progress(
        temp.path(),
        &registry,
        WriterOptions::default(),
        |progress| {
            if progress.phase == "committed" {
                Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    "injected committed status failure",
                ))
            } else {
                Ok(())
            }
        },
    )
    .expect("commit visibility is irreversible success");

    assert_eq!(receipt.sources, vec![certificate]);
    assert_eq!(
        VerifiedIndex::open(temp.path()).unwrap().generation_id(),
        receipt.commit.generation_id
    );
}

#[test]
fn source_record_progress_resets_per_route_and_is_absent_outside_scans() {
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(fixture_route(
        CaptureProvider::Gemini,
        GEMINI_CLI_SOURCE_FORMAT,
        1,
    ));
    registry.register(fixture_route(
        CaptureProvider::Hermes,
        "hermes_state_sqlite",
        2,
    ));
    let temp = tempdir().unwrap();
    let initial =
        refresh_source_backed_generation(temp.path(), &registry, WriterOptions::default()).unwrap();
    let mut updates = Vec::new();

    let replay = refresh_source_backed_generation_with_progress(
        temp.path(),
        &registry,
        WriterOptions::default(),
        |progress| {
            updates.push((
                progress.phase,
                progress.current_source,
                progress.completed_records,
            ));
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(replay.commit.generation_id, initial.commit.generation_id);

    let active = updates
        .iter()
        .filter(|(_, source, _)| source.is_some())
        .map(|(_, _, completed_records)| *completed_records)
        .collect::<Vec<_>>();
    assert_eq!(active, vec![Some(0), Some(1), Some(0), Some(1)]);
    assert!(updates
        .iter()
        .filter(|(_, source, _)| source.is_none())
        .all(|(_, _, completed_records)| completed_records.is_none()));
}

#[test]
fn accepted_record_progress_failure_stays_typed_and_prevents_publication() {
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(fixture_route(
        CaptureProvider::Gemini,
        GEMINI_CLI_SOURCE_FORMAT,
        1,
    ));
    let temp = tempdir().unwrap();

    let error = refresh_source_backed_generation_with_progress(
        temp.path(),
        &registry,
        WriterOptions::default(),
        |progress| {
            if progress.completed_records == Some(1) {
                Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    "injected source-record progress failure",
                ))
            } else {
                Ok(())
            }
        },
    )
    .unwrap_err();

    assert!(matches!(
        error,
        SourceBackedCoordinatorError::Progress(SourceBackedRouteError { detail, .. })
            if detail == "injected source-record progress failure"
    ));
    assert!(VerifiedIndex::open(temp.path()).is_err());
}

fn revisioned_receipt_route(revision: u8) -> (SourceBackedRoute, CertifiedSource) {
    let source = fixture_source(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 91);
    let session_id = fixture_session_id(&source);
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &NativeItemKey::native_id("message", TypedKey::U64(1)).unwrap(),
        subrecord_selector: None,
    })
    .unwrap();
    let revision_digest = [revision; 32];
    let mut document = CoreRecord::new_selected(
        event_id,
        session_id,
        session_id,
        source.clone(),
        1,
        "message",
        "primary",
        true,
        "coordinator-test-v1",
        format!("receipt revision {revision}"),
    )
    .unwrap();
    document.provider_session_id = Some("receipt-race".to_owned());
    document.native_event_id = Some(TypedKey::U64(1));
    document.occurred_at_unix_ms = Some(i64::from(revision));
    document.role = Some("user".to_owned());
    let observation =
        SourceObservation::new(source.clone(), "fixture-revision", vec![revision]).unwrap();
    let certificate = CertifiedSource::certify(
        observation.clone(),
        observation,
        "coordinator-test-v1",
        revision_digest,
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
    let scan_document = document.clone();
    let owned_source = source.clone();
    let driver = SourceBackedRouteDriver::new(
        move |sink| {
            sink.replace_source(scan_certificate.clone(), [scan_document.clone()])
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

fn inventory_replay_certificate(source: &SourceKey) -> CertifiedSource {
    let digest = source.identity().digest();
    let observation = SourceObservation::new(
        source.clone(),
        "inventory-replay-source-v1",
        digest.to_vec(),
    )
    .unwrap();
    let frontier = SourceFrontier::new(
        "inventory-replay-frontier-v1",
        TypedKey::bytes(digest.to_vec()).unwrap(),
        0,
        digest,
    )
    .unwrap();
    CertifiedSource::certify_with_frontier(
        observation.clone(),
        observation,
        "inventory-replay-parser-v1",
        digest,
        ScannedSourceCounts::default(),
        Some(frontier),
    )
    .unwrap()
}

fn inventory_replay_inventory(sources: &[SourceKey]) -> CertifiedSourceInventory {
    let mut ordered = sources.to_vec();
    ordered.sort();
    let revision = ordered
        .iter()
        .flat_map(|source| source.identity().digest())
        .collect::<Vec<_>>();
    let observation = SourceInventoryObservation::new(
        CaptureProvider::Gemini.as_str(),
        "inventory-replay-root-v1",
        TypedKey::utf8("root").unwrap(),
        "inventory-replay-membership-v1",
        if revision.is_empty() {
            vec![0]
        } else {
            revision
        },
    )
    .unwrap();
    CertifiedSourceInventory::certify(
        observation.clone(),
        observation,
        "inventory-replay-discovery-v1",
        ordered,
    )
    .unwrap()
}

fn owns_inventory_replay_source(source: &SourceKey) -> bool {
    source.provider() == CaptureProvider::Gemini.as_str()
        && source.source_format() == GEMINI_CLI_SOURCE_FORMAT
}

fn inventory_replay_registry(
    current_sources: Arc<Mutex<Vec<SourceKey>>>,
) -> SourceBackedProviderRegistry {
    let scan_sources = Arc::clone(&current_sources);
    let revalidation_sources = Arc::clone(&current_sources);
    let driver = SourceBackedRouteDriver::new(
        move |sink| {
            let current = scan_sources.lock().unwrap().clone();
            let base_sources = source_backed_base_sources(sink, owns_inventory_replay_source);
            for source in &current {
                if let Some(base) = sink.base_source(source).cloned() {
                    let frontier = base.frontier().expect("replay frontier");
                    sink.begin_source_append(source.clone())
                        .map_err(route_coordinator_error)?;
                    let append = CertifiedSourceAppend::certify(
                        &base,
                        base.clone(),
                        frontier.certified_prefix_bytes(),
                        *frontier.certified_prefix_digest(),
                    )
                    .map_err(|error| {
                        SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::Internal,
                            error.to_string(),
                        )
                    })?;
                    sink.certify_source_append(append)
                        .map_err(route_coordinator_error)?;
                } else {
                    sink.begin_source(source.clone())
                        .map_err(route_coordinator_error)?;
                    sink.certify_source(inventory_replay_certificate(source))
                        .map_err(route_coordinator_error)?;
                }
            }
            let inventory = inventory_replay_inventory(&current);
            sink.certify_complete_inventory(inventory.clone())
                .map_err(route_coordinator_error)?;
            for base in base_sources {
                let source = base.observation().source();
                if current
                    .iter()
                    .any(|candidate| candidate.exact_descriptor_eq(source))
                {
                    continue;
                }
                let deletion = CertifiedSourceDeletion::from_inventory(source.clone(), &inventory)
                    .map_err(|error| {
                        SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::Internal,
                            error.to_string(),
                        )
                    })?;
                sink.delete_source(deletion, inventory.clone())
                    .map_err(route_coordinator_error)?;
            }
            Ok(())
        },
        owns_inventory_replay_source,
        move |target| {
            let current = revalidation_sources.lock().unwrap().clone();
            let inventory = inventory_replay_inventory(&current);
            match target {
                SourceBackedRevalidationTarget::Source(certificate) => current
                    .iter()
                    .any(|source| inventory_replay_certificate(source) == *certificate),
                SourceBackedRevalidationTarget::Deletion(deletion) => {
                    owns_inventory_replay_source(deletion.source())
                        && !inventory.contains(deletion.source())
                        && deletion.verifies(&inventory)
                }
            }
        },
    )
    .with_complete_inventory_revalidation(move |inventory| {
        let current = current_sources.lock().unwrap().clone();
        inventory == &inventory_replay_inventory(&current)
    });
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(fixture_executable_route(
        CaptureProvider::Gemini,
        GEMINI_CLI_SOURCE_FORMAT,
        driver,
    ));
    registry
}

#[test]
fn delete_b_then_discover_c_keeps_receipts_and_manifests_current() {
    let source_a = fixture_source(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 101);
    let source_b = fixture_source(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 102);
    let source_c = fixture_source(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 103);
    let current = Arc::new(Mutex::new(vec![source_a.clone(), source_b.clone()]));
    let registry = inventory_replay_registry(Arc::clone(&current));
    let temp = tempdir().unwrap();

    let initial =
        refresh_source_backed_generation(temp.path(), &registry, WriterOptions::default()).unwrap();
    assert_eq!(initial.sources.len(), 2);
    assert!(initial.removals.is_empty());

    *current.lock().unwrap() = vec![source_a.clone()];
    let deleted =
        refresh_source_backed_generation(temp.path(), &registry, WriterOptions::default()).unwrap();
    assert_eq!(deleted.sources.len(), 1);
    assert_eq!(deleted.removals.len(), 1);
    assert!(deleted.removals[0]
        .deletion
        .source()
        .exact_descriptor_eq(&source_b));

    *current.lock().unwrap() = vec![source_a, source_c];
    let discovered =
        refresh_source_backed_generation(temp.path(), &registry, WriterOptions::default()).unwrap();
    assert_eq!(discovered.sources.len(), 2);
    assert!(discovered.removals.is_empty());
    assert_eq!(discovered.commit.manifest().sources.len(), 2);

    let replay =
        refresh_source_backed_generation(temp.path(), &registry, WriterOptions::default()).unwrap();
    assert_eq!(replay.commit.generation_id, discovered.commit.generation_id);
    assert!(replay.removals.is_empty());

    drop(registry);
    let restarted_registry = inventory_replay_registry(Arc::clone(&current));
    let restarted = refresh_source_backed_generation(
        temp.path(),
        &restarted_registry,
        WriterOptions::default(),
    )
    .unwrap();
    assert_eq!(
        restarted.commit.generation_id,
        discovered.commit.generation_id
    );
    assert!(restarted.removals.is_empty());
}

#[test]
fn unsupported_detected_format_stays_typed_and_never_executes() {
    let source = fixture_provider_source(
        CaptureProvider::Unknown,
        "unknown_detected_format",
        ProviderImportSupport::Unsupported,
    );
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(SourceBackedRoute::unsupported(
        source,
        "no product-approved source-backed adapter",
    ));
    let temp = tempdir().unwrap();
    let receipt =
        refresh_source_backed_generation(temp.path(), &registry, WriterOptions::default()).unwrap();
    assert_eq!(receipt.scanned_routes, 0);
    assert_eq!(receipt.unsupported_routes.len(), 1);
    assert!(receipt.sources.is_empty());
    assert!(receipt.removals.is_empty());
    assert!(VerifiedIndex::open(temp.path())
        .unwrap()
        .manifest()
        .sources
        .is_empty());
}
