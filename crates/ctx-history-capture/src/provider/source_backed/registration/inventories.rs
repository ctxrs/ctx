use super::*;

/// Registers Crush's selector-owned finite project inventory. The coordinator
/// consumes the adapter's existing scan helpers but remains the only owner of
/// `GenerationWriter` and commit.
pub fn register_crush_source_backed_route<I>(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    inventory_source: Arc<I>,
) -> SourceBackedCoordinatorResult<()>
where
    I: CrushProjectInventorySourceV0 + Send + Sync + 'static,
{
    let scan_inventory = Arc::clone(&inventory_source);
    let revalidation_inventory = Arc::clone(&inventory_source);
    let complete_inventory_revalidation = Arc::clone(&inventory_source);
    let hydration_inventory = inventory_source;
    let driver = SourceBackedRouteDriver::new(
        move |sink| {
            let opening = bind_crush_inventory(scan_inventory.observe().map_err(route_error)?)
                .map_err(route_error)?;
            let base_sources = sink
                .writer
                .base_manifest()
                .map(|manifest| {
                    manifest
                        .sources
                        .iter()
                        .cloned()
                        .map(|certificate| {
                            (certificate.observation().source().clone(), certificate)
                        })
                        .collect::<HashMap<_, _>>()
                })
                .unwrap_or_default();

            for database in &opening.databases {
                let opened = open_crush_source(database.clone()).map_err(route_error)?;
                let base = base_sources.get(&database.source_key);
                if base.is_some_and(|base| crush_exact_replay_matches(base, &opened)) {
                    if !finish_crush_source(opened).map_err(route_error)? {
                        return Err(SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::SourceChanged,
                            "Crush source changed while its replay was staged",
                        ));
                    }
                    let base = base.ok_or_else(|| {
                        SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::Internal,
                            "Crush replay base disappeared",
                        )
                    })?;
                    let writer_base = sink
                        .begin_source_append(database.source_key.clone())
                        .map_err(route_coordinator_error)?;
                    if writer_base != base {
                        return Err(SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::SourceChanged,
                            "Crush replay base changed inside the shared writer",
                        ));
                    }
                    let frontier = base.frontier().ok_or_else(|| {
                        SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::InvalidSource,
                            "Crush replay base has no exact frontier",
                        )
                    })?;
                    sink.certify_source_append(
                        CertifiedSourceAppend::certify(
                            base,
                            base.clone(),
                            frontier.certified_prefix_bytes(),
                            *frontier.certified_prefix_digest(),
                        )
                        .map_err(route_error)?,
                    )
                    .map_err(route_coordinator_error)?;
                } else {
                    sink.begin_source(database.source_key.clone())
                        .map_err(route_coordinator_error)?;
                    let scan = scan_crush_source(&opened, sink.writer).map_err(route_error)?;
                    let closing = closing_crush_observation(&opened).map_err(route_error)?;
                    let opening_observation = opened.observation.clone();
                    if !finish_crush_source(opened).map_err(route_error)? {
                        return Err(SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::SourceChanged,
                            "Crush source changed while its replacement was staged",
                        ));
                    }
                    let frontier = SourceFrontier::new(
                        CRUSH_FRONTIER_KIND,
                        TypedKey::bytes(opening_observation.revision().to_vec())
                            .map_err(route_error)?,
                        scan.counts.certified_bytes,
                        scan.content_digest,
                    )
                    .map_err(route_error)?;
                    let certificate = CertifiedSource::certify_with_frontier(
                        opening_observation,
                        closing,
                        CRUSH_PARSER_REVISION,
                        scan.content_digest,
                        scan.counts,
                        Some(frontier),
                    )
                    .map_err(route_error)?;
                    sink.certify_source(certificate)
                        .map_err(route_coordinator_error)?;
                }
            }

            let closing_observation = scan_inventory.observe().map_err(route_error)?;
            if !opening
                .matches(closing_observation.clone())
                .map_err(route_error)?
            {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::SourceChanged,
                    "Crush project inventory changed during shared staging",
                ));
            }
            let closing = bind_crush_inventory(closing_observation).map_err(route_error)?;
            let certified_inventory = CertifiedSourceInventory::certify(
                opening.observation.clone(),
                closing.observation,
                CRUSH_DISCOVERY_REVISION,
                opening.source_keys(),
            )
            .map_err(route_error)?;
            sink.certify_complete_inventory(certified_inventory.clone())
                .map_err(route_coordinator_error)?;
            for base in base_sources.values() {
                let base_source = base.observation().source();
                if base_source.provider() == CaptureProvider::Crush.as_str()
                    && base_source.source_format() == "crush_sqlite"
                    && base_source.schema_variant() == CRUSH_SOURCE_SCHEMA_VARIANT
                    && !opening.contains_exact_source(base_source)
                {
                    sink.delete_source(
                        CertifiedSourceDeletion::from_inventory(
                            base_source.clone(),
                            &certified_inventory,
                        )
                        .map_err(route_error)?,
                        certified_inventory.clone(),
                    )
                    .map_err(route_coordinator_error)?;
                }
            }
            Ok(())
        },
        provider_format_scope(CaptureProvider::Crush, "crush_sqlite"),
        move |target| match target {
            SourceBackedRevalidationTarget::Source(expected) => {
                let Ok(observation) = revalidation_inventory.observe() else {
                    return false;
                };
                let Ok(inventory) = bind_crush_inventory(observation) else {
                    return false;
                };
                let Some(database) = inventory.databases.iter().find(|database| {
                    database
                        .source_key
                        .exact_descriptor_eq(expected.observation().source())
                }) else {
                    return false;
                };
                let Ok(opened) = open_crush_source(database.clone()) else {
                    return false;
                };
                let observation_matches = opened.observation == *expected.observation();
                observation_matches && finish_crush_source(opened).unwrap_or(false)
            }
            SourceBackedRevalidationTarget::Deletion(deletion) => {
                let Ok(opening_observation) = revalidation_inventory.observe() else {
                    return false;
                };
                let Ok(opening) = bind_crush_inventory(opening_observation.clone()) else {
                    return false;
                };
                let Ok(closing_observation) = revalidation_inventory.observe() else {
                    return false;
                };
                if !opening
                    .matches(closing_observation.clone())
                    .unwrap_or(false)
                {
                    return false;
                }
                let Ok(closing) = bind_crush_inventory(closing_observation) else {
                    return false;
                };
                let source_keys = opening.source_keys();
                CertifiedSourceInventory::certify(
                    opening.observation,
                    closing.observation,
                    CRUSH_DISCOVERY_REVISION,
                    source_keys,
                )
                .is_ok_and(|inventory| deletion.verifies(&inventory))
            }
        },
        move |request| {
            let hydrated = CrushLocatorResolverV0::discover(hydration_inventory.as_ref())
                .and_then(|resolver| resolver.hydrate(request.locator()))
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::StaleRecordEvidence, error)
                })?;
            let provider_bytes = hydrated
                .decoded_display_text
                .ok_or_else(|| {
                    hydration_failure(
                        HydrationFailureKind::UnsupportedParserRevision,
                        "Crush record has no exact display text",
                    )
                })?
                .into_bytes();
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes,
            })
        },
    )
    .with_complete_inventory_revalidation(move |expected| {
        let Ok(opening_observation) = complete_inventory_revalidation.observe() else {
            return false;
        };
        let Ok(opening) = bind_crush_inventory(opening_observation.clone()) else {
            return false;
        };
        let Ok(closing_observation) = complete_inventory_revalidation.observe() else {
            return false;
        };
        if !opening
            .matches(closing_observation.clone())
            .unwrap_or(false)
        {
            return false;
        }
        let Ok(closing) = bind_crush_inventory(closing_observation) else {
            return false;
        };
        let source_keys = opening.source_keys();
        CertifiedSourceInventory::certify(
            opening.observation,
            closing.observation,
            CRUSH_DISCOVERY_REVISION,
            source_keys,
        )
        .is_ok_and(|current| current == *expected)
    });
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::SelectedWithRetainedExplicit,
        driver,
    )?);
    Ok(())
}

