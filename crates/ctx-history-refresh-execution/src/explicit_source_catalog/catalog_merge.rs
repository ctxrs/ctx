use super::*;
use crate::ExplicitSourcePathMissing;
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

    pub(crate) fn register_retained_routes_for_rebuild(
        &self,
        data_root: &Path,
        previous_bindings: &[ExplicitSourceCatalogRouteBinding],
        replaced_catalog: Option<&Self>,
        build: &mut SourceBackedAutomaticRegistryBuild,
    ) -> Result<Vec<ExplicitSourceCatalogRouteBinding>> {
        let replaced_authorities = replaced_catalog
            .map(|catalog| Self::authority_keys(&catalog.entries))
            .transpose()?
            .unwrap_or_default();
        let bound_routes = self.bound_routes(previous_bindings)?;
        let mut bindings = Vec::with_capacity(bound_routes.len());
        for (entry, previous_route) in bound_routes {
            let authority = (
                entry.provider()?.as_str().to_owned(),
                entry.certified_source_format()?.to_owned(),
            );
            if replaced_authorities.contains(&authority) {
                bindings.push(ExplicitSourceCatalogRouteBinding {
                    catalog_lineage: entry.catalog_lineage.clone(),
                    route_identity: previous_route.as_str().to_owned(),
                });
                continue;
            }
            if let Some(coverage) = automatic_route_coverage_binding(&build.registry, entry)? {
                bindings.push(ExplicitSourceCatalogRouteBinding {
                    catalog_lineage: entry.catalog_lineage.clone(),
                    route_identity: coverage.route_identity.as_str().to_owned(),
                });
                continue;
            }
            let source = match source_from_catalog_entry(data_root, entry) {
                Ok(source) => source,
                Err(error) if error.downcast_ref::<ExplicitSourcePathMissing>().is_some() => {
                    bindings.push(ExplicitSourceCatalogRouteBinding {
                        catalog_lineage: entry.catalog_lineage.clone(),
                        route_identity: previous_route.as_str().to_owned(),
                    });
                    continue;
                }
                Err(error) => return Err(error),
            };
            validate_explicit_source_root(data_root, &source)?;
            let before = build
                .registry
                .routes()
                .filter_map(|route| route.route_identity.clone())
                .collect::<BTreeSet<_>>();
            register_enabled_catalog_route(
                data_root,
                &mut build.registry,
                source,
                entry.lineage()?,
                &[],
            )
            .with_context(|| {
                format!(
                    "register retained explicit route {} {} for full rebuild",
                    entry.provider,
                    entry.path.display()
                )
            })?;
            if let Some(preserved) = entry.route_identity.as_ref() {
                let constructed = build
                    .registry
                    .routes()
                    .filter_map(|route| route.route_identity.clone())
                    .filter(|identity| !before.contains(identity))
                    .collect::<Vec<_>>();
                let [constructed] = constructed.as_slice() else {
                    bail!(
                        "retained relocated catalog lineage {} constructed {} routes instead of one",
                        entry.catalog_lineage,
                        constructed.len()
                    );
                };
                build.registry.preserve_explicit_route_identity(
                    constructed,
                    ctx_history_index::SourceRouteIdentity::from_sha256(preserved.clone())?,
                    entry.relocate_from.as_deref().ok_or_else(|| {
                        anyhow!("preserved relocation route has no exact old-path witness")
                    })?,
                )?;
            }
            let added = build
                .registry
                .routes()
                .filter_map(|route| route.route_identity.clone())
                .filter(|identity| !before.contains(identity))
                .collect::<Vec<_>>();
            let [route_identity] = added.as_slice() else {
                bail!(
                    "retained explicit catalog lineage {} registered {} executable routes instead of one",
                    entry.catalog_lineage,
                    added.len()
                );
            };
            bindings.push(ExplicitSourceCatalogRouteBinding {
                catalog_lineage: entry.catalog_lineage.clone(),
                route_identity: route_identity.as_str().to_owned(),
            });
        }
        bindings.sort_by(|left, right| left.catalog_lineage.cmp(&right.catalog_lineage));
        Ok(bindings)
    }
}
