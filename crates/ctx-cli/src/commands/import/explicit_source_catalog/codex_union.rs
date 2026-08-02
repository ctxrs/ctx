use super::*;

impl ExplicitSourceCatalogAuthority {
    pub(crate) fn prepare_discovery_report(
        &self,
        data_root: &Path,
        report: &mut DiscoveryReport,
    ) -> Result<()> {
        let snapshot = load_catalog_for_authority(data_root, self)?;
        remove_automatic_routes_shadowed_by_snapshot(report, &snapshot);
        merge_enabled_codex_session_roots(report, &snapshot)
    }

    pub(crate) fn register_routes_after_discovery_merge(
        &self,
        data_root: &Path,
        base_generation: Option<&VerifiedIndex>,
        build: &mut SourceBackedAutomaticRegistryBuild,
    ) -> Result<Vec<ExplicitSourceCatalogRouteBinding>> {
        let snapshot = load_catalog_for_authority(data_root, self)?;
        register_explicit_source_catalog_snapshot_routes(
            data_root,
            base_generation,
            build,
            &snapshot,
            true,
        )
    }
}

fn merge_enabled_codex_session_roots(
    report: &mut DiscoveryReport,
    snapshot: &ExplicitSourceCatalogSnapshot,
) -> Result<()> {
    for entry in &snapshot.entries {
        if !is_enabled_codex_session_tree(entry)? {
            continue;
        }
        let source = source_from_catalog_entry(entry, true)?;
        if source.source_format != "codex_session_jsonl_tree" {
            bail!(
                "explicit Codex session-tree authority at {} resolved to incompatible format `{}`",
                entry.path.display(),
                source.source_format
            );
        }
        report.sources.push(source);
    }
    Ok(())
}
