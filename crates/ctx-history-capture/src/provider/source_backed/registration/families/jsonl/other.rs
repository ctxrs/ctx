use super::*;
use crate::provider::custom_history_jsonl::CustomHistorySourceBackedInput;

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
    let adapter = crate::provider::custom_history_jsonl::custom_history_jsonl_family_adapter(input)
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    let driver = crate::provider::source_backed::family::jsonl::jsonl_family_driver(
        adapter,
        source.path.clone(),
    );
    registry.register(SourceBackedRoute::explicit_manual(
        source,
        SourceBackedSelectorAuthority::CatalogLineage,
        driver,
    )?);
    Ok(())
}