/// Registers AstrBot's complete selected/launcher inventory from the same
/// bounded discovery context used by provider selection.
pub fn register_astrbot_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    discovery: DiscoveryContext,
) -> SourceBackedCoordinatorResult<()> {
    let capture_discovery = discovery.clone();
    let hydration_discovery = discovery.clone();
    let batch_hydration_discovery = discovery;
    let driver = captured_route_driver(
        move |sink| {
            let opening = AstrBotSourceBackedInventoryV0::discover(&capture_discovery)
                .map_err(route_error)?;
            for selected in opening.sources() {
                sink.begin(selected.source_key().clone())?;
                let certificate =
                    scan_astrbot_source_backed_v0(selected, &mut |document| {
                        sink.document(document).map_err(|error| {
                            crate::provider::providers::astrbot::native_path::source_backed::AstrBotSourceBackedErrorV0::Capture(
                                CaptureError::InvalidPayload(error.to_string()),
                            )
                        })
                    })
                    .map_err(route_error)?;
                sink.certify(certificate)?;
            }
            let closing = AstrBotSourceBackedInventoryV0::discover(&capture_discovery)
                .map_err(route_error)?;
            opening.certify(&closing).map_err(route_error)?;
            Ok(())
        },
        provider_format_scope(CaptureProvider::AstrBot, "astrbot_data_v4_sqlite"),
        move |request| {
            let inventory = AstrBotSourceBackedInventoryV0::discover(&hydration_discovery)
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
                })?;
            AstrBotSourceBackedResolverV0::from_inventory(&inventory)
                .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error))?
                .hydrate_event(request)
        },
    )
    .with_batch_hydration(move |request| {
        let inventory = AstrBotSourceBackedInventoryV0::discover(&batch_hydration_discovery)
            .map_err(|error| {
                hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
            })?;
        AstrBotSourceBackedResolverV0::from_inventory(&inventory)
            .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error))?
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

