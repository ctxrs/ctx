use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceBackedAutomaticUnavailableReason {
    SourceStatus(ProviderSourceStatus),
    UnsafeRootOverlap { detail: String },
    UnsupportedFormat { detail: &'static str },
    SelectorAuthorityUnavailable { detail: &'static str },
    RegistrationRejected { detail: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceBackedAutomaticRegistryIssue {
    Discovery(DiscoveryIssue),
    Unavailable {
        source: ProviderSource,
        reason: SourceBackedAutomaticUnavailableReason,
    },
}

#[derive(Debug, Clone)]
pub struct SourceBackedAutomaticRegistryBuild {
    pub registry: SourceBackedProviderRegistry,
    pub issues: Vec<SourceBackedAutomaticRegistryIssue>,
    pub discovery_duration: Duration,
}

impl SourceBackedAutomaticRegistryBuild {
    pub fn executable_route_count(&self) -> usize {
        self.registry.executable_route_count()
    }

    pub fn unsupported_route_count(&self) -> usize {
        self.registry.unsupported_route_count()
    }

    pub fn into_parts(
        self,
    ) -> (
        SourceBackedProviderRegistry,
        Vec<SourceBackedAutomaticRegistryIssue>,
    ) {
        (self.registry, self.issues)
    }

    pub fn into_refresh_executor(
        self,
        writer_options: WriterOptions,
    ) -> (
        SourceBackedRefreshExecutor,
        Vec<SourceBackedAutomaticRegistryIssue>,
    ) {
        (
            SourceBackedRefreshExecutor::with_discovery_duration(
                self.registry,
                writer_options,
                self.discovery_duration,
            ),
            self.issues,
        )
    }
}

/// Discovers and registers every automatic source-backed route capture can
/// construct without daemon-side provider branching.
///
/// Normal provider absence and selector/discovery limitations are returned as
/// typed issues. A detected format whose adapter seam is unavailable is also
/// retained as a typed unsupported route, so refresh and hydration cannot
/// silently claim it.
pub fn build_automatic_source_backed_registry(
    discovery: &DiscoveryContext,
    data_root: &Path,
) -> SourceBackedAutomaticRegistryBuild {
    let discovery_started = Instant::now();
    let discovery = discovery.clone().with_data_root(data_root);
    let report = discover_provider_sources_with_context(&discovery);
    let mut build =
        build_automatic_source_backed_registry_from_report(&discovery, data_root, report);
    build.discovery_duration = discovery_started.elapsed();
    build
}

/// Registers automatic routes from one already-completed discovery report.
///
/// Callers that must validate source roots before their first persistent write
/// can pass the same report through registration instead of traversing every
/// provider tree a second time.
pub fn build_automatic_source_backed_registry_from_report(
    discovery: &DiscoveryContext,
    data_root: &Path,
    report: DiscoveryReport,
) -> SourceBackedAutomaticRegistryBuild {
    build_automatic_source_backed_registry_from_parts(
        discovery,
        data_root,
        report.sources,
        report.issues,
    )
}

pub(in crate::provider::source_backed) fn build_automatic_source_backed_registry_from_parts(
    discovery: &DiscoveryContext,
    data_root: &Path,
    sources: Vec<ProviderSource>,
    discovery_issues: Vec<DiscoveryIssue>,
) -> SourceBackedAutomaticRegistryBuild {
    let mut registry = SourceBackedProviderRegistry::new();
    let mut issues = discovery_issues
        .into_iter()
        .map(SourceBackedAutomaticRegistryIssue::Discovery)
        .collect::<Vec<_>>();
    let mut compound_provider_registered = HashSet::new();
    let mut codex_session_tree_sources = Vec::new();

    for source in sources {
        if let Err(error) =
            validate_provider_source_roots_outside_data_root(data_root, std::iter::once(&source))
        {
            let reason = SourceBackedAutomaticUnavailableReason::UnsafeRootOverlap {
                detail: error.to_string(),
            };
            registry.register(SourceBackedRoute::unsupported(
                source.clone(),
                automatic_unavailable_detail(&reason),
            ));
            issues.push(SourceBackedAutomaticRegistryIssue::Unavailable { source, reason });
            continue;
        }
        if source.import_support == ProviderImportSupport::Explicit {
            continue;
        }
        if source.import_support == ProviderImportSupport::Unsupported
            || source.source_kind == ProviderSourceKind::DetectionOnly
            || source.status == ProviderSourceStatus::Unsupported
            || (source.unsupported_reason.is_some() && source.status != ProviderSourceStatus::Empty)
        {
            let detail = source
                .unsupported_reason
                .unwrap_or("the detected provider format is not supported for automatic refresh");
            retain_unsupported_automatic_format(&mut registry, &mut issues, source, detail);
            continue;
        }
        if source.status == ProviderSourceStatus::Unknown {
            let reason = SourceBackedAutomaticUnavailableReason::SourceStatus(source.status);
            registry.register(SourceBackedRoute::unsupported(
                source.clone(),
                automatic_unavailable_detail(&reason),
            ));
            issues.push(SourceBackedAutomaticRegistryIssue::Unavailable { reason, source });
            continue;
        }

        let Some(format_route) = landed_format_route(source.provider, source.source_format) else {
            retain_unsupported_automatic_format(
                &mut registry,
                &mut issues,
                source,
                "the discovered provider format has no landed source-backed route",
            );
            continue;
        };
        if !format_route.automatic {
            retain_unsupported_automatic_format(
                &mut registry,
                &mut issues,
                source,
                "the discovered provider format is not registered for automatic refresh",
            );
            continue;
        }
        if let Some(reason) = format_route.unsupported_reason {
            retain_unsupported_automatic_format(&mut registry, &mut issues, source, reason);
            continue;
        }

        if source.status == ProviderSourceStatus::Missing {
            match SourceBackedRoute::certified_missing(
                source.clone(),
                format_route.selector_authority,
            ) {
                Ok(route) => registry.register(route),
                Err(error) => {
                    let reason = SourceBackedAutomaticUnavailableReason::RegistrationRejected {
                        detail: error.to_string(),
                    };
                    registry.register(SourceBackedRoute::unsupported(
                        source.clone(),
                        automatic_unavailable_detail(&reason),
                    ));
                    issues.push(SourceBackedAutomaticRegistryIssue::Unavailable { source, reason });
                }
            }
            continue;
        }

        if !matches!(
            source.status,
            ProviderSourceStatus::Available | ProviderSourceStatus::Empty
        ) {
            let reason = SourceBackedAutomaticUnavailableReason::SourceStatus(source.status);
            registry.register(SourceBackedRoute::unsupported(
                source.clone(),
                automatic_unavailable_detail(&reason),
            ));
            issues.push(SourceBackedAutomaticRegistryIssue::Unavailable { source, reason });
            continue;
        }

        let mut source = source;
        if source.status == ProviderSourceStatus::Empty {
            // Resolver diagnostics explain why a present root is empty; they do
            // not make its landed adapter unsupported.
            source.unsupported_reason = None;
        }
        if source.provider == CaptureProvider::Codex
            && source.source_format == "codex_session_jsonl_tree"
        {
            codex_session_tree_sources.push(source);
            continue;
        }

        let compound_provider = matches!(
            format_route.constructor,
            SourceBackedRouteConstructor::FiniteInventory
                | SourceBackedRouteConstructor::DiscoveryContext
        );
        if compound_provider && compound_provider_registered.contains(&source.provider) {
            continue;
        }

        match register_discovered_automatic_route(
            &mut registry,
            discovery,
            data_root,
            format_route.constructor,
            source.clone(),
        ) {
            Ok(()) => {
                if compound_provider {
                    compound_provider_registered.insert(source.provider);
                }
            }
            Err(reason) => {
                if compound_provider {
                    compound_provider_registered.insert(source.provider);
                }
                registry.register(SourceBackedRoute::unsupported(
                    source.clone(),
                    automatic_unavailable_detail(&reason),
                ));
                issues.push(SourceBackedAutomaticRegistryIssue::Unavailable { source, reason });
            }
        }
    }

    if !codex_session_tree_sources.is_empty() {
        codex_session_tree_sources.sort_by(|left, right| {
            codex_automatic_session_root_rank(&left.path)
                .cmp(&codex_automatic_session_root_rank(&right.path))
                .then_with(|| left.path.cmp(&right.path))
        });
        codex_session_tree_sources.dedup_by(|left, right| left.path == right.path);
        let source = codex_session_tree_sources.first().cloned();
        let registration = register_codex_session_tree_routes(
            &mut registry,
            codex_session_tree_sources,
            SourceBackedRouteSelection::Automatic,
        );
        if let (Some(source), Err(error)) = (source, registration) {
            let reason = SourceBackedAutomaticUnavailableReason::RegistrationRejected {
                detail: error.to_string(),
            };
            registry.register(SourceBackedRoute::unsupported(
                source.clone(),
                automatic_unavailable_detail(&reason),
            ));
            issues.push(SourceBackedAutomaticRegistryIssue::Unavailable { source, reason });
        }
    }

    SourceBackedAutomaticRegistryBuild {
        registry,
        issues,
        discovery_duration: Duration::ZERO,
    }
}

fn codex_automatic_session_root_rank(root: &Path) -> u8 {
    match root.file_name().and_then(std::ffi::OsStr::to_str) {
        Some("sessions") => 0,
        Some("archived_sessions") => 1,
        _ => 2,
    }
}

fn retain_unsupported_automatic_format(
    registry: &mut SourceBackedProviderRegistry,
    issues: &mut Vec<SourceBackedAutomaticRegistryIssue>,
    source: ProviderSource,
    detail: &'static str,
) {
    registry.register(SourceBackedRoute::unsupported(source.clone(), detail));
    issues.push(SourceBackedAutomaticRegistryIssue::Unavailable {
        source,
        reason: SourceBackedAutomaticUnavailableReason::UnsupportedFormat { detail },
    });
}

fn automatic_unavailable_detail(reason: &SourceBackedAutomaticUnavailableReason) -> String {
    match reason {
        SourceBackedAutomaticUnavailableReason::SourceStatus(status) => {
            format!("provider source status is {}", status.as_str())
        }
        SourceBackedAutomaticUnavailableReason::UnsafeRootOverlap { detail }
        | SourceBackedAutomaticUnavailableReason::RegistrationRejected { detail } => detail.clone(),
        SourceBackedAutomaticUnavailableReason::UnsupportedFormat { detail }
        | SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable { detail } => {
            (*detail).to_owned()
        }
    }
}

fn register_discovered_automatic_route(
    registry: &mut SourceBackedProviderRegistry,
    discovery: &DiscoveryContext,
    data_root: &Path,
    constructor: SourceBackedRouteConstructor,
    source: ProviderSource,
) -> Result<(), SourceBackedAutomaticUnavailableReason> {
    let result = match (constructor, source.provider) {
        (SourceBackedRouteConstructor::NamedSurface, CaptureProvider::Warp) => {
            let selected =
                resolve_warp_discovery_authority(discovery, &source).map_err(|error| {
                    SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable {
                        detail: warp_discovery_unavailable_detail(error),
                    }
                })?;
            register_warp_source_backed_route(
                registry,
                source,
                SourceBackedRouteSelection::Automatic,
                data_root,
                selected.surface_key().as_str(),
            )
        }
        (SourceBackedRouteConstructor::SelectedWithRetainedRoutes, CaptureProvider::Goose) => {
            let platform_root = goose_platform_root(discovery, &source.path).ok_or(
                SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable {
                    detail: "Goose discovery selected a database without its exact platform root",
                },
            )?;
            register_goose_source_backed_route(
                registry,
                source,
                SourceBackedRouteSelection::Automatic,
                data_root,
                platform_root,
                Vec::new(),
            )
        }
        (SourceBackedRouteConstructor::FiniteInventory, CaptureProvider::Crush) => {
            let inventory_source = discovered_crush_inventory_source(discovery, &source)?;
            register_crush_source_backed_route(
                registry,
                source,
                SourceBackedRouteSelection::Automatic,
                data_root,
                inventory_source,
            )
        }
        (SourceBackedRouteConstructor::FiniteInventory, CaptureProvider::Lingma) => {
            let inventory_source = discovered_lingma_inventory_source(discovery, &source)?;
            register_lingma_inventory_source(
                registry,
                source,
                SourceBackedRouteSelection::Automatic,
                data_root,
                inventory_source,
            )
        }
        (SourceBackedRouteConstructor::DiscoveryContext, CaptureProvider::AstrBot) => {
            register_astrbot_source_backed_route(
                registry,
                source,
                SourceBackedRouteSelection::Automatic,
                data_root,
                discovery.clone(),
            )
        }
        (SourceBackedRouteConstructor::ExactCwd, CaptureProvider::Shelley) => {
            let exact_cwd = discovery.cwd().ok_or(
                SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable {
                    detail: "Shelley automatic registration requires the exact discovery CWD",
                },
            )?;
            register_shelley_source_backed_route(registry, source, data_root, exact_cwd)
        }
        (SourceBackedRouteConstructor::ProviderSource, _) => {
            register_landed_source_backed_route_with_data_root(
                registry,
                source,
                SourceBackedRouteSelection::Automatic,
                data_root,
            )
        }
        _ => Err(invalid_route(
            source.provider,
            "the landed route constructor does not match its provider registration callback",
        )),
    };
    result.map_err(
        |error| SourceBackedAutomaticUnavailableReason::RegistrationRejected {
            detail: error.to_string(),
        },
    )
}

fn goose_platform_root(discovery: &DiscoveryContext, database: &Path) -> Option<PathBuf> {
    if let Some(root) = discovery
        .env("GOOSE_PATH_ROOT")
        .filter(|value| !value.is_empty())
    {
        let root = PathBuf::from(root);
        if root.is_absolute() && database == root.join("data/sessions/sessions.db") {
            return Some(root);
        }
    }
    let root = match discovery.platform() {
        DiscoveryPlatform::Linux | DiscoveryPlatform::MacOS => {
            match discovery.env("XDG_DATA_HOME") {
                Some(value) if !value.is_empty() && Path::new(value).is_absolute() => {
                    PathBuf::from(value).join("goose")
                }
                _ => discovery.home().join(".local/share/goose"),
            }
        }
        DiscoveryPlatform::Windows => discovery
            .platform_dirs()
            .data
            .as_ref()?
            .join("Block/goose/data"),
        DiscoveryPlatform::OtherUnix => {
            let value = discovery
                .env("XDG_DATA_HOME")
                .filter(|value| !value.is_empty() && Path::new(value).is_absolute())?;
            PathBuf::from(value).join("goose")
        }
    };
    (database == root.join("sessions/sessions.db")).then_some(root)
}

const fn warp_discovery_unavailable_detail(error: WarpDiscoveryUnavailable) -> &'static str {
    match error {
        WarpDiscoveryUnavailable::UnsupportedPlatform { .. } => {
            "Warp installed-surface authority is unavailable on this platform"
        }
        WarpDiscoveryUnavailable::WindowsLocalDataRootUnavailable => {
            "Warp installed-surface authority has no Windows local-data root"
        }
        WarpDiscoveryUnavailable::ProviderSpecUnavailable => {
            "Warp provider discovery specification is unavailable"
        }
        WarpDiscoveryUnavailable::SourceCandidateRejected { .. } => {
            "Warp installed-surface discovery rejected the selected source within fixed bounds"
        }
        WarpDiscoveryUnavailable::SourceNotSelected => {
            "Warp source is absent from authoritative installed-surface discovery"
        }
    }
}

#[derive(Debug, Clone)]
struct DiscoveredCrushInventorySource {
    selector: CrushProjectInventorySelector,
    spec: &'static ProviderSourceSpec,
}

impl CrushProjectInventorySourceV0 for DiscoveredCrushInventorySource {
    fn observe(&self) -> CrushSourceBackedResultV0<CrushProjectInventoryObservationV0> {
        self.selector
            .observe(self.spec)
            .map_err(crush_selector_adapter_error)
            .and_then(crush_adapter_inventory)
    }
}

fn discovered_crush_inventory_source(
    discovery: &DiscoveryContext,
    selected_source: &ProviderSource,
) -> Result<Arc<DiscoveredCrushInventorySource>, SourceBackedAutomaticUnavailableReason> {
    let spec = provider_source_spec(CaptureProvider::Crush).ok_or(
        SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable {
            detail: "Crush provider discovery specification is unavailable",
        },
    )?;
    let source = Arc::new(DiscoveredCrushInventorySource {
        selector: CrushProjectInventorySelector::new(discovery.clone()),
        spec,
    });
    let opening = source.selector.observe(spec).map_err(|error| {
        SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable {
            detail: error.detail(),
        }
    })?;
    if !opening
        .databases()
        .iter()
        .any(|database| database.database_path() == selected_source.path)
    {
        return Err(
            SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable {
                detail:
                    "Crush selected database is absent from its authoritative project inventory",
            },
        );
    }
    crush_adapter_inventory(opening).map_err(|error| {
        SourceBackedAutomaticUnavailableReason::RegistrationRejected {
            detail: error.to_string(),
        }
    })?;
    Ok(source)
}

fn crush_adapter_inventory(
    inventory: CrushDiscoveredProjectInventory,
) -> CrushSourceBackedResultV0<CrushProjectInventoryObservationV0> {
    let authority_key = inventory
        .authority_key()
        .map_err(crush_selector_adapter_error)?;
    let databases = inventory
        .databases()
        .iter()
        .map(|database| {
            let project_key = database
                .selector_key()
                .typed_key()
                .map_err(crush_selector_adapter_error)?;
            CrushProjectDatabaseV0::new(project_key, database.database_path())
        })
        .collect::<CrushSourceBackedResultV0<Vec<_>>>()?;
    CrushProjectInventoryObservationV0::new(authority_key, inventory.revision().to_vec(), databases)
}

fn crush_selector_adapter_error(
    error: CrushProjectInventorySelectorError,
) -> CrushSourceBackedErrorV0 {
    CaptureError::InvalidPayload(error.to_string()).into()
}
