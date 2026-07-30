use super::*;

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
