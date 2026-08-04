use super::*;

pub(super) fn base_sources_for_root(
    adapter: &dyn JsonlFamilyAdapter,
    inventory: &JsonlFamilyInventory,
    requested_root: &Path,
    sink: &SourceBackedGenerationSink<'_>,
) -> SourceBackedRouteResult<Vec<CertifiedSource>> {
    let sources = match adapter.base_scope() {
        JsonlFamilyBaseScope::ProviderFamily => {
            source_backed_base_sources(sink, |source| adapter.owns(source))
        }
        JsonlFamilyBaseScope::Route => sink
            .base_route_sources()
            .map_err(route_internal)?
            .into_values()
            .filter(|source| adapter.owns(source.observation().source()))
            .collect(),
    };
    sources
        .into_iter()
        .filter_map(|source| match adapter.base_source_path(&source) {
            Ok(path)
                if inventory.authorities.is_empty() && path.starts_with(requested_root)
                    || inventory
                        .authorities
                        .iter()
                        .any(|authority| path.starts_with(authority.named_path())) =>
            {
                Some(Ok(source))
            }
            Ok(_) => None,
            Err(error) => Some(Err(route_invalid(error))),
        })
        .collect()
}