/// Registers Shelley only when the caller retains the exact CWD that selected
/// `shelley.db`. No branch or fallback CWD is inferred.
pub fn register_shelley_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    exact_cwd: impl Into<std::path::PathBuf>,
) -> SourceBackedCoordinatorResult<()> {
    let exact_cwd = exact_cwd.into();
    let adapter = discover_shelley_source_backed_exact_cwd(&exact_cwd)
        .map_err(|error| invalid_route(source.provider, error.to_string()))?
        .ok_or_else(|| {
            invalid_route(
                source.provider,
                "the exact Shelley CWD no longer contains an admitted database",
            )
        })?;
    if adapter.database_path() != source.path {
        return Err(invalid_route(
            source.provider,
            "the Shelley source path does not belong to the supplied exact CWD",
        ));
    }
    register_shelley_adapter(registry, source, adapter)
}

fn register_shelley_adapter(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    adapter: ShelleySourceBackedAdapter,
) -> SourceBackedCoordinatorResult<()> {
    let capture_adapter = adapter.clone();
    let hydration_adapter = adapter;
    let driver = captured_route_driver(
        move |sink| {
            sink.begin(capture_adapter.source().clone())?;
            let mut scan = capture_adapter.start_scan().map_err(route_error)?;
            while let Some(page) = scan.next_page().map_err(route_error)? {
                for document in page.documents {
                    sink.document(document)?;
                }
            }
            sink.certify(scan.finish().map_err(route_error)?.certificate)
        },
        provider_format_scope(CaptureProvider::Shelley, "shelley_sqlite"),
        move |request| {
            let hydrated = hydration_adapter
                .hydrate(request.locator())
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::StaleRecordEvidence, error)
                })?;
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes: hydrated.text.into_bytes(),
            })
        },
    );
    registry.register(SourceBackedRoute::automatic(
        source,
        SourceBackedSelectorAuthority::ExactCwd,
        driver,
    )?);
    Ok(())
}

/// Registers an inactive Hermes database only with a caller-owned persistent
/// anchor. Automatic profile routes continue to use provider-native profile
/// identity.
pub fn register_hermes_explicit_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    anchor: SourceAnchor,
) -> SourceBackedCoordinatorResult<()> {
    let candidate = hermes_source_backed_explicit(source.path.clone(), anchor)
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    register_hermes_candidate(
        registry,
        source,
        SourceBackedRouteSelection::ExplicitManual,
        candidate,
        SourceBackedSelectorAuthority::ExplicitPath,
    )
}
