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

fn controlled_revision_route(
    label: &'static str,
    lineage: u8,
    revision: u8,
    authority_revision: Arc<std::sync::atomic::AtomicU8>,
    scan_log: Arc<Mutex<Vec<String>>>,
    publication_count: Arc<std::sync::atomic::AtomicUsize>,
) -> (SourceBackedRoute, SourceKey) {
    let source = fixture_source(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, lineage);
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
        session_id,
        source.clone(),
        1,
        "message",
        "primary",
        true,
        "coordinator-test-v1",
        format!("{label}revision{revision}"),
    )
    .unwrap();
    record.provider_session_id = Some(label.to_owned());
    record.native_event_id = Some(TypedKey::U64(1));
    record.occurred_at_unix_ms = Some(i64::from(revision));
    record.role = Some("user".to_owned());
    let certificate = controlled_revision_certificate(&source, revision);
    let scan_certificate = certificate.clone();
    let revalidation_certificate = certificate;
    let owned_source = source.clone();
    let revalidation_authority = Arc::clone(&authority_revision);
    let mut driver = SourceBackedRouteDriver::new(
        move |sink| {
            scan_log.lock().unwrap().push(label.to_owned());
            sink.replace_source(scan_certificate.clone(), [record.clone()])
                .map_err(route_coordinator_error)
        },
        move |candidate| candidate.exact_descriptor_eq(&owned_source),
        move |target| {
            revalidation_authority.load(std::sync::atomic::Ordering::SeqCst) == revision
                && matches!(
                    target,
                    SourceBackedRevalidationTarget::Source(source)
                        if source == &revalidation_certificate
                )
        },
    );
    driver = driver.with_successful_publication(move || {
        publication_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    });
    (
        SourceBackedRoute::automatic(
            fixture_provider_source_at(
                CaptureProvider::Gemini,
                GEMINI_CLI_SOURCE_FORMAT,
                ProviderImportSupport::Native,
                format!("/fixture/{label}"),
            ),
            SourceBackedSelectorAuthority::DiscoveredWinner,
            driver,
        )
        .unwrap(),
        source,
    )
}

fn controlled_revision_certificate(source: &SourceKey, revision: u8) -> CertifiedSource {
    let observation =
        SourceObservation::new(source.clone(), "fixture-revision", vec![revision]).unwrap();
    CertifiedSource::certify(
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
    .unwrap()
}

fn rebuild_driver(
    mut route: SourceBackedRoute,
    scan: impl for<'writer> Fn(&mut SourceBackedGenerationSink<'writer>) -> SourceBackedRouteResult<()>
        + Send
        + Sync
        + 'static,
) -> SourceBackedRoute {
    let original = route.driver.take().unwrap();
    let owns = Arc::clone(&original.owns_source);
    let revalidate = Arc::clone(&original.revalidate);
    let complete_revalidate = original.revalidate_complete_inventory.clone();
    let after_publication = original.after_successful_publication.clone();
    let mut replacement = SourceBackedRouteDriver::new(
        scan,
        move |source| owns(source),
        move |target| revalidate(target),
    );
    if let Some(revalidate) = complete_revalidate {
        replacement = replacement
            .with_complete_inventory_revalidation(move |inventory| revalidate(inventory));
    }
    if let Some(after_publication) = after_publication {
        replacement = replacement.with_successful_publication(move || after_publication());
    }
    route.driver = Some(replacement);
    route
}

fn fail_route_after_scan(
    route: SourceBackedRoute,
    kind: SourceBackedRouteErrorKind,
    detail: impl Into<String>,
) -> SourceBackedRoute {
    let scan = Arc::clone(&route.driver.as_ref().unwrap().scan);
    let detail = detail.into();
    rebuild_driver(route, move |sink| {
        scan(sink)?;
        Err(SourceBackedRouteError::new(kind, detail.clone()))
    })
}

fn fail_route_before_scan(
    route: SourceBackedRoute,
    kind: SourceBackedRouteErrorKind,
    detail: impl Into<String>,
) -> SourceBackedRoute {
    let detail = detail.into();
    rebuild_driver(route, move |_| {
        Err(SourceBackedRouteError::new(kind, detail.clone()))
    })
}

fn after_scan(
    route: SourceBackedRoute,
    action: impl Fn() + Send + Sync + 'static,
) -> SourceBackedRoute {
    let scan = Arc::clone(&route.driver.as_ref().unwrap().scan);
    rebuild_driver(route, move |sink| {
        scan(sink)?;
        action();
        Ok(())
    })
}

