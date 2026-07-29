use super::*;

pub(super) fn register_zed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let path = source.path.clone();
    let capture_path = path.clone();
    let revalidation_path = path.clone();
    let hydration_path = path;
    let driver = SourceBackedRouteDriver::new(
        move |sink| {
            let source_key = zed_source_key().map_err(route_error)?;
            let mut snapshot = acquire_zed_snapshot(&capture_path).map_err(route_error)?;
            let snapshot_revision = snapshot.snapshot_revision.clone();
            let physical_locator = snapshot.physical_locator.clone();
            let revision_digest = zed_snapshot_revision_digest(&snapshot_revision);
            sink.begin_source(source_key.clone())
                .map_err(route_coordinator_error)?;
            let connection = snapshot.connection().map_err(route_error)?;
            let mut zed_sink = ZedSourceBackedSinkV0::new(
                sink.writer,
                connection,
                source_key.clone(),
                revision_digest,
                capture_path.to_string_lossy().into_owned(),
            )
            .map_err(route_error)?;
            let scan = scan_zed_native_snapshot(
                connection,
                &physical_locator,
                &snapshot_revision,
                &mut zed_sink,
            )
            .map_err(route_error)?;
            if let Some(error) = zed_sink.take_failure() {
                return Err(route_error(error));
            }
            let staged_documents = zed_sink.staged_documents();
            drop(zed_sink);
            snapshot.finish().map_err(route_error)?;
            if staged_documents != scan.counters.retained_events {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    "Zed source-backed counts do not reconcile",
                ));
            }
            let complete_records = scan
                .counters
                .retained_events
                .checked_add(scan.counters.rejected_threads)
                .ok_or_else(|| {
                    SourceBackedRouteError::new(
                        SourceBackedRouteErrorKind::Internal,
                        "Zed source-backed counts overflowed",
                    )
                })?;
            let counts = ScannedSourceCounts {
                complete_records,
                retained_records: scan.counters.retained_events,
                rejected_records: scan.counters.rejected_threads,
                ignored_records: 0,
                indexed_documents: staged_documents,
                certified_bytes: scan.counters.certified_logical_bytes,
            };
            let observation =
                zed_source_observation(&source_key, &snapshot_revision).map_err(route_error)?;
            let certificate = CertifiedSource::certify(
                observation.clone(),
                observation,
                "zed-nativepath-source-backed-v0",
                decode_zed_digest(&scan.source_integrity_digest).map_err(route_error)?,
                counts,
            )
            .map_err(route_error)?;
            sink.certify_source(certificate)
                .map_err(route_coordinator_error)
        },
        provider_format_scope(CaptureProvider::Zed, "zed_threads_sqlite"),
        move |target| match target {
            SourceBackedRevalidationTarget::Source(expected) => {
                let Ok(source_key) = zed_source_key() else {
                    return false;
                };
                let Ok(mut snapshot) = acquire_zed_snapshot(&revalidation_path) else {
                    return false;
                };
                let observation = zed_source_observation(&source_key, &snapshot.snapshot_revision);
                observation.is_ok_and(|observation| observation == *expected.observation())
                    && snapshot.finish().is_ok()
            }
            SourceBackedRevalidationTarget::Deletion(_) => false,
        },
        move |request| {
            ZedLocatorResolverV0::new(&hydration_path)
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
                })?
                .hydrate_event(request)
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

pub(super) fn register_pi_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let root = match selection {
        SourceBackedRouteSelection::Automatic => {
            PiSourceBackedRoot::winning(source.path.clone())
                .map_err(|error| invalid_route(source.provider, error.to_string()))?
        }
        SourceBackedRouteSelection::ExplicitManual => {
            PiSourceBackedRoot::explicit(source.path.clone())
        }
    };
    let context = ProviderAdapterContext {
        machine_id: "source-backed-pi".to_owned(),
        source_path: Some(source.path.clone()),
        source_root: Some(source.path.clone()),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let capture_root = root.clone();
    let capture_context = context.clone();
    let hydration_root = root;
    let hydration_context = context;
    let driver = captured_route_driver(
        &source,
        move |sink| {
            let mut begun = HashSet::new();
            let mut sink_failure = None;
            let projection = project_pi_source_backed_root_cold(
                &capture_root,
                capture_context.clone(),
                |page| {
                    if sink_failure.is_some() {
                        return;
                    }
                    if begun.insert(page.source.identity().digest()) {
                        if let Err(error) = sink.begin(page.source) {
                            sink_failure = Some(error);
                            return;
                        }
                    }
                    for document in page.documents {
                        if let Err(error) = sink.document(document) {
                            sink_failure = Some(error);
                            return;
                        }
                    }
                },
            )
            .map_err(route_error)?;
            if let Some(error) = sink_failure {
                return Err(error);
            }
            for source in projection.sources {
                if begun.insert(source.route.source.identity().digest()) {
                    sink.begin(source.route.source)?;
                }
                sink.certify(source.certificate)?;
            }
            let _inventory = projection.inventory;
            Ok(())
        },
        provider_format_scope(CaptureProvider::Pi, "pi_session_jsonl"),
        move |request| {
            let projection = project_pi_source_backed_root_cold(
                &hydration_root,
                hydration_context.clone(),
                |_| {},
            )
            .map_err(|error| {
                hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
            })?;
            PiSourceBackedResolver::new(projection.sources.into_iter().map(|source| source.route))
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
                })?
                .hydrate_event(request)
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

pub(super) fn register_forgecode_selected_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    if selection != SourceBackedRouteSelection::Automatic {
        return Err(invalid_route(
            source.provider,
            "manual ForgeCode registration requires explicit catalog lineage",
        ));
    }
    register_forgecode_route(
        registry,
        source,
        selection,
        ForgeCodeSourceSelectionV0::selected,
    )
}

