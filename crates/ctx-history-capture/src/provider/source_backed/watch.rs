use super::*;

/// Exact, content-free filesystem targets grouped by the capture registry's
/// stable route identities.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceBackedWatchCatalog {
    routes: BTreeMap<SourceRouteIdentity, BTreeSet<PathBuf>>,
}

impl SourceBackedWatchCatalog {
    pub fn route_ids(&self) -> impl ExactSizeIterator<Item = &SourceRouteIdentity> {
        self.routes.keys()
    }

    pub fn route_targets(
        &self,
    ) -> impl ExactSizeIterator<Item = (&SourceRouteIdentity, &BTreeSet<PathBuf>)> {
        self.routes.iter()
    }

    pub fn target_paths(&self) -> impl Iterator<Item = &Path> {
        self.routes
            .values()
            .flat_map(|targets| targets.iter().map(PathBuf::as_path))
    }

    /// Maps an event only to exact authorized targets or their true path
    /// ancestors/descendants. A target is recursive only while it is an actual
    /// directory; missing targets therefore cannot match sibling basenames.
    pub fn routes_overlapping_path(&self, event: &Path) -> BTreeSet<SourceRouteIdentity> {
        self.routes
            .iter()
            .filter(|(_, targets)| {
                targets.iter().any(|target| {
                    target == event
                        || target.starts_with(event)
                        || (target.is_dir() && event.starts_with(target))
                })
            })
            .map(|(identity, _)| identity.clone())
            .collect()
    }
}

impl SourceBackedProviderRegistry {
    /// Derives watcher authority from this exact executable registry snapshot.
    pub fn watch_catalog(&self) -> SourceBackedWatchCatalog {
        let mut catalog = SourceBackedWatchCatalog::default();
        for route in &self.routes {
            if route.driver.is_none() && route.certified_missing_paths.is_empty() {
                continue;
            }
            let Some(identity) = route.metadata.route_identity.clone() else {
                continue;
            };
            let targets = catalog.routes.entry(identity).or_default();
            insert_route_watch_targets(
                targets,
                &route.metadata.source.path,
                route.metadata.watch_target_kind,
            );
            for missing in &route.certified_missing_paths {
                insert_route_watch_targets(targets, missing, route.metadata.watch_target_kind);
            }
        }
        catalog
    }
}

fn insert_route_watch_targets(
    targets: &mut BTreeSet<PathBuf>,
    path: &Path,
    kind: SourceBackedWatchTargetKind,
) {
    targets.insert(path.to_path_buf());
    if kind != SourceBackedWatchTargetKind::SqliteDatabase {
        return;
    }
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut companion = path.as_os_str().to_os_string();
        companion.push(suffix);
        targets.insert(PathBuf::from(companion));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(
        provider: CaptureProvider,
        path: PathBuf,
        source_format: &'static str,
    ) -> ProviderSource {
        ProviderSource {
            provider,
            exists: true,
            path,
            source_format,
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: crate::ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
        }
    }

    fn driver() -> SourceBackedRouteDriver {
        SourceBackedRouteDriver::new(|_| Ok(()), |_| false, |_| true)
    }

    #[test]
    fn sqlite_route_authorizes_only_its_exact_database_family() {
        let database = PathBuf::from("/provider/state.db");
        let route = SourceBackedRoute::automatic(
            source(
                CaptureProvider::OpenCode,
                database.clone(),
                "opencode_sqlite",
            ),
            SourceBackedSelectorAuthority::DiscoveredWinner,
            driver(),
        )
        .unwrap();
        let identity = route.metadata.route_identity.clone().unwrap();
        let mut registry = SourceBackedProviderRegistry::new();
        registry.register(route);

        let catalog = registry.watch_catalog();
        let targets = catalog.route_targets().next().unwrap().1;
        assert_eq!(targets.len(), 4);
        assert!(targets.contains(&database));
        assert!(targets.contains(&PathBuf::from("/provider/state.db-wal")));
        assert!(targets.contains(&PathBuf::from("/provider/state.db-shm")));
        assert!(targets.contains(&PathBuf::from("/provider/state.db-journal")));
        assert_eq!(
            catalog.routes_overlapping_path(Path::new("/provider/state.db-wal")),
            BTreeSet::from([identity])
        );
        assert!(catalog
            .routes_overlapping_path(Path::new("/provider/other.db-wal"))
            .is_empty());
    }

    #[test]
    fn ordinary_file_route_does_not_invent_sqlite_companions_or_match_siblings() {
        let source_path = PathBuf::from("/provider/history.jsonl");
        let route = SourceBackedRoute::automatic(
            source(
                CaptureProvider::Codex,
                source_path.clone(),
                "codex_history_jsonl",
            ),
            SourceBackedSelectorAuthority::DiscoveredWinner,
            driver(),
        )
        .unwrap();
        let mut registry = SourceBackedProviderRegistry::new();
        registry.register(route);

        let catalog = registry.watch_catalog();
        let targets = catalog.route_targets().next().unwrap().1;
        assert_eq!(targets, &BTreeSet::from([source_path]));
        assert!(catalog
            .routes_overlapping_path(Path::new("/provider/history.jsonl-wal"))
            .is_empty());
        assert!(catalog
            .routes_overlapping_path(Path::new("/provider/other/history.jsonl"))
            .is_empty());
    }
}
