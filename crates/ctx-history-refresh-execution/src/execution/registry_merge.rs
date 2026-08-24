use super::*;

pub(crate) fn build_merged_source_backed_registry(
    discovery: &DiscoveryContext,
    report: DiscoveryReport,
    discovery_duration: StdDuration,
    data_root: &Path,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    published_state: &dyn PublishedSourceBackedStatePort,
) -> Result<MergedSourceBackedRegistry> {
    build_merged_source_backed_registry_with_automatic_routes(
        discovery,
        report,
        discovery_duration,
        data_root,
        explicit_source_catalog,
        &BTreeSet::new(),
        published_state,
    )
}

pub(super) fn build_merged_source_backed_registry_with_automatic_routes(
    discovery: &DiscoveryContext,
    report: DiscoveryReport,
    discovery_duration: StdDuration,
    data_root: &Path,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    admitted_automatic_routes: &BTreeSet<SourceRouteIdentity>,
    published_state: &dyn PublishedSourceBackedStatePort,
) -> Result<MergedSourceBackedRegistry> {
    let PublishedSourceBackedState {
        verified_index: retained_generation,
        explicit_source_catalog: previous_explicit_source_catalog,
        catalog_route_bindings: previous_catalog_route_bindings,
        route_controls: previous_route_controls,
    } = published_state.open_published_state(data_root)?;
    let provider_root_identities =
        configured_provider_root_identities(discovery, retained_generation.as_ref());
    let mut build = build_automatic_source_backed_registry_from_report_with_root_identities(
        discovery,
        data_root,
        report,
        &provider_root_identities,
    );
    build.discovery_duration = discovery_duration;
    let requested_catalog_route_bindings = explicit_source_catalog
        .map(|catalog| {
            catalog.register_routes_after_discovery_merge(
                data_root,
                retained_generation.as_ref(),
                &mut build,
            )
        })
        .transpose()?
        .unwrap_or_default();
    let canonicalized_previous = previous_explicit_source_catalog
        .as_ref()
        .map(|catalog| {
            catalog.canonicalize_published_bindings(
                &previous_catalog_route_bindings,
                &build.registry,
                admitted_automatic_routes,
            )
        })
        .transpose()?;
    let reactivated_automatic_routes = canonicalized_previous
        .as_ref()
        .map(|canonicalized| canonicalized.transitioned_routes.clone())
        .unwrap_or_default();
    for (replacement, retired) in canonicalized_previous
        .as_ref()
        .map(|canonicalized| canonicalized.retirements.clone())
        .unwrap_or_default()
    {
        build
            .registry
            .retire_routes_after_success(&replacement, retired)?;
    }
    let previous_catalog_route_bindings = canonicalized_previous
        .map(|canonicalized| canonicalized.bindings)
        .unwrap_or(previous_catalog_route_bindings);
    let route_retirements = ExplicitSourceCatalogAuthority::replacement_route_retirements(
        previous_explicit_source_catalog
            .as_ref()
            .map(|catalog| (catalog, previous_catalog_route_bindings.as_slice())),
        explicit_source_catalog
            .map(|catalog| (catalog, requested_catalog_route_bindings.as_slice())),
    )?;
    for (replacement, retired) in route_retirements {
        build
            .registry
            .retire_routes_after_success(&replacement, retired)?;
    }
    let previous_provider_root_routes = retained_generation
        .as_ref()
        .map(|generation| {
            generation
                .manifest()
                .provider_roots()
                .iter()
                .flat_map(|root| root.routes().iter().cloned())
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let current_provider_root_routes = build
        .registry
        .applied_provider_roots()
        .map(|(_, _, roots)| {
            roots
                .iter()
                .flat_map(|root| root.routes().iter().cloned())
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let current_executable_routes = build
        .registry
        .executable_route_identities()
        .into_iter()
        .collect::<BTreeSet<_>>();
    let retired_provider_root_routes = previous_provider_root_routes
        .difference(&current_provider_root_routes)
        .filter(|route| !current_executable_routes.contains(*route))
        .cloned()
        .collect::<BTreeSet<_>>();
    build
        .registry
        .set_provider_root_route_retirements(retired_provider_root_routes);
    Ok(MergedSourceBackedRegistry {
        build,
        reactivated_automatic_routes,
        previous_explicit_source_catalog,
        previous_catalog_route_bindings,
        requested_explicit_source_catalog: explicit_source_catalog.cloned(),
        retained_generation,
        requested_catalog_route_bindings,
        previous_route_controls,
    })
}

pub(super) fn provider_root_publication_scope(
    requested: &SourceBackedRefreshScope,
    physical: &SourceBackedRefreshScope,
    registry: &ctx_history_capture::SourceBackedProviderRegistry,
    retained: Option<&VerifiedIndex>,
) -> SourceBackedRefreshScope {
    let changed = matches!(requested, SourceBackedRefreshScope::All)
        && retained.zip(registry.applied_provider_roots()).is_some_and(
            |(retained, (automatic, digest, roots))| {
                let manifest = retained.manifest();
                *automatic != manifest.automatic_provider_discovery()
                    || digest != manifest.provider_root_config_digest()
                    || roots != manifest.provider_roots()
            },
        );
    if changed {
        SourceBackedRefreshScope::All
    } else {
        physical.clone()
    }
}