pub fn register_forgecode_explicit_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    catalog_lineage: [u8; 32],
) -> SourceBackedCoordinatorResult<()> {
    register_forgecode_route(
        registry,
        source,
        SourceBackedRouteSelection::ExplicitManual,
        move |path| ForgeCodeSourceSelectionV0::explicit(path, catalog_lineage),
    )
}

/// Registers one caller-owned Custom History JSONL route. The path is only a
/// resolver location; `catalog_lineage` remains the durable source identity.
pub fn register_custom_history_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    catalog_lineage: [u8; 32],
) -> SourceBackedCoordinatorResult<()> {
    let input = CustomHistorySourceBackedInput::explicit(source.path.clone(), catalog_lineage);
    let owned_source = input
        .source_key()
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    let scan_input = input.clone();
    let revalidation_input = input.clone();
    let hydration_input = input;
    let claimed_source = owned_source.clone();
    let driver = SourceBackedRouteDriver::new(
        move |sink| {
            let opening =
                observe_custom_history_source_backed_explicit(&scan_input).map_err(route_error)?;
            let base = sink.base_source(&claimed_source).cloned();
            if opening.is_missing() {
                let outcome =
                    scan_custom_history_source_backed_explicit(&scan_input, base.as_ref(), |_| {
                        Ok(())
                    })
                    .map_err(route_error)?;
                let CustomHistorySourceBackedOutcome::Missing {
                    inventory,
                    deletion,
                } = outcome
                else {
                    return Err(SourceBackedRouteError::new(
                        SourceBackedRouteErrorKind::SourceChanged,
                        "Custom History source appeared after its opening observation",
                    ));
                };
                if let Some(deletion) = deletion {
                    sink.delete_source(deletion, inventory)
                        .map_err(route_coordinator_error)?;
                }
                return Ok(());
            }

            sink.begin_source(claimed_source.clone())
                .map_err(route_coordinator_error)?;
            let outcome = scan_custom_history_source_backed_explicit(&scan_input, None, |page| {
                for document in page.documents {
                    sink.add_document(document)
                        .map_err(capture_coordinator_error)?;
                }
                Ok(())
            })
            .map_err(route_error)?;
            let CustomHistorySourceBackedOutcome::Present(receipt) = outcome else {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::SourceChanged,
                    "Custom History source disappeared during its replacement scan",
                ));
            };
            if !matches!(
                receipt.disposition,
                CustomHistorySourceBackedDisposition::Cold
            ) {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    "cold Custom History scan returned a non-cold disposition",
                ));
            }
            sink.certify_source(receipt.certificate)
                .map_err(route_coordinator_error)
        },
        move |candidate| candidate.exact_descriptor_eq(&owned_source),
        move |target| match target {
            SourceBackedRevalidationTarget::Source(certificate) => {
                revalidate_custom_history_source_backed(&revalidation_input, certificate)
                    .unwrap_or(false)
            }
            SourceBackedRevalidationTarget::Deletion(deletion) => {
                let Ok(opening) =
                    observe_custom_history_source_backed_explicit(&revalidation_input)
                else {
                    return false;
                };
                let Ok(closing) =
                    observe_custom_history_source_backed_explicit(&revalidation_input)
                else {
                    return false;
                };
                opening
                    .certify_against(&closing)
                    .is_ok_and(|inventory| deletion.verifies(&inventory))
            }
        },
        move |request| {
            let outcome =
                scan_custom_history_source_backed_explicit(&hydration_input, None, |_| Ok(()))
                    .map_err(|error| {
                        hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
                    })?;
            let CustomHistorySourceBackedOutcome::Present(receipt) = outcome else {
                return Err(hydration_failure(
                    HydrationFailureKind::ConfirmedDeleted,
                    "the explicit Custom History source is absent",
                ));
            };
            CustomHistorySourceBackedResolver::new([receipt.route])
                .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error))?
                .hydrate_event(request)
        },
    );
    registry.register(SourceBackedRoute::explicit_manual(
        source,
        SourceBackedSelectorAuthority::CatalogLineage,
        driver,
    )?);
    Ok(())
}