#[test]
fn source_failure_keeps_a_and_c_carries_b_and_retry_updates_b() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let authority_a = Arc::new(std::sync::atomic::AtomicU8::new(1));
    let authority_b = Arc::new(std::sync::atomic::AtomicU8::new(1));
    let authority_c = Arc::new(std::sync::atomic::AtomicU8::new(1));
    let published_a = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let published_b = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let published_c = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut initial_registry = SourceBackedProviderRegistry::new();
    initial_registry.register(
        controlled_revision_route(
            "a",
            1,
            1,
            Arc::clone(&authority_a),
            Arc::clone(&log),
            Arc::clone(&published_a),
        )
        .0,
    );
    initial_registry.register(
        controlled_revision_route(
            "b",
            2,
            1,
            Arc::clone(&authority_b),
            Arc::clone(&log),
            Arc::clone(&published_b),
        )
        .0,
    );
    initial_registry.register(
        controlled_revision_route(
            "c",
            3,
            1,
            Arc::clone(&authority_c),
            Arc::clone(&log),
            Arc::clone(&published_c),
        )
        .0,
    );
    let temp = tempdir().unwrap();
    refresh_source_backed_generation(temp.path(), &initial_registry, WriterOptions::default())
        .unwrap();
    log.lock().unwrap().clear();

    authority_a.store(2, std::sync::atomic::Ordering::SeqCst);
    authority_b.store(2, std::sync::atomic::Ordering::SeqCst);
    authority_c.store(2, std::sync::atomic::Ordering::SeqCst);
    let route_a = controlled_revision_route(
        "a",
        1,
        2,
        Arc::clone(&authority_a),
        Arc::clone(&log),
        Arc::clone(&published_a),
    )
    .0;
    let route_b = controlled_revision_route(
        "b",
        2,
        2,
        Arc::clone(&authority_b),
        Arc::clone(&log),
        Arc::clone(&published_b),
    )
    .0;
    let route_c = controlled_revision_route(
        "c",
        3,
        2,
        Arc::clone(&authority_c),
        Arc::clone(&log),
        Arc::clone(&published_c),
    )
    .0;
    let mut failing_registry = SourceBackedProviderRegistry::new();
    failing_registry.register(route_a);
    failing_registry.register(fail_route_after_scan(
        route_b,
        SourceBackedRouteErrorKind::SourceChanged,
        "SQLite source changed while its read snapshot was active",
    ));
    failing_registry.register(route_c);

    let isolated =
        refresh_source_backed_generation(temp.path(), &failing_registry, WriterOptions::default())
            .unwrap();
    assert_eq!(
        isolated.outcome,
        SourceBackedRefreshOutcome::CompletedWithSourceFailures
    );
    assert_eq!(isolated.successful_routes, 2);
    assert_eq!(&*log.lock().unwrap(), &["a", "b", "c"]);
    let failure = &isolated.source_failures.failures()[0];
    assert_eq!(failure.class, SourceBackedSourceFailureClass::SourceChanged);
    assert!(failure.carried_forward);
    assert_eq!(failure.source_selector, "/fixture/b");
    assert!(failure.detail.contains("read snapshot"));
    assert_eq!(failure.source_identity.len(), 64);
    let visible = VerifiedIndex::open(temp.path()).unwrap();
    let source_a = fixture_source(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 1);
    let source_b = fixture_source(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 2);
    let source_c = fixture_source(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 3);
    assert!(visible
        .manifest()
        .sources
        .contains(&controlled_revision_certificate(&source_a, 2)));
    assert!(visible
        .manifest()
        .sources
        .contains(&controlled_revision_certificate(&source_b, 1)));
    assert!(!visible
        .manifest()
        .sources
        .contains(&controlled_revision_certificate(&source_b, 2)));
    assert!(visible
        .manifest()
        .sources
        .contains(&controlled_revision_certificate(&source_c, 2)));
    assert_eq!(published_a.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(published_b.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(published_c.load(std::sync::atomic::Ordering::SeqCst), 2);

    log.lock().unwrap().clear();
    let mut retry_registry = SourceBackedProviderRegistry::new();
    retry_registry.register(
        controlled_revision_route(
            "a",
            1,
            2,
            Arc::clone(&authority_a),
            Arc::clone(&log),
            Arc::clone(&published_a),
        )
        .0,
    );
    retry_registry.register(
        controlled_revision_route(
            "b",
            2,
            2,
            Arc::clone(&authority_b),
            Arc::clone(&log),
            Arc::clone(&published_b),
        )
        .0,
    );
    retry_registry.register(
        controlled_revision_route(
            "c",
            3,
            2,
            authority_c,
            Arc::clone(&log),
            Arc::clone(&published_c),
        )
        .0,
    );
    let retried =
        refresh_source_backed_generation(temp.path(), &retry_registry, WriterOptions::default())
            .unwrap();
    assert_eq!(retried.outcome, SourceBackedRefreshOutcome::Completed);
    assert!(retried.source_failures.is_empty());
    assert!(VerifiedIndex::open(temp.path())
        .unwrap()
        .manifest()
        .sources
        .contains(&controlled_revision_certificate(&source_b, 2)));
    assert_eq!(published_b.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[test]
fn cold_source_failure_omits_b_and_publishes_a_and_c() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let published = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let authority = Arc::new(std::sync::atomic::AtomicU8::new(1));
    let route_a = controlled_revision_route(
        "colda",
        11,
        1,
        Arc::clone(&authority),
        Arc::clone(&log),
        Arc::clone(&published),
    )
    .0;
    let route_b = controlled_revision_route(
        "coldb",
        12,
        1,
        Arc::clone(&authority),
        Arc::clone(&log),
        Arc::clone(&published),
    )
    .0;
    let route_c = controlled_revision_route(
        "coldc",
        13,
        1,
        authority,
        Arc::clone(&log),
        Arc::clone(&published),
    )
    .0;
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(route_a);
    registry.register(fail_route_after_scan(
        route_b,
        SourceBackedRouteErrorKind::InvalidSource,
        "source payload is unreadable",
    ));
    registry.register(route_c);
    let temp = tempdir().unwrap();

    let receipt =
        refresh_source_backed_generation(temp.path(), &registry, WriterOptions::default()).unwrap();
    assert_eq!(receipt.sources.len(), 2);
    assert_eq!(receipt.successful_routes, 2);
    assert!(!receipt.source_failures.failures()[0].carried_forward);
    let visible = VerifiedIndex::open(temp.path()).unwrap();
    let cold_b = fixture_source(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 12);
    assert_eq!(visible.document_count(), 2);
    assert!(!visible
        .manifest()
        .sources
        .contains(&controlled_revision_certificate(&cold_b, 1)));
}

#[test]
fn all_failed_warm_refresh_returns_unchanged_base_but_cold_refresh_has_no_publication() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let authority = Arc::new(std::sync::atomic::AtomicU8::new(1));
    let published = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut initial_registry = SourceBackedProviderRegistry::new();
    for (label, lineage) in [("walla", 21), ("wallb", 22), ("wallc", 23)] {
        initial_registry.register(
            controlled_revision_route(
                label,
                lineage,
                1,
                Arc::clone(&authority),
                Arc::clone(&log),
                Arc::clone(&published),
            )
            .0,
        );
    }
    let warm_root = tempdir().unwrap();
    let initial = refresh_source_backed_generation(
        warm_root.path(),
        &initial_registry,
        WriterOptions::default(),
    )
    .unwrap();
    let mut failed_registry = SourceBackedProviderRegistry::new();
    for (label, lineage) in [("walla", 21), ("wallb", 22), ("wallc", 23)] {
        let route = controlled_revision_route(
            label,
            lineage,
            1,
            Arc::clone(&authority),
            Arc::clone(&log),
            Arc::clone(&published),
        )
        .0;
        failed_registry.register(fail_route_before_scan(
            route,
            SourceBackedRouteErrorKind::Unavailable,
            "source is temporarily unavailable",
        ));
    }

    let retained = refresh_source_backed_generation(
        warm_root.path(),
        &failed_registry,
        WriterOptions::default(),
    )
    .unwrap();
    assert_eq!(retained.commit.generation_id, initial.commit.generation_id);
    assert_eq!(retained.commit.opstamp, initial.commit.opstamp);
    assert_eq!(retained.successful_routes, 0);
    assert_eq!(retained.source_failures.total(), 3);
    assert!(retained
        .source_failures
        .failures()
        .iter()
        .all(|failure| failure.carried_forward));
    assert_eq!(published.load(std::sync::atomic::Ordering::SeqCst), 3);

    let cold_root = tempdir().unwrap();
    let error = refresh_source_backed_generation(
        cold_root.path(),
        &failed_registry,
        WriterOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        SourceBackedCoordinatorError::NoUsableSourceRoutes { failures }
            if failures.total() == 3
                && failures.failures().iter().all(|failure| !failure.carried_forward)
    ));
    assert!(matches!(
        VerifiedIndex::open(cold_root.path()),
        Err(IndexError::MissingActiveGenerationPointer)
    ));
}

