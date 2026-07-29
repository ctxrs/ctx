use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceBackedAutomaticUnavailableReason {
    SourceStatus(ProviderSourceStatus),
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
) -> SourceBackedAutomaticRegistryBuild {
    let discovery_started = Instant::now();
    let report = discover_provider_sources_with_context(discovery);
    let mut build = build_automatic_source_backed_registry_from_report(
        discovery,
        report.sources,
        report.issues,
    );
    build.discovery_duration = discovery_started.elapsed();
    build
}

pub(in crate::provider::source_backed) fn build_automatic_source_backed_registry_from_report(
    discovery: &DiscoveryContext,
    sources: Vec<ProviderSource>,
    discovery_issues: Vec<DiscoveryIssue>,
) -> SourceBackedAutomaticRegistryBuild {
    let mut registry = SourceBackedProviderRegistry::new();
    let mut issues = discovery_issues
        .into_iter()
        .map(SourceBackedAutomaticRegistryIssue::Discovery)
        .collect::<Vec<_>>();
    let mut compound_provider_registered = HashSet::new();

    for source in sources {
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
        if !matches!(
            source.status,
            ProviderSourceStatus::Available | ProviderSourceStatus::Empty
        ) {
            issues.push(SourceBackedAutomaticRegistryIssue::Unavailable {
                reason: SourceBackedAutomaticUnavailableReason::SourceStatus(source.status),
                source,
            });
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

        let mut source = source;
        if source.status == ProviderSourceStatus::Empty {
            // Resolver diagnostics explain why a present root is empty; they do
            // not make its landed adapter unsupported.
            source.unsupported_reason = None;
        }

        let compound_provider = matches!(
            source.provider,
            CaptureProvider::AstrBot | CaptureProvider::Crush | CaptureProvider::Lingma
        );
        if compound_provider && compound_provider_registered.contains(&source.provider) {
            continue;
        }

        match register_discovered_automatic_route(&mut registry, discovery, source.clone()) {
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

    SourceBackedAutomaticRegistryBuild {
        registry,
        issues,
        discovery_duration: Duration::ZERO,
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
        SourceBackedAutomaticUnavailableReason::UnsupportedFormat { detail }
        | SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable { detail } => {
            (*detail).to_owned()
        }
        SourceBackedAutomaticUnavailableReason::RegistrationRejected { detail } => detail.clone(),
    }
}

fn register_discovered_automatic_route(
    registry: &mut SourceBackedProviderRegistry,
    discovery: &DiscoveryContext,
    source: ProviderSource,
) -> Result<(), SourceBackedAutomaticUnavailableReason> {
    let result = match source.provider {
        CaptureProvider::Warp => {
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
                selected.surface_key().as_str(),
            )
        }
        CaptureProvider::Goose => {
            let platform_root = goose_platform_root(discovery, &source.path).ok_or(
                SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable {
                    detail: "Goose discovery selected a database without its exact platform root",
                },
            )?;
            register_goose_source_backed_route(
                registry,
                source,
                SourceBackedRouteSelection::Automatic,
                platform_root,
                Vec::new(),
            )
        }
        CaptureProvider::Crush => {
            let inventory_source = discovered_crush_inventory_source(discovery, &source)?;
            register_crush_source_backed_route(
                registry,
                source,
                SourceBackedRouteSelection::Automatic,
                inventory_source,
            )
        }
        CaptureProvider::Lingma => {
            let inventory_source = discovered_lingma_inventory_source(discovery, &source)?;
            register_lingma_inventory_source(
                registry,
                source,
                SourceBackedRouteSelection::Automatic,
                inventory_source,
            )
        }
        CaptureProvider::AstrBot => register_astrbot_source_backed_route(
            registry,
            source,
            SourceBackedRouteSelection::Automatic,
            discovery.clone(),
        ),
        CaptureProvider::Shelley => {
            let exact_cwd = discovery.cwd().ok_or(
                SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable {
                    detail: "Shelley automatic registration requires the exact discovery CWD",
                },
            )?;
            register_shelley_source_backed_route(registry, source, exact_cwd)
        }
        _ => register_landed_source_backed_route(
            registry,
            source,
            SourceBackedRouteSelection::Automatic,
        ),
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