/// Registers one explicit NanoClaw compound project with caller-owned catalog
/// lineage.
pub fn register_nanoclaw_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    catalog_lineage: [u8; 32],
) -> SourceBackedCoordinatorResult<()> {
    let path = source.path.clone();
    let capture_path = path.clone();
    let hydration_path = path;
    let owned_source = nanoclaw_source_key(catalog_lineage)
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    let driver = captured_route_driver(
        &source,
        move |sink| {
            sink.begin(owned_source.clone())?;
            let receipt =
                scan_nanoclaw_source_backed(&capture_path, catalog_lineage, |page| {
                    for document in page.documents {
                        sink.document(document).map_err(|error| {
                            crate::provider::providers::nanoclaw::native_path::source_backed::NanoClawSourceBackedError::Capture(
                                CaptureError::InvalidPayload(error.to_string()),
                            )
                        })?;
                    }
                    Ok(())
                })
                .map_err(route_error)?;
            sink.certify(receipt.source)
        },
        provider_format_scope(CaptureProvider::NanoClaw, "nanoclaw_project"),
        move |request| {
            let record = hydrate_nanoclaw_source_backed_exact(
                &hydration_path,
                catalog_lineage,
                request.locator(),
            )
            .map_err(|error| hydration_failure(HydrationFailureKind::StaleRecordEvidence, error))?;
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes: record.text.into_bytes(),
            })
        },
    );
    registry.register(SourceBackedRoute::explicit_manual(
        source,
        SourceBackedSelectorAuthority::CatalogLineage,
        driver,
    )?);
    Ok(())
}

/// Registers a Warp database under its stable installed-surface key.
pub fn register_warp_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    surface_key: impl Into<String>,
) -> SourceBackedCoordinatorResult<()> {
    let selected = WarpSourceSelectionV0::new(source.path.clone(), surface_key)
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    let capture_selection = selected.clone();
    let hydration_selection = selected;
    let driver = captured_route_driver(
        &source,
        move |sink| {
            let snapshot =
                project_warp_source_backed_v0(capture_selection.clone()).map_err(route_error)?;
            sink.begin(snapshot.source)?;
            for document in snapshot.documents {
                sink.document(document)?;
            }
            sink.certify(snapshot.certified_source)
        },
        provider_format_scope(CaptureProvider::Warp, "warp_sqlite"),
        move |request| {
            let hydrated = resolve_warp_locator_v0(&hydration_selection, request.locator())
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::StaleRecordEvidence, error)
                })?;
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes: hydrated.provider_bytes,
            })
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::NamedSurface,
        driver,
    )?);
    Ok(())
}

/// Registers Goose's selected database and the exact platform root needed to
/// resolve attachments. Historical routes are retained only when supplied.
pub fn register_goose_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    platform_root: impl Into<std::path::PathBuf>,
    retained_routes: Vec<(std::path::PathBuf, std::path::PathBuf)>,
) -> SourceBackedCoordinatorResult<()> {
    let mut selected =
        GooseSourceBackedSelectionV0::exact(source.path.clone(), platform_root.into());
    if !retained_routes.is_empty() {
        selected = selected
            .with_explicit_retained_routes(
                retained_routes
                    .into_iter()
                    .map(|(database, root)| GooseSourceRouteV0::exact(database, root))
                    .collect(),
            )
            .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    }
    let capture_selection = selected.clone();
    let hydration_selection = selected;
    let driver = captured_route_driver(
        &source,
        move |sink| {
            let adapter =
                GooseSourceBackedAdapterV0::open(capture_selection.clone()).map_err(route_error)?;
            sink.begin(adapter.source().clone())?;
            let mut scan = adapter.scan().map_err(route_error)?;
            while let Some(page) = scan.next_page().map_err(route_error)? {
                for document in page.into_documents() {
                    sink.document(document)?;
                }
            }
            sink.certify(scan.finish().map_err(route_error)?.certificate().clone())
        },
        provider_format_scope(CaptureProvider::Goose, "goose_sessions_sqlite"),
        move |request| {
            GooseSourceBackedResolverV0::new(hydration_selection.clone())
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
                })?
                .hydrate_event(request)
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::SelectedWithRetainedExplicit,
        driver,
    )?);
    Ok(())
}

