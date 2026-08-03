use super::*;
use std::{fs, time::SystemTime};

/// Provider-neutral result of comparing one exact authorized route target
/// against the observation bound to the active Core publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteObservation {
    Unchanged,
    Changed,
    Unavailable,
    Indeterminate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RouteWatchTargets {
    primary: PathBuf,
    kind: Option<SourceBackedWatchTargetKind>,
    targets: BTreeSet<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RouteTargetSample {
    Available(String),
    Unavailable,
    Indeterminate,
}

/// Exact, content-free filesystem targets grouped by the capture registry's
/// stable route identities.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceBackedWatchCatalog {
    routes: BTreeMap<SourceRouteIdentity, RouteWatchTargets>,
}

impl SourceBackedWatchCatalog {
    pub fn route_ids(&self) -> impl ExactSizeIterator<Item = &SourceRouteIdentity> {
        self.routes.keys()
    }

    pub fn route_targets(
        &self,
    ) -> impl ExactSizeIterator<Item = (&SourceRouteIdentity, &BTreeSet<PathBuf>)> {
        self.routes
            .iter()
            .map(|(identity, route)| (identity, &route.targets))
    }

    pub fn target_paths(&self) -> impl Iterator<Item = &Path> {
        self.routes
            .values()
            .flat_map(|route| route.targets.iter().map(PathBuf::as_path))
    }

    /// Maps an event only to exact authorized targets or their true path
    /// ancestors/descendants. A target is recursive only while it is an actual
    /// directory; missing targets therefore cannot match sibling basenames.
    pub fn routes_overlapping_path(&self, event: &Path) -> BTreeSet<SourceRouteIdentity> {
        self.routes
            .iter()
            .filter(|(_, route)| {
                route.targets.iter().any(|target| {
                    target == event
                        || target.starts_with(event)
                        || (target.is_dir() && event.starts_with(target))
                })
            })
            .map(|(identity, _)| identity.clone())
            .collect()
    }

    /// Returns the current content-free certification token for one exact
    /// route. Routes without a bounded provider-neutral adapter deliberately
    /// return no token and therefore cannot participate in the warm skip.
    pub fn certify_route_observation(&self, route: &SourceRouteIdentity) -> Option<String> {
        match self.sample_route(route) {
            RouteTargetSample::Available(fingerprint) => Some(fingerprint),
            RouteTargetSample::Unavailable | RouteTargetSample::Indeterminate => None,
        }
    }

    /// Compares one exact route with a durable publication observation.
    /// Missing durable evidence is indeterminate rather than implicitly clean.
    pub fn observe_route(
        &self,
        route: &SourceRouteIdentity,
        expected_fingerprint: Option<&str>,
    ) -> RouteObservation {
        let Some(expected) = expected_fingerprint.filter(|value| is_sha256(value)) else {
            return RouteObservation::Indeterminate;
        };
        match self.sample_route(route) {
            RouteTargetSample::Available(actual) if actual == expected => {
                RouteObservation::Unchanged
            }
            RouteTargetSample::Available(_) => RouteObservation::Changed,
            RouteTargetSample::Unavailable => RouteObservation::Unavailable,
            RouteTargetSample::Indeterminate => RouteObservation::Indeterminate,
        }
    }

    fn sample_route(&self, route: &SourceRouteIdentity) -> RouteTargetSample {
        let Some(targets) = self.routes.get(route) else {
            return RouteTargetSample::Indeterminate;
        };
        let Some(kind) = targets.kind else {
            return RouteTargetSample::Indeterminate;
        };
        match kind {
            SourceBackedWatchTargetKind::Path => sample_ordinary_file(targets),
            SourceBackedWatchTargetKind::SqliteDatabase => sample_sqlite_family(targets),
        }
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
            let targets = catalog
                .routes
                .entry(identity)
                .or_insert_with(|| RouteWatchTargets {
                    primary: route.metadata.source.path.clone(),
                    kind: Some(route.metadata.watch_target_kind),
                    targets: BTreeSet::new(),
                });
            if targets.primary != route.metadata.source.path
                || targets.kind != Some(route.metadata.watch_target_kind)
            {
                targets.kind = None;
            }
            insert_route_watch_targets(
                &mut targets.targets,
                &route.metadata.source.path,
                route.metadata.watch_target_kind,
            );
            for missing in &route.certified_missing_paths {
                insert_route_watch_targets(
                    &mut targets.targets,
                    missing,
                    route.metadata.watch_target_kind,
                );
            }
        }
        catalog
    }
}

fn sample_ordinary_file(route: &RouteWatchTargets) -> RouteTargetSample {
    if route.targets.len() != 1 || !route.targets.contains(&route.primary) {
        return RouteTargetSample::Indeterminate;
    }
    sample_exact_files(std::slice::from_ref(&route.primary), &route.primary)
}

fn sample_sqlite_family(route: &RouteWatchTargets) -> RouteTargetSample {
    if route.targets.len() != 4 || !route.targets.contains(&route.primary) {
        return RouteTargetSample::Indeterminate;
    }
    let targets = route.targets.iter().cloned().collect::<Vec<_>>();
    sample_exact_files(&targets, &route.primary)
}

