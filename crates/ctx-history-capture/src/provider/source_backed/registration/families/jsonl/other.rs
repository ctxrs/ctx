use super::*;

/// Registers Cursor's thin adapter over the shared replacement-only JSONL
/// lifecycle.
pub fn register_cursor_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let driver = crate::provider::source_backed::family::jsonl::jsonl_family_driver(
        crate::provider::providers::cursor::cursor_jsonl_adapter(),
        source.path.clone(),
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

pub(super) fn register_junie_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let driver = crate::provider::source_backed::family::jsonl::jsonl_family_driver(
        junie_jsonl_adapter(),
        source.path.clone(),
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

pub(super) fn register_kimi_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let adapter: KimiSourceBackedResolver = KimiSourceBackedCatalog::shared();
    let driver = crate::provider::source_backed::family::jsonl::jsonl_family_driver(
        adapter.into_shared(),
        source.path.clone(),
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}
pub(super) fn register_mistral_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let driver = crate::provider::source_backed::family::jsonl::jsonl_family_driver(
        scan_mistral_vibe_source_backed(),
        source.path.clone(),
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

pub(super) fn register_openclaw_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let driver = crate::provider::source_backed::family::jsonl::jsonl_family_driver(
        openclaw_source_backed_adapter_v0(),
        source.path.clone(),
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}
pub(super) fn register_mux_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let driver = crate::provider::source_backed::family::jsonl::jsonl_family_driver(
        mux_jsonl_adapter(),
        source.path.clone(),
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
    let driver = crate::provider::source_backed::family::jsonl::jsonl_family_driver(
        pi_source_backed_adapter(),
        root.path().to_path_buf(),
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
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
