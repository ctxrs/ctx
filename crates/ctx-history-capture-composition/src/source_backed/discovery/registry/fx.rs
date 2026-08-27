use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn register_configured_fx_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    data_root: &Path,
    certified_source_format: &str,
    configured_root: &ProviderRootDefinition,
    configured_source_identity: Option<ProviderRootSourceIdentity>,
    provider_root_registrations: &BTreeMap<String, ProviderRootRegistration>,
    source_root_lineage: Option<[u8; 32]>,
    route_role: &ProviderRouteRole,
) -> SourceBackedCoordinatorResult<()> {
    let adapter_lineage = match configured_source_identity {
        Some(ProviderRootSourceIdentity::Released) => provider_root_registrations
            .get(&configured_root.id)
            .and_then(|registration| registration.released_identity_root.as_deref())
            .map(|identity_root| {
                explicit_source_catalog_lineage(
                    source.provider,
                    certified_source_format,
                    identity_root,
                )
            })
            .ok_or_else(|| {
                invalid_route(
                    source.provider,
                    "released fx root has no immutable automatic identity root",
                )
            })?,
        Some(ProviderRootSourceIdentity::NamedV1) => source_root_lineage.ok_or_else(|| {
            invalid_route(source.provider, "named fx root has no catalog lineage")
        })?,
        None => {
            return Err(invalid_route(
                source.provider,
                "configured fx root has no source identity",
            ));
        }
    };
    register_configured_fx_source_backed_route(
        registry,
        source,
        data_root,
        adapter_lineage,
        source_root_lineage,
        route_role,
    )
}
