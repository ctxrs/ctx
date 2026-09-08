use super::*;
use std::collections::BTreeMap;

pub(crate) struct CanonicalizedCatalogBindings {
    pub(crate) bindings: Vec<ExplicitSourceCatalogRouteBinding>,
    pub(crate) transitioned_routes: BTreeSet<ctx_history_index::SourceRouteIdentity>,
    pub(crate) retirements: BTreeMap<
        ctx_history_index::SourceRouteIdentity,
        Vec<ctx_history_index::SourceRouteIdentity>,
    >,
}

impl ExplicitSourceCatalogAuthority {
    pub(crate) fn canonicalize_published_bindings(
        &self,
        bindings: &[ExplicitSourceCatalogRouteBinding],
        registry: &SourceBackedProviderRegistry,
        admitted_automatic_routes: &BTreeSet<ctx_history_index::SourceRouteIdentity>,
    ) -> Result<CanonicalizedCatalogBindings> {
        let mut canonicalized = Vec::new();
        let mut transitioned_routes = BTreeSet::new();
        let mut retirements = BTreeMap::<_, Vec<_>>::new();
        for (entry, previous_route) in self.bound_routes(bindings)? {
            let coverage = automatic_route_coverage_binding(registry, entry)?
                .filter(|binding| admitted_automatic_routes.contains(&binding.route_identity));
            let route_identity = coverage
                .map(|binding| binding.route_identity)
                .unwrap_or_else(|| previous_route.clone());
            if route_identity != previous_route {
                transitioned_routes.insert(route_identity.clone());
                retirements
                    .entry(route_identity.clone())
                    .or_default()
                    .push(previous_route);
            }
            canonicalized.push(ExplicitSourceCatalogRouteBinding {
                catalog_lineage: entry.catalog_lineage.clone(),
                route_identity: route_identity.as_str().to_owned(),
            });
        }
        for retired in retirements.values_mut() {
            retired.sort();
            retired.dedup();
        }
        canonicalized.sort_by(|left, right| left.catalog_lineage.cmp(&right.catalog_lineage));
        Ok(CanonicalizedCatalogBindings {
            bindings: canonicalized,
            transitioned_routes,
            retirements,
        })
    }

    pub(crate) fn automatic_route_worksets(
        &self,
        registry: &SourceBackedProviderRegistry,
        bindings: &[ExplicitSourceCatalogRouteBinding],
    ) -> Result<BTreeMap<ctx_history_index::SourceRouteIdentity, SourceBackedRefreshWorkset>> {
        let mut worksets = BTreeMap::<_, SourceBackedRefreshWorkset>::new();
        for (entry, bound_route) in self.bound_routes(bindings)? {
            let Some(coverage) = automatic_route_coverage_binding(registry, entry)? else {
                continue;
            };
            if coverage.route_identity != bound_route {
                continue;
            }
            worksets
                .entry(bound_route)
                .and_modify(|workset| workset.merge(coverage.workset.clone()))
                .or_insert(coverage.workset);
        }
        Ok(worksets)
    }

    pub(crate) fn register_routes_after_discovery_merge(
        &self,
        data_root: &Path,
        base_generation: Option<&VerifiedGenerationSnapshot>,
        build: &mut SourceBackedAutomaticRegistryBuild,
    ) -> Result<Vec<ExplicitSourceCatalogRouteBinding>> {
        let snapshot = self.snapshot();
        register_explicit_source_catalog_snapshot_routes(
            data_root,
            base_generation,
            build,
            &snapshot,
        )
    }
}
