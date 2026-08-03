use super::*;

impl ExplicitSourceCatalogAuthority {
    pub(crate) fn prepare_discovery_report(
        &self,
        _data_root: &Path,
        report: &mut DiscoveryReport,
    ) -> Result<()> {
        let snapshot = self.snapshot();
        remove_automatic_routes_shadowed_by_snapshot(report, &snapshot);
        Ok(())
    }

    pub(crate) fn register_routes_after_discovery_merge(
        &self,
        data_root: &Path,
        base_generation: Option<&VerifiedIndex>,
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
