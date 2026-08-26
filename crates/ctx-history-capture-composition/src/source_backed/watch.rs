use super::*;
use std::fs;

mod fingerprint;
use fingerprint::{hash_file_metadata, hash_os_str};

/// Provider-neutral result of comparing one exact authorized route target
/// against the observation bound to the active Core publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteObservation {
    Unchanged,
    Changed,
    Unavailable,
    Indeterminate,
}

pub use ctx_history_provider_runtime::ProviderRouteControlExpectation as SourceBackedRouteControlExpectation;

#[derive(Debug, Clone, PartialEq, Eq)]
struct RouteWatchTargets {
    primary: PathBuf,
    kind: Option<SourceBackedWatchTargetKind>,
    control: Option<SourceBackedRouteControlExpectation>,
    targets: BTreeSet<PathBuf>,
    admission_sources: Option<Vec<ProviderSource>>,
    registration_sources: Option<Vec<RegisteredSource>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegisteredPathKind {
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RegisteredSource {
    source: ProviderSource,
    path_kind: RegisteredPathKind,
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
    provider_root_config_digest: Option<String>,
    automatic_split_legacy_routes: BTreeSet<SourceRouteIdentity>,
}

impl SourceBackedWatchCatalog {
    /// Returns the provider-root config snapshot used to construct this
    /// catalog. Daemons use this value only to decide when exact watch-route
    /// maintenance must be widened to one full topology refresh.
    pub fn provider_root_config_digest(&self) -> Option<&str> {
        self.provider_root_config_digest.as_deref()
    }

    pub fn route_ids(&self) -> impl ExactSizeIterator<Item = &SourceRouteIdentity> {
        self.routes.keys()
    }

    /// Whether a retained route identity is the released predecessor of a
    /// current role-specific automatic cohort.  This is topology metadata,
    /// not authority to publish the migration; the executor still validates
    /// the complete cohort and its witness before changing generations.
    pub fn has_automatic_split_legacy_route(&self, route: &SourceRouteIdentity) -> bool {
        self.automatic_split_legacy_routes.contains(route)
    }

    pub fn route_ids_for_provider(
        &self,
        provider: CaptureProvider,
    ) -> BTreeSet<SourceRouteIdentity> {
        self.routes
            .iter()
            .filter(|(_, targets)| {
                targets
                    .admission_sources
                    .as_ref()
                    .is_some_and(|sources| sources.iter().any(|source| source.provider == provider))
            })
            .map(|(route, _)| route.clone())
            .collect()
    }

    /// Returns immutable registration roots for one route eligible to satisfy
    /// explicit catalog coverage without rerunning provider-wide discovery.
    pub fn catalog_coverage_route_registration_sources(
        &self,
        route: &SourceRouteIdentity,
    ) -> Option<impl ExactSizeIterator<Item = &ProviderSource>> {
        Some(
            self.routes
                .get(route)?
                .registration_sources
                .as_ref()?
                .iter()
                .map(|registered| &registered.source),
        )
    }

    pub fn route_targets(
        &self,
    ) -> impl ExactSizeIterator<Item = (&SourceRouteIdentity, &BTreeSet<PathBuf>)> {
        self.routes
            .iter()
            .map(|(identity, route)| (identity, &route.targets))
    }

    pub fn route_control_expectation(
        &self,
        route: &SourceRouteIdentity,
    ) -> Option<&SourceBackedRouteControlExpectation> {
        self.routes.get(route)?.control.as_ref()
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

    /// Returns one exact ordinary-file member when a native event identifies
    /// it without weakening the route's declared watch authority. Directory,
    /// missing, symlink, database-family, and otherwise ambiguous events must
    /// use exhaustive route reconciliation.
    pub fn exact_member_for_event(
        &self,
        route: &SourceRouteIdentity,
        event: &Path,
    ) -> Option<PathBuf> {
        let targets = self.routes.get(route)?;
        if targets.kind != Some(SourceBackedWatchTargetKind::Path) {
            return None;
        }
        let event_metadata = fs::symlink_metadata(event).ok()?;
        if !event_metadata.file_type().is_file() {
            return None;
        }
        let registrations = targets.registration_sources.as_ref()?;
        (registrations
            .iter()
            .filter(|source| member_belongs_to_root(event, &source.source.path))
            .count()
            == 1)
            .then(|| event.to_path_buf())
    }

    /// Reconstructs the bounded discovery inputs for exact registered routes.
    /// This skips unrelated provider discovery without weakening the selected
    /// route's own exhaustive inventory or terminal revalidation.
    pub fn route_discovery_report(
        &self,
        routes: &BTreeSet<SourceRouteIdentity>,
    ) -> Option<DiscoveryReport> {
        if routes.is_empty() {
            return None;
        }
        for route in routes {
            let registrations = self.routes.get(route)?.registration_sources.as_ref()?;
            if registrations
                .iter()
                .any(|source| !registration_source_is_available(source))
            {
                return None;
            }
        }
        self.route_admission_report(routes)
    }

    /// Reconstructs the bounded discovery inputs for exact registered routes,
    /// including routes whose source is currently unavailable. Admission may
    /// use this immutable catalog authority to execute the exact request and
    /// produce a route-local missing/unavailable disposition; a route absent
    /// from the catalog still fails closed.
    pub fn route_admission_report(
        &self,
        routes: &BTreeSet<SourceRouteIdentity>,
    ) -> Option<DiscoveryReport> {
        if routes.is_empty() {
            return None;
        }
        let mut sources = Vec::new();
        for route in routes {
            let registrations = self.routes.get(route)?.admission_sources.as_ref()?;
            sources.extend(
                registrations
                    .iter()
                    .cloned()
                    .map(refresh_admission_source_presence),
            );
        }
        sources.sort_by(|left, right| {
            left.provider
                .as_str()
                .cmp(right.provider.as_str())
                .then_with(|| left.source_format.cmp(right.source_format))
                .then_with(|| left.path.cmp(&right.path))
        });
        sources.dedup();
        Some(DiscoveryReport {
            sources,
            issues: Vec::new(),
        })
    }

    /// Reconstructs one exact-member discovery report from this immutable
    /// catalog snapshot. Every member is revalidated against exactly one
    /// retained ordinary-file registration root before any source is returned.
    /// `None` is a fail-closed abstention; callers retain route-local exhaustive
    /// work when possible and otherwise use normal global discovery.
    pub fn exact_member_discovery_report(
        &self,
        routes: &BTreeSet<SourceRouteIdentity>,
        worksets: &BTreeMap<SourceRouteIdentity, BTreeSet<PathBuf>>,
    ) -> Option<DiscoveryReport> {
        if routes.is_empty() || routes.len() != worksets.len() {
            return None;
        }
        for route in routes {
            let targets = self.routes.get(route)?;
            if targets.kind != Some(SourceBackedWatchTargetKind::Path) {
                return None;
            }
            let registrations = targets.registration_sources.as_ref()?;
            let members = worksets.get(route)?;
            if members.is_empty()
                || members.iter().any(|member| {
                    !fs::symlink_metadata(member)
                        .is_ok_and(|metadata| metadata.file_type().is_file())
                        || registrations
                            .iter()
                            .filter(|source| member_belongs_to_root(member, &source.source.path))
                            .count()
                            != 1
                })
            {
                return None;
            }
        }
        self.route_discovery_report(routes)
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
    pub(in crate::source_backed) fn attach_route_watch_targets(
        &mut self,
        source: &ProviderSource,
        observe: impl Fn() -> Option<SourceBackedRouteWatchTargets> + Send + Sync + 'static,
    ) -> SourceBackedCoordinatorResult<()> {
        let route = self
            .routes
            .iter_mut()
            .find(|route| {
                route.metadata.source.provider == source.provider
                    && route.metadata.source.path == source.path
                    && route.metadata.source.source_format == source.source_format
            })
            .ok_or_else(|| SourceBackedCoordinatorError::InvalidRoute {
                provider: source.provider,
                detail: "registered route is unavailable for exact watch-target attachment"
                    .to_owned(),
            })?;
        let driver =
            route
                .driver
                .as_mut()
                .ok_or_else(|| SourceBackedCoordinatorError::InvalidRoute {
                    provider: source.provider,
                    detail: "registered route has no executable watch authority".to_owned(),
                })?;
        driver.watch_targets = Some(Arc::new(observe));
        Ok(())
    }

    /// Derives watcher authority from this exact executable registry snapshot.
    pub fn watch_catalog(&self) -> SourceBackedWatchCatalog {
        let mut catalog = SourceBackedWatchCatalog {
            provider_root_config_digest: self
                .applied_provider_roots
                .as_ref()
                .map(|(_, digest, _)| digest.clone()),
            ..SourceBackedWatchCatalog::default()
        };
        let configured_route_ids = self
            .applied_provider_roots
            .iter()
            .flat_map(|(_, _, roots)| roots)
            .flat_map(|root| root.routes().iter().cloned())
            .collect::<BTreeSet<_>>();
        for route in &self.routes {
            if route.driver.is_none()
                && route.certified_missing_paths.is_empty()
                && route.metadata.source.status != ProviderSourceStatus::Missing
            {
                continue;
            }
            let Some(identity) = route.metadata.route_identity.clone() else {
                continue;
            };
            if let Some(legacy) = automatic_route_split_legacy_route(route) {
                catalog.automatic_split_legacy_routes.insert(legacy);
            }
            let catalog_coverage_eligible = configured_route_ids.contains(&identity);
            let targets = catalog
                .routes
                .entry(identity)
                .or_insert_with(|| RouteWatchTargets {
                    primary: route.metadata.source.path.clone(),
                    kind: Some(route.metadata.watch_target_kind),
                    control: route
                        .driver
                        .as_ref()
                        .and_then(|driver| driver.route_control_expectation),
                    targets: BTreeSet::new(),
                    admission_sources: route_admission_sources(route),
                    registration_sources: catalog_coverage_executable_registration_sources(
                        route,
                        catalog_coverage_eligible,
                    ),
                });
            if targets.primary != route.metadata.source.path
                || targets.kind != Some(route.metadata.watch_target_kind)
            {
                targets.kind = None;
                targets.admission_sources = None;
            }
            let route_control = route
                .driver
                .as_ref()
                .and_then(|driver| driver.route_control_expectation.as_ref());
            if targets.control.as_ref() != route_control {
                targets.control = None;
                targets.admission_sources = None;
                targets.registration_sources = None;
            }
            match (
                targets.admission_sources.as_mut(),
                route_admission_sources(route),
            ) {
                (Some(existing), Some(additional)) => {
                    existing.extend(additional);
                    sort_and_dedup_sources(existing);
                }
                _ => targets.admission_sources = None,
            }
            if targets.registration_sources
                != catalog_coverage_executable_registration_sources(
                    route,
                    catalog_coverage_eligible,
                )
            {
                targets.registration_sources = None;
            }
            insert_route_watch_targets(
                &mut targets.targets,
                &route.metadata.source.path,
                route.metadata.watch_target_kind,
            );
            for source in &route.registration_sources {
                insert_route_watch_targets(
                    &mut targets.targets,
                    &source.path,
                    route.metadata.watch_target_kind,
                );
            }
            for missing in &route.certified_missing_paths {
                insert_route_watch_targets(
                    &mut targets.targets,
                    missing,
                    route.metadata.watch_target_kind,
                );
            }
            if let Some(observe_targets) = route
                .driver
                .as_ref()
                .and_then(|driver| driver.watch_targets.as_ref())
            {
                // A finite inventory is watched across every exact admitted
                // database and its authority parent. Directory metadata is
                // deliberately not a certified warm-skip token, so these
                // routes remain Indeterminate while still receiving complete
                // live invalidation coverage.
                targets.kind = None;
                if let Some(observed) = observe_targets() {
                    for database in observed.sqlite_databases {
                        insert_route_watch_targets(
                            &mut targets.targets,
                            &database,
                            SourceBackedWatchTargetKind::SqliteDatabase,
                        );
                    }
                    targets.targets.extend(observed.authority_paths);
                }
            }
        }
        catalog
    }
}

fn route_admission_sources(route: &SourceBackedRoute) -> Option<Vec<ProviderSource>> {
    route.metadata.selection.is_some().then(|| {
        if route.registration_sources.is_empty() {
            vec![route.metadata.source.clone()]
        } else {
            route.registration_sources.clone()
        }
    })
}

fn sort_and_dedup_sources(sources: &mut Vec<ProviderSource>) {
    sources.sort_by(|left, right| {
        left.provider
            .as_str()
            .cmp(right.provider.as_str())
            .then_with(|| left.source_format.cmp(right.source_format))
            .then_with(|| left.path.cmp(&right.path))
    });
    sources.dedup();
}

fn refresh_admission_source_presence(mut source: ProviderSource) -> ProviderSource {
    match fs::symlink_metadata(&source.path) {
        Ok(_) => {
            source.exists = true;
            if source.status == ProviderSourceStatus::Missing {
                source.status = ProviderSourceStatus::Available;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            source.exists = false;
            source.status = ProviderSourceStatus::Missing;
        }
        Err(_) => {}
    }
    source
}

fn catalog_coverage_executable_registration_sources(
    route: &SourceBackedRoute,
    configured: bool,
) -> Option<Vec<RegisteredSource>> {
    ((route.metadata.selection == Some(SourceBackedRouteSelection::Automatic) || configured)
        && route.driver.is_some()
        && route.certified_missing_paths.is_empty()
        && !route.registration_sources.is_empty())
    .then(|| {
        route
            .registration_sources
            .iter()
            .map(|source| {
                Some(RegisteredSource {
                    path_kind: registered_path_kind(&source.path)?,
                    source: source.clone(),
                })
            })
            .collect::<Option<Vec<_>>>()
    })
    .flatten()
}

fn member_belongs_to_root(member: &Path, root: &Path) -> bool {
    member == root
        || fs::symlink_metadata(root)
            .ok()
            .is_some_and(|metadata| metadata.file_type().is_dir() && member.starts_with(root))
}

fn registered_path_kind(path: &Path) -> Option<RegisteredPathKind> {
    fs::symlink_metadata(path).ok().and_then(|metadata| {
        let kind = metadata.file_type();
        if kind.is_symlink() {
            None
        } else if kind.is_file() {
            Some(RegisteredPathKind::File)
        } else if kind.is_dir() {
            Some(RegisteredPathKind::Directory)
        } else {
            None
        }
    })
}

fn registration_source_is_available(source: &RegisteredSource) -> bool {
    source.source.exists && registered_path_kind(&source.source.path) == Some(source.path_kind)
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
            route_provenance: Default::default(),
        }
    }

    fn driver() -> SourceBackedRouteDriver {
        SourceBackedRouteDriver::new(|_| Ok(()), |_| false, |_| true)
    }

    fn automatic_route(
        provider: CaptureProvider,
        path: PathBuf,
        source_format: &'static str,
    ) -> SourceBackedRoute {
        SourceBackedRoute::automatic(
            source(provider, path, source_format),
            SourceBackedSelectorAuthority::DiscoveredWinner,
            driver(),
        )
        .unwrap()
    }

    fn catalog_for(route: SourceBackedRoute) -> (SourceBackedWatchCatalog, SourceRouteIdentity) {
        let identity = route.metadata.route_identity.clone().unwrap();
        let mut registry = SourceBackedProviderRegistry::new();
        registry.register(route);
        (registry.watch_catalog(), identity)
    }

    #[test]
    fn sqlite_route_authorizes_only_its_exact_database_family() {
        let database = PathBuf::from("/provider/state.db");
        let (catalog, identity) = catalog_for(automatic_route(
            CaptureProvider::OpenCode,
            database.clone(),
            "opencode_sqlite",
        ));
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
    fn dynamic_sqlite_inventory_keeps_route_local_discovery_authority() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("profiles/active/state.db");
        fs::create_dir_all(database.parent().unwrap()).unwrap();
        fs::write(&database, b"sqlite").unwrap();
        let source = source(
            CaptureProvider::OpenCode,
            database.clone(),
            "opencode_sqlite",
        );
        let route = SourceBackedRoute::automatic(
            source.clone(),
            SourceBackedSelectorAuthority::DiscoveredWinner,
            driver(),
        )
        .unwrap();
        let identity = route.metadata.route_identity.clone().unwrap();
        let mut registry = SourceBackedProviderRegistry::new();
        registry.register(route);
        let observed_database = database.clone();
        registry
            .attach_route_watch_targets(&source, move || {
                Some(SourceBackedRouteWatchTargets {
                    sqlite_databases: BTreeSet::from([observed_database.clone()]),
                    authority_paths: BTreeSet::from([observed_database
                        .parent()
                        .unwrap()
                        .to_path_buf()]),
                })
            })
            .unwrap();

        let catalog = registry.watch_catalog();
        assert!(catalog
            .exact_member_for_event(&identity, &database)
            .is_none());
        let report = catalog
            .route_discovery_report(&BTreeSet::from([identity]))
            .expect("dynamic SQLite inventory retains its registered route input");
        assert_eq!(report.sources, vec![source]);
    }

    #[test]
    fn ordinary_file_route_does_not_invent_sqlite_companions_or_match_siblings() {
        let source_path = PathBuf::from("/provider/history.jsonl");
        let (catalog, _) = catalog_for(automatic_route(
            CaptureProvider::Codex,
            source_path.clone(),
            "codex_history_jsonl",
        ));
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
    fn exact_catalog_reuses_one_direct_claude_registration_without_global_discovery() {
        let temp = tempfile::tempdir().unwrap();
        let member = temp.path().join("session.jsonl");
        fs::write(&member, b"{}\n").unwrap();
        let (catalog, identity) = catalog_for(automatic_route(
            CaptureProvider::Claude,
            member.clone(),
            "claude_projects_jsonl_tree",
        ));
        let worksets = BTreeMap::from([(identity.clone(), BTreeSet::from([member.clone()]))]);

        assert_eq!(
            catalog.exact_member_for_event(&identity, &member),
            Some(member)
        );
        let report = catalog
            .exact_member_discovery_report(&BTreeSet::from([identity.clone()]), &worksets)
            .expect("direct ordinary route remains catalog-authorized");
        assert_eq!(report.sources.len(), 1);
        assert_eq!(report.sources[0].provider, CaptureProvider::Claude);

        let directory_member = temp.path().join("not-a-member");
        fs::create_dir_all(&directory_member).unwrap();
        assert!(catalog
            .exact_member_discovery_report(
                &BTreeSet::from([identity.clone()]),
                &BTreeMap::from([(identity.clone(), BTreeSet::from([directory_member]))]),
            )
            .is_none());
    }

    #[test]
    fn exact_catalog_reconstructs_compound_codex_roots_and_rejects_uncertainty() {
        let temp = tempfile::tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        let archived = temp.path().join("archived_sessions");
        let active_member = sessions.join("2026/08/rollout.jsonl");
        let archived_member = archived.join("2026/08/rollout.jsonl");
        fs::create_dir_all(active_member.parent().unwrap()).unwrap();
        fs::create_dir_all(archived_member.parent().unwrap()).unwrap();
        fs::write(&active_member, b"{}\n").unwrap();
        fs::write(&archived_member, b"{}\n").unwrap();
        let active = source(
            CaptureProvider::Codex,
            sessions.clone(),
            "codex_session_jsonl_tree",
        );
        let archived_source = source(
            CaptureProvider::Codex,
            archived.clone(),
            "codex_session_jsonl_tree",
        );
        let mut route = SourceBackedRoute::automatic(
            active.clone(),
            SourceBackedSelectorAuthority::DiscoveredWinner,
            driver(),
        )
        .unwrap();
        route.registration_sources = vec![active.clone(), archived_source.clone()];
        let (catalog, identity) = catalog_for(route);
        let worksets = BTreeMap::from([(
            identity.clone(),
            BTreeSet::from([active_member.clone(), archived_member.clone()]),
        )]);

        assert_eq!(
            catalog.exact_member_for_event(&identity, &active_member),
            Some(active_member.clone())
        );
        assert_eq!(
            catalog.exact_member_for_event(&identity, &archived_member),
            Some(archived_member.clone())
        );
        let report = catalog
            .exact_member_discovery_report(&BTreeSet::from([identity.clone()]), &worksets)
            .expect("each Codex member has exactly one retained root");
        assert_eq!(
            report.sources,
            vec![archived_source.clone(), active.clone()]
        );

        fs::remove_file(&archived_member).unwrap();
        assert!(catalog
            .exact_member_discovery_report(&BTreeSet::from([identity.clone()]), &worksets)
            .is_none());
        fs::remove_dir_all(&sessions).unwrap();
        fs::write(&sessions, b"not a directory").unwrap();
        assert!(catalog
            .route_discovery_report(&BTreeSet::from([identity]))
            .is_none());
    }

    #[test]
    fn exact_catalog_abstains_for_ambiguous_roots_non_files_and_sqlite() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sessions");
        let nested = root.join("nested");
        let member = nested.join("rollout.jsonl");
        fs::create_dir_all(&nested).unwrap();
        fs::write(&member, b"{}\n").unwrap();
        let primary = source(CaptureProvider::Codex, root, "codex_session_jsonl_tree");
        let nested_source = source(
            CaptureProvider::Codex,
            nested.clone(),
            "codex_session_jsonl_tree",
        );
        let mut route = SourceBackedRoute::automatic(
            primary.clone(),
            SourceBackedSelectorAuthority::DiscoveredWinner,
            driver(),
        )
        .unwrap();
        route.registration_sources = vec![primary, nested_source];
        let (catalog, identity) = catalog_for(route);
        let worksets = BTreeMap::from([(identity.clone(), BTreeSet::from([member.clone()]))]);
        assert!(catalog.exact_member_for_event(&identity, &member).is_none());
        assert!(catalog
            .exact_member_discovery_report(&BTreeSet::from([identity]), &worksets)
            .is_none());

        let sqlite = temp.path().join("history.sqlite");
        fs::write(&sqlite, b"sqlite").unwrap();
        let (catalog, identity) = catalog_for(automatic_route(
            CaptureProvider::OpenCode,
            sqlite.clone(),
            "opencode_sqlite",
        ));
        let worksets = BTreeMap::from([(identity.clone(), BTreeSet::from([sqlite.clone()]))]);
        assert!(catalog.exact_member_for_event(&identity, &sqlite).is_none());
        assert!(catalog
            .exact_member_discovery_report(&BTreeSet::from([identity.clone()]), &worksets)
            .is_none());
        let report = catalog
            .route_discovery_report(&BTreeSet::from([identity.clone()]))
            .expect("SQLite keeps its registered route while abstaining from member work");
        assert_eq!(report.sources.len(), 1);
        assert_eq!(report.sources[0].provider, CaptureProvider::OpenCode);
        fs::remove_file(&sqlite).unwrap();
        assert!(catalog
            .route_discovery_report(&BTreeSet::from([identity.clone()]))
            .is_none());
        let admitted = catalog
            .route_admission_report(&BTreeSet::from([identity.clone()]))
            .expect("exact admission retains the missing route's bounded source descriptor");
        assert_eq!(admitted.sources.len(), 1);
        assert!(!admitted.sources[0].exists);
        assert_eq!(admitted.sources[0].status, ProviderSourceStatus::Missing);
        fs::create_dir(&sqlite).unwrap();
        assert!(catalog
            .route_discovery_report(&BTreeSet::from([identity]))
            .is_none());
    }

    #[test]
    fn ordinary_file_observation_catches_append_rewrite_and_delete() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("history.jsonl");
        fs::write(&source_path, b"one\n").unwrap();
        let (catalog, identity) = catalog_for(automatic_route(
            CaptureProvider::Codex,
            source_path.clone(),
            "codex_history_jsonl",
        ));
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
        let (catalog, identity) = catalog_for(automatic_route(
            CaptureProvider::OpenCode,
            database.clone(),
            "opencode_sqlite",
        ));
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
        let (catalog, identity) = catalog_for(automatic_route(
            CaptureProvider::Codex,
            temp.path().to_path_buf(),
            "codex_history_jsonl",
        ));

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