#[test]
fn empty_success_does_not_make_a_failed_cold_refresh_usable() {
    let empty_driver = SourceBackedRouteDriver::new(|_| Ok(()), |_| false, |_| false);
    let empty_route = SourceBackedRoute::automatic(
        fixture_provider_source_at(
            CaptureProvider::Gemini,
            GEMINI_CLI_SOURCE_FORMAT,
            ProviderImportSupport::Native,
            "/fixture/empty-success",
        ),
        SourceBackedSelectorAuthority::DiscoveredWinner,
        empty_driver,
    )
    .unwrap();
    let failed_route = fail_route_before_scan(
        fixture_route(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 31),
        SourceBackedRouteErrorKind::Unavailable,
        "failed source",
    );
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(empty_route);
    registry.register(failed_route);
    let temp = tempdir().unwrap();

    assert!(matches!(
        refresh_source_backed_generation(temp.path(), &registry, WriterOptions::default()),
        Err(SourceBackedCoordinatorError::NoUsableSourceRoutes { failures })
            if failures.total() == 1
    ));
    assert!(matches!(
        VerifiedIndex::open(temp.path()),
        Err(IndexError::MissingActiveGenerationPointer)
    ));
}

#[test]
fn terminally_changed_a_is_omitted_and_b_c_publish_after_fail_closed_restart() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let authority_a = Arc::new(std::sync::atomic::AtomicU8::new(1));
    let stable_authority = Arc::new(std::sync::atomic::AtomicU8::new(1));
    let published_a = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let published_b = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let published_c = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let route_a = controlled_revision_route(
        "terminala",
        41,
        1,
        Arc::clone(&authority_a),
        Arc::clone(&log),
        Arc::clone(&published_a),
    )
    .0;
    let route_b = controlled_revision_route(
        "terminalb",
        42,
        1,
        Arc::clone(&stable_authority),
        Arc::clone(&log),
        Arc::clone(&published_b),
    )
    .0;
    let route_c = controlled_revision_route(
        "terminalc",
        43,
        1,
        stable_authority,
        Arc::clone(&log),
        Arc::clone(&published_c),
    )
    .0;
    let changing_authority = Arc::clone(&authority_a);
    let route_c = after_scan(route_c, move || {
        changing_authority.store(2, std::sync::atomic::Ordering::SeqCst);
    });
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(route_a);
    registry.register(route_b);
    registry.register(route_c);
    let temp = tempdir().unwrap();

    let receipt =
        refresh_source_backed_generation(temp.path(), &registry, WriterOptions::default()).unwrap();
    assert_eq!(
        receipt.outcome,
        SourceBackedRefreshOutcome::CompletedWithSourceFailures
    );
    assert_eq!(receipt.successful_routes, 2);
    assert_eq!(receipt.source_failures.total(), 1);
    let failure = &receipt.source_failures.failures()[0];
    assert_eq!(failure.source_selector, "/fixture/terminala");
    assert_eq!(failure.class, SourceBackedSourceFailureClass::SourceChanged);
    assert!(!failure.carried_forward);
    assert_eq!(
        &*log.lock().unwrap(),
        &[
            "terminala",
            "terminalb",
            "terminalc",
            "terminalb",
            "terminalc"
        ]
    );
    let visible = VerifiedIndex::open(temp.path()).unwrap();
    let terminal_a = fixture_source(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 41);
    assert_eq!(visible.document_count(), 2);
    assert!(!visible
        .manifest()
        .sources
        .contains(&controlled_revision_certificate(&terminal_a, 1)));
    assert_eq!(published_a.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(published_b.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(published_c.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn source_failure_receipts_are_row_and_detail_bounded() {
    let mut registry = SourceBackedProviderRegistry::new();
    for route_index in 0..=MAX_RECORDED_SOURCE_BACKED_FAILURES {
        let detail = "x".repeat(MAX_SOURCE_BACKED_FAILURE_DETAIL_BYTES + 100);
        let driver = SourceBackedRouteDriver::new(
            move |_| {
                Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Unavailable,
                    detail.clone(),
                ))
            },
            |_| false,
            |_| false,
        );
        registry.register(
            SourceBackedRoute::automatic(
                fixture_provider_source_at(
                    CaptureProvider::Gemini,
                    GEMINI_CLI_SOURCE_FORMAT,
                    ProviderImportSupport::Native,
                    format!("/fixture/bounded-{route_index}"),
                ),
                SourceBackedSelectorAuthority::DiscoveredWinner,
                driver,
            )
            .unwrap(),
        );
    }
    let temp = tempdir().unwrap();
    let error = refresh_source_backed_generation(temp.path(), &registry, WriterOptions::default())
        .unwrap_err();
    let SourceBackedCoordinatorError::NoUsableSourceRoutes { failures } = error else {
        panic!("unexpected bounded failure result: {error:?}");
    };
    assert_eq!(failures.total(), MAX_RECORDED_SOURCE_BACKED_FAILURES + 1);
    assert_eq!(
        failures.failures().len(),
        MAX_RECORDED_SOURCE_BACKED_FAILURES
    );
    assert_eq!(failures.omitted(), 1);
    assert!(failures.failures().iter().all(|failure| {
        failure.detail.len() <= MAX_SOURCE_BACKED_FAILURE_DETAIL_BYTES
            && failure.source_selector.len() <= MAX_SOURCE_BACKED_FAILURE_SELECTOR_BYTES
    }));
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
    registry.register(fixture_route(
        CaptureProvider::Gemini,
        GEMINI_CLI_SOURCE_FORMAT,
        42,
    ));
    registry.register(fixture_route(
        CaptureProvider::Gemini,
        GEMINI_CLI_SOURCE_FORMAT,
        42,
    ));
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
fn delete_b_then_discover_c_recertifies_replay_and_restart() {
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
    let first_missing =
        refresh_source_backed_generation(temp.path(), &registry, WriterOptions::default()).unwrap();
    assert!(first_missing.removals.is_empty());
    assert_eq!(first_missing.sources.len(), 2);
    assert_eq!(
        first_missing
            .commit
            .manifest()
            .source_catalog()
            .missing_source(&source_b)
            .unwrap()
            .consecutive_missing()
            .get(),
        1
    );

    drop(registry);
    let registry = inventory_replay_registry(Arc::clone(&current));
    let second_missing =
        refresh_source_backed_generation(temp.path(), &registry, WriterOptions::default()).unwrap();
    assert!(second_missing.removals.is_empty());
    assert_eq!(second_missing.sources.len(), 2);
    assert_eq!(
        second_missing
            .commit
            .manifest()
            .source_catalog()
            .missing_source(&source_b)
            .unwrap()
            .consecutive_missing()
            .get(),
        2
    );

    let deleted =
        refresh_source_backed_generation(temp.path(), &registry, WriterOptions::default()).unwrap();
    let deleted_b = deleted
        .removals
        .iter()
        .find(|removal| removal.deletion.source().exact_descriptor_eq(&source_b))
        .expect("B deletion");
    assert!(deleted_b.deletion.verifies(&deleted_b.inventory));

    *current.lock().unwrap() = vec![source_a, source_c];
    let discovered =
        refresh_source_backed_generation(temp.path(), &registry, WriterOptions::default()).unwrap();
    let recertified_b = discovered
        .removals
        .iter()
        .find(|removal| removal.deletion.source().exact_descriptor_eq(&source_b))
        .expect("recertified B deletion");
    assert!(recertified_b.deletion.verifies(&recertified_b.inventory));
    assert_eq!(recertified_b.inventory.observed_sources(), 2);
    assert_ne!(
        deleted_b.inventory.inventory_digest(),
        recertified_b.inventory.inventory_digest()
    );

    let replay =
        refresh_source_backed_generation(temp.path(), &registry, WriterOptions::default()).unwrap();
    assert_eq!(replay.commit.generation_id, discovered.commit.generation_id);
    assert!(replay.removals[0]
        .deletion
        .verifies(&replay.removals[0].inventory));

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
    assert!(restarted.removals[0]
        .deletion
        .verifies(&restarted.removals[0].inventory));
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
