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
    let retained_provider_roots =
        configured_retained_provider_roots(discovery, retained_generation.as_ref())?;
    let mut build = build_automatic_source_backed_registry_from_report_with_retained_roots(
        discovery,
        data_root,
        report,
        &retained_provider_roots,
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
    let removed_provider_root_routes = retained_generation
        .as_ref()
        .map(|generation| {
            removed_configured_provider_root_routes(
                generation.manifest().provider_roots(),
                discovery.configured_provider_roots(),
            )
        })
        .unwrap_or_default();
    if let Some(retained) = retained_generation.as_ref() {
        build
            .registry
            .retain_unavailable_provider_root_routes(retained.manifest().provider_roots())?;
    }
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
    if let Some(retained) = retained_generation.as_ref() {
        for predecessor in retained.manifest().provider_roots() {
            let Some(desired) = discovery
                .configured_provider_roots()
                .iter()
                .find(|desired| {
                    desired.id == predecessor.definition().id
                        && !provider_root_retention_compatible(predecessor.definition(), desired)
                })
            else {
                continue;
            };
            let retired = predecessor
                .routes()
                .iter()
                .filter(|route| predecessor.exact_source_tokens_for_route(route).is_none())
                .filter(|route| !current_executable_routes.contains(*route))
                .cloned()
                .collect::<BTreeSet<_>>();
            // Retirement is owned by the final executable member in the
            // registry's publication order. The publication executor gates
            // this deferred retirement on the terminal success of every
            // route in the replacement root before authorizing it.
            let replacement_owner = build
                .registry
                .routes()
                .filter(|metadata| {
                    metadata.source.provider == desired.provider
                        && metadata
                            .source
                            .route_provenance
                            .configured_root()
                            .is_some_and(|(root_id, _)| root_id == desired.id)
                })
                .filter_map(|metadata| metadata.route_identity.as_ref())
                .filter(|route| current_executable_routes.contains(*route))
                .last()
                .cloned();
            if let Some(route) = replacement_owner {
                build
                    .registry
                    .retire_routes_after_success(&route, retired.iter().cloned())?;
            }
        }
    }
    if let Some(retained) = retained_generation.as_ref() {
        for root in retained.manifest().provider_roots().iter().filter(|root| {
            root.source_identity() == ProviderRootSourceIdentity::Released
                && !discovery
                    .configured_provider_roots()
                    .iter()
                    .any(|desired| provider_root_retention_compatible(root.definition(), desired))
        }) {
            let coexistence_lineage =
                automatic_provider_root_coexistence_source_lineage(root.definition());
            for ordinary in root
                .routes()
                .iter()
                .filter(|route| current_executable_routes.contains(*route))
            {
                let coexistence = automatic_provider_root_coexistence_route_identity(
                    ordinary,
                    coexistence_lineage,
                )?;
                if retained.manifest().source_route(&coexistence).is_some() {
                    build
                        .registry
                        .retire_routes_after_success(ordinary, [coexistence])?;
                }
            }
        }
    }
    let withdrawn_provider_root_routes = removed_provider_root_routes
        .difference(&current_provider_root_routes)
        .filter(|route| !current_executable_routes.contains(*route))
        .cloned()
        .collect::<BTreeSet<_>>();
    build
        .registry
        .set_root_withdrawals(withdrawn_provider_root_routes);
    build.registry.set_root_retirements(BTreeSet::new());
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
