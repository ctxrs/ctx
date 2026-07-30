use super::*;
use crate::provider::source_backed::family::document::register_replacement_document_tree_route_with_authority;

/// Registers a Warp database under its stable installed-surface key.
pub fn register_warp_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    surface_key: impl Into<String>,
) -> SourceBackedCoordinatorResult<()> {
    let selected = WarpSourceSelectionV0::new(source.path.clone(), surface_key)
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    let adapter = project_warp_source_backed_v0(selected, resolve_warp_locator_v0)
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    register_replacement_document_tree_route_with_authority(
        registry,
        source,
        selection,
        SourceBackedSelectorAuthority::NamedSurface,
        adapter,
    )
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
    let resolver = GooseSourceBackedResolverV0::new(selected.clone())
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    let adapter = GooseSourceBackedAdapterV0::open(selected, resolver)
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    register_replacement_document_tree_route_with_authority(
        registry,
        source,
        selection,
        SourceBackedSelectorAuthority::SelectedWithRetainedExplicit,
        adapter,
    )
}
