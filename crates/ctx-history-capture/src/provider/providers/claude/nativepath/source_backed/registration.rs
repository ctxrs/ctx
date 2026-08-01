use super::claude_source_backed_adapter;
use crate::{
    provider::source_backed::{
        executable_route, family::jsonl::jsonl_family_driver, SourceBackedCoordinatorResult,
        SourceBackedProviderRegistry, SourceBackedRouteSelection, SourceBackedSelectorAuthority,
    },
    ProviderSource,
};

pub(crate) fn register(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let driver = jsonl_family_driver(claude_source_backed_adapter(), source.path.clone());
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}
