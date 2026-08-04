use super::*;

impl ExplicitSourceCatalogAuthority {
    pub(crate) fn prepare_retained_discovery_report(
        &self,
        requested: Option<&Self>,
        report: &mut DiscoveryReport,
    ) -> Result<()> {
        let requested_keys = requested
            .map(Self::exact_route_keys)
            .transpose()?
            .unwrap_or_default();
        let entries = self
            .entries
            .iter()
            .map(|entry| Ok((entry.exact_route_key()?, entry)))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter_map(|(key, entry)| (!requested_keys.contains(&key)).then(|| entry.clone()))
            .collect();
        remove_automatic_routes_shadowed_by_snapshot(
            report,
            &ExplicitSourceCatalogSnapshot { entries },
        );
        Ok(())
    }

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

    fn exact_route_keys(&self) -> Result<BTreeSet<(String, String, PathBuf)>> {
        self.entries
            .iter()
            .map(CatalogEntry::exact_route_key)
            .collect()
    }
}

impl CatalogEntry {
    fn exact_route_key(&self) -> Result<(String, String, PathBuf)> {
        Ok((
            self.provider()?.as_str().to_owned(),
            self.certified_source_format()?.to_owned(),
            self.path.clone(),
        ))
    }
}
