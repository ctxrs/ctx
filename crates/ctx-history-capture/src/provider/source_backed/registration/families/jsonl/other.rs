use super::*;
use crate::ProviderSourceFailureKind;

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
    let claimed_source = owned_source.clone();
    let driver = SourceBackedRouteDriver::new(
        move |sink| {
            let base = sink.base_source(&claimed_source).cloned();
            let mut staging_started = false;
            let outcome = scan_custom_history_source_backed_explicit(
                &scan_input,
                base.as_ref(),
                |disposition, page| {
                    if !staging_started {
                        match disposition {
                            CustomHistorySourceBackedDisposition::Unchanged
                            | CustomHistorySourceBackedDisposition::Append => {
                                let staged = sink
                                    .begin_source_append(claimed_source.clone())
                                    .map_err(capture_coordinator_error)?;
                                if base.as_ref() != Some(staged) {
                                    return Err(CaptureError::InvalidPayload(
                                        "Custom History append base changed before staging"
                                            .to_owned(),
                                    )
                                    .into());
                                }
                            }
                            CustomHistorySourceBackedDisposition::Cold
                            | CustomHistorySourceBackedDisposition::Replacement => {
                                sink.begin_source(claimed_source.clone())
                                    .map_err(capture_coordinator_error)?;
                            }
                        }
                        staging_started = true;
                    }
                    for document in page.documents {
                        sink.add_core_record(document)
                            .map_err(capture_coordinator_error)?;
                    }
                    Ok(())
                },
            )
            .map_err(custom_history_route_error)?;
            let receipt = match outcome {
                CustomHistorySourceBackedOutcome::Missing {
                    inventory,
                    deletion,
                } => {
                    if staging_started {
                        return Err(SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::SourceChanged,
                            "missing Custom History source emitted projection pages",
                        ));
                    }
                    if let Some(deletion) = deletion {
                        sink.delete_source(deletion, inventory)
                            .map_err(route_coordinator_error)?;
                    }
                    return Ok(());
                }
                CustomHistorySourceBackedOutcome::Present(receipt) => receipt,
            };
            if !receipt
                .certificate
                .observation()
                .source()
                .exact_descriptor_eq(&claimed_source)
            {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::SourceChanged,
                    "Custom History scan changed its source descriptor",
                ));
            }
            if !staging_started {
                match receipt.disposition {
                    CustomHistorySourceBackedDisposition::Unchanged
                    | CustomHistorySourceBackedDisposition::Append => {
                        let staged = sink
                            .begin_source_append(claimed_source.clone())
                            .map_err(route_coordinator_error)?;
                        if base.as_ref() != Some(staged) {
                            return Err(SourceBackedRouteError::new(
                                SourceBackedRouteErrorKind::SourceChanged,
                                "Custom History append base changed before empty staging",
                            ));
                        }
                    }
                    CustomHistorySourceBackedDisposition::Cold
                    | CustomHistorySourceBackedDisposition::Replacement => {
                        sink.begin_source(claimed_source.clone())
                            .map_err(route_coordinator_error)?;
                    }
                }
            }
            match receipt.append {
                Some(append)
                    if matches!(
                        receipt.disposition,
                        CustomHistorySourceBackedDisposition::Unchanged
                            | CustomHistorySourceBackedDisposition::Append
                    ) =>
                {
                    if base.as_ref() != Some(append.base())
                        || append.current() != &receipt.certificate
                    {
                        return Err(SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::SourceChanged,
                            "Custom History append evidence changed before certification",
                        ));
                    }
                    sink.certify_source_append(append)
                        .map_err(route_coordinator_error)
                }
                None if matches!(
                    receipt.disposition,
                    CustomHistorySourceBackedDisposition::Cold
                        | CustomHistorySourceBackedDisposition::Replacement
                ) =>
                {
                    sink.certify_source(receipt.certificate)
                        .map_err(route_coordinator_error)
                }
                _ => Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    "Custom History disposition and append evidence disagree",
                )),
            }
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
    );
    registry.register(SourceBackedRoute::explicit_manual(
        source,
        SourceBackedSelectorAuthority::CatalogLineage,
        driver,
    )?);
    Ok(())
}

fn custom_history_route_error(error: CustomHistorySourceBackedError) -> SourceBackedRouteError {
    let kind = match &error {
        CustomHistorySourceBackedError::Capture(CaptureError::ProviderSource {
            kind: ProviderSourceFailureKind::SchemaIncompatible,
            ..
        }) => SourceBackedRouteErrorKind::Unsupported,
        _ => SourceBackedRouteErrorKind::InvalidSource,
    };
    SourceBackedRouteError::new(kind, error.to_string())
}
