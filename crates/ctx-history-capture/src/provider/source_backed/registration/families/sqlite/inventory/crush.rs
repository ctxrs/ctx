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
