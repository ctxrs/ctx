use super::*;
use ctx_history_provider_runtime::ProviderRouteRegistrar;

macro_rules! register_shared_jsonl_route {
    ($registry:expr, $source:expr, $selection:expr, $adapter:expr) => {{
        let adapter =
            $adapter.map_err(|error| invalid_route($source.provider, error.to_string()))?;
        let driver = crate::provider::source_backed::family::jsonl::jsonl_family_driver(
            adapter,
            $source.path.clone(),
        );
        $registry.register(executable_route(
            $source,
            $selection,
            SourceBackedSelectorAuthority::DiscoveredWinner,
            driver,
        )?);
        Ok(())
    }};
}

pub(super) fn register_deepseek_harness_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    register_shared_jsonl_route!(
        registry,
        source,
        selection,
        ctx_history_providers_jsonl_shared::adapters::deepseek_harness_with_source_root_lineage::<
            crate::provider::source_backed::family::jsonl::JsonlFamilyRuntime,
        >(source.source_format, source_root_lineage)
    )
}

pub(super) fn register_fx_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    let driver = crate::provider::source_backed::family::jsonl::jsonl_family_driver(
        ctx_history_provider_fx::fx_sessions_tree_adapter::<CaptureProviderRuntime>(
            source_root_lineage,
        ),
        source.path.clone(),
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::CatalogLineage,
        driver,
    )?);
    Ok(())
}

/// Registers Cursor's thin adapter over the shared certified-append JSONL
/// lifecycle.
pub fn register_cursor_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    let driver = crate::provider::source_backed::family::jsonl::jsonl_family_driver(
        ctx_history_provider_claude_cursor::cursor_jsonl_adapter_with_source_root_lineage::<
            CaptureProviderRuntime,
        >(source_root_lineage),
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
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    register_shared_jsonl_route!(
        registry,
        source,
        selection,
        Ok::<_, crate::CaptureError>(
            ctx_history_providers_jsonl_shared::adapters::junie_with_source_root_lineage::<
                crate::provider::source_backed::family::jsonl::JsonlFamilyRuntime,
            >(source_root_lineage)
        )
    )
}

pub(super) fn register_kimi_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    register_shared_jsonl_route!(
        registry,
        source,
        selection,
        Ok::<_, crate::CaptureError>(
            ctx_history_providers_jsonl_shared::adapters::kimi_with_source_root_lineage::<
                crate::provider::source_backed::family::jsonl::JsonlFamilyRuntime,
            >(source_root_lineage)
        )
    )
}
pub(super) fn register_mistral_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    let driver = crate::provider::source_backed::family::jsonl::jsonl_family_driver(
        mistral_vibe_jsonl_adapter_with_source_root_lineage::<CaptureProviderRuntime>(
            source_root_lineage,
        ),
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
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    if source.status == ProviderSourceStatus::Unsupported {
        return Err(invalid_route(
            source.provider,
            source
                .unsupported_reason
                .unwrap_or("unsupported OpenClaw history format"),
        ));
    }
    let driver = crate::provider::source_backed::family::jsonl::jsonl_family_driver(
        ctx_history_providers_jsonl_shared::adapters::openclaw_with_source_root_lineage::<
            crate::provider::source_backed::family::jsonl::JsonlFamilyRuntime,
        >(source_root_lineage),
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
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    let driver = crate::provider::source_backed::family::jsonl::jsonl_family_driver(
        mux_jsonl_adapter_with_source_root_lineage::<CaptureProviderRuntime>(source_root_lineage),
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
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    let (root, adapter) =
        ctx_history_providers_jsonl_shared::adapters::pi_with_source_root_lineage::<
            crate::provider::source_backed::family::jsonl::JsonlFamilyRuntime,
        >(
            source.path.clone(),
            matches!(selection, SourceBackedRouteSelection::Automatic),
            source_root_lineage,
        )
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    let driver = crate::provider::source_backed::family::jsonl::jsonl_family_driver(adapter, root);
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
    let provider = source.provider;
    let registration = ctx_history_providers_jsonl_shared::custom_history_explicit_route::<
        CaptureProviderRuntime,
    >(source, catalog_lineage)
    .map_err(|error| invalid_route(provider, error.to_string()))?;
    registry.register_provider_route(registration)
}