/// Registers the finite Lingma database inventory supplied by product
/// discovery. Database lineage and inventory authority are caller-owned.
pub fn register_lingma_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    authority_key: TypedKey,
    databases: Vec<(std::path::PathBuf, TypedKey)>,
) -> SourceBackedCoordinatorResult<()> {
    let databases = databases
        .into_iter()
        .map(|(path, lineage)| LingmaDatabaseSourceV0::new(path, lineage))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    let inventory = LingmaSourceInventoryV0::new(authority_key, databases)
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    register_lingma_inventory_source(
        registry,
        source,
        selection,
        Arc::new(FixedLingmaInventorySource { inventory }),
    )
}

pub(in crate::provider::source_backed) trait LingmaInventorySource:
    Send + Sync
{
    fn observe(&self) -> LingmaSourceBackedResultV0<LingmaSourceInventoryV0>;
}

#[derive(Debug, Clone)]
struct FixedLingmaInventorySource {
    inventory: LingmaSourceInventoryV0,
}

impl LingmaInventorySource for FixedLingmaInventorySource {
    fn observe(&self) -> LingmaSourceBackedResultV0<LingmaSourceInventoryV0> {
        Ok(self.inventory.clone())
    }
}

#[derive(Debug, Clone)]
struct DiscoveredLingmaInventorySource {
    selector: LingmaInventorySelector,
}

impl LingmaInventorySource for DiscoveredLingmaInventorySource {
    fn observe(&self) -> LingmaSourceBackedResultV0<LingmaSourceInventoryV0> {
        self.selector
            .observe()
            .map_err(lingma_discovery_adapter_error)
            .and_then(lingma_adapter_inventory)
    }
}

pub(in crate::provider::source_backed) fn register_lingma_inventory_source(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    inventory_source: Arc<dyn LingmaInventorySource>,
) -> SourceBackedCoordinatorResult<()> {
    let capture_inventory = Arc::clone(&inventory_source);
    let hydration_inventory = Arc::clone(&inventory_source);
    let batch_hydration_inventory = inventory_source;
    let driver = captured_route_driver(
        &source,
        move |sink| {
            let opening = capture_inventory.observe().map_err(route_error)?;
            let closing_inventory = Arc::clone(&capture_inventory);
            let scan = scan_lingma_source_backed_v0(opening, move || closing_inventory.observe())
                .map_err(route_error)?;
            for database in scan.databases() {
                sink.begin(database.certificate().observation().source().clone())?;
                for record in database.records() {
                    sink.document(record.document().clone())?;
                }
                sink.certify(database.certificate().clone())?;
            }
            Ok(())
        },
        provider_format_scope(CaptureProvider::Lingma, "lingma_sqlite"),
        move |request| {
            let inventory = hydration_inventory.observe().map_err(|error| {
                hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
            })?;
            LingmaSourceBackedResolverV0::new(&inventory)
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
                })?
                .hydrate_event(request)
        },
    )
    .with_batch_hydration(move |request| {
        let inventory = batch_hydration_inventory.observe().map_err(|error| {
            hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
        })?;
        LingmaSourceBackedResolverV0::new(&inventory)
            .map_err(|error| {
                hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
            })?
            .hydrate_batch_request(request)
    });
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

pub(in crate::provider::source_backed) fn discovered_lingma_inventory_source(
    discovery: &DiscoveryContext,
    selected_source: &ProviderSource,
) -> Result<Arc<dyn LingmaInventorySource>, SourceBackedAutomaticUnavailableReason> {
    let source = DiscoveredLingmaInventorySource {
        selector: LingmaInventorySelector::new(discovery.clone()),
    };
    let opening = source.selector.observe().map_err(|error| {
        SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable {
            detail: error.detail(),
        }
    })?;
    if !opening
        .databases()
        .iter()
        .any(|database| database.source() == selected_source)
    {
        return Err(
            SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable {
                detail: "Lingma selected database is absent from its installed-client inventory",
            },
        );
    }
    lingma_adapter_inventory(opening).map_err(|error| {
        SourceBackedAutomaticUnavailableReason::RegistrationRejected {
            detail: error.to_string(),
        }
    })?;
    Ok(Arc::new(source))
}

fn lingma_adapter_inventory(
    inventory: crate::provider_sources::LingmaDiscoveredInventory,
) -> LingmaSourceBackedResultV0<LingmaSourceInventoryV0> {
    let authority_key = inventory
        .authority_key()
        .map_err(lingma_discovery_adapter_error)?;
    let databases = inventory
        .databases()
        .iter()
        .map(|database| {
            let lineage = database
                .catalog_lineage()
                .typed_key()
                .map_err(lingma_discovery_adapter_error)?;
            LingmaDatabaseSourceV0::new(database.path(), lineage)
        })
        .collect::<LingmaSourceBackedResultV0<Vec<_>>>()?;
    LingmaSourceInventoryV0::new(authority_key, databases)
}

fn lingma_discovery_adapter_error(error: LingmaDiscoveryUnavailable) -> LingmaSourceBackedErrorV0 {
    CaptureError::InvalidPayload(error.to_string()).into()
}
