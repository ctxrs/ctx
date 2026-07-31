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
    assert!(matches!(
        refresh_source_backed_generation(
            tempdir().unwrap().path(),
            &registry,
            WriterOptions::default()
        ),
        Err(SourceBackedCoordinatorError::NoExecutableRoutes)
    ));
}