fn sample_exact_files(targets: &[PathBuf], primary: &Path) -> RouteTargetSample {
    let mut digest = Sha256::new();
    digest.update(b"ctx.route-observation.v1\0");
    digest.update((targets.len() as u64).to_be_bytes());
    for target in targets {
        hash_os_str(&mut digest, target.as_os_str());
        let metadata = match fs::symlink_metadata(target) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if target == primary {
                    return RouteTargetSample::Unavailable;
                }
                digest.update(b"missing\0");
                continue;
            }
            Err(_) => return RouteTargetSample::Indeterminate,
        };
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return RouteTargetSample::Indeterminate;
        }
        digest.update(b"file\0");
        hash_file_metadata(&mut digest, &metadata);
    }
    RouteTargetSample::Available(format!("{:x}", digest.finalize()))
}

fn hash_file_metadata(digest: &mut Sha256, metadata: &fs::Metadata) {
    digest.update(metadata.len().to_be_bytes());
    hash_system_time(digest, metadata.modified().ok());
    hash_system_time(digest, metadata.created().ok());

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        digest.update(metadata.dev().to_be_bytes());
        digest.update(metadata.ino().to_be_bytes());
        digest.update(metadata.mode().to_be_bytes());
        digest.update(metadata.nlink().to_be_bytes());
        digest.update(metadata.mtime().to_be_bytes());
        digest.update(metadata.mtime_nsec().to_be_bytes());
        digest.update(metadata.ctime().to_be_bytes());
        digest.update(metadata.ctime_nsec().to_be_bytes());
    }

    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        digest.update(metadata.file_attributes().to_be_bytes());
        digest.update(metadata.creation_time().to_be_bytes());
        digest.update(metadata.last_write_time().to_be_bytes());
        digest.update(metadata.file_size().to_be_bytes());
    }
}

fn hash_system_time(digest: &mut Sha256, value: Option<SystemTime>) {
    match value.and_then(|value| value.duration_since(SystemTime::UNIX_EPOCH).ok()) {
        Some(value) => {
            digest.update([1]);
            digest.update(value.as_secs().to_be_bytes());
            digest.update(value.subsec_nanos().to_be_bytes());
        }
        None => digest.update([0]),
    }
}

fn hash_os_str(digest: &mut Sha256, value: &std::ffi::OsStr) {
    let bytes = value.as_encoded_bytes();
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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

    #[test]
    fn ordinary_file_observation_catches_append_rewrite_and_delete() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("history.jsonl");
        fs::write(&source_path, b"one\n").unwrap();
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
        let identity = route.metadata.route_identity.clone().unwrap();
        let mut registry = SourceBackedProviderRegistry::new();
        registry.register(route);
        let catalog = registry.watch_catalog();
        let certified = catalog.certify_route_observation(&identity).unwrap();

        assert_eq!(
            catalog.observe_route(&identity, Some(&certified)),
            RouteObservation::Unchanged
        );
        fs::write(&source_path, b"one\ntwo\n").unwrap();
        assert_eq!(
            catalog.observe_route(&identity, Some(&certified)),
            RouteObservation::Changed
        );

        let rewritten = catalog.certify_route_observation(&identity).unwrap();
        fs::write(&source_path, b"rewritten").unwrap();
        assert_eq!(
            catalog.observe_route(&identity, Some(&rewritten)),
            RouteObservation::Changed
        );
        fs::remove_file(&source_path).unwrap();
        assert_eq!(
            catalog.observe_route(&identity, Some(&rewritten)),
            RouteObservation::Unavailable
        );
    }

    #[test]
    fn sqlite_observation_includes_wal_and_missing_main_is_unavailable() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("state.db");
        fs::write(&database, b"sqlite-main").unwrap();
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
        let certified = catalog.certify_route_observation(&identity).unwrap();

        let mut wal = database.as_os_str().to_os_string();
        wal.push("-wal");
        fs::write(PathBuf::from(wal), b"wal-only change").unwrap();
        assert_eq!(
            catalog.observe_route(&identity, Some(&certified)),
            RouteObservation::Changed
        );
        fs::remove_file(database).unwrap();
        assert_eq!(
            catalog.observe_route(&identity, Some(&certified)),
            RouteObservation::Unavailable
        );
    }

    #[test]
    fn directories_and_missing_certificates_are_indeterminate() {
        let temp = tempfile::tempdir().unwrap();
        let route = SourceBackedRoute::automatic(
            source(
                CaptureProvider::Codex,
                temp.path().to_path_buf(),
                "codex_history_jsonl",
            ),
            SourceBackedSelectorAuthority::DiscoveredWinner,
            driver(),
        )
        .unwrap();
        let identity = route.metadata.route_identity.clone().unwrap();
        let mut registry = SourceBackedProviderRegistry::new();
        registry.register(route);
        let catalog = registry.watch_catalog();

        assert_eq!(
            catalog.observe_route(&identity, Some(&"00".repeat(32))),
            RouteObservation::Indeterminate
        );
        assert_eq!(
            catalog.observe_route(&identity, None),
            RouteObservation::Indeterminate
        );
    }
}
