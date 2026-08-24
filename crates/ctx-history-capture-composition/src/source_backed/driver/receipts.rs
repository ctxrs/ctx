use super::super::*;

mod refresh_control_plane;

#[cfg(test)]
thread_local! {
    static BASE_SOURCE_MANIFEST_VISITS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn record_base_source_manifest_visit() {
    BASE_SOURCE_MANIFEST_VISITS.with(|visits| visits.set(visits.get().saturating_add(1)));
}

pub use ctx_history_capture_runtime::{
    CompleteInventoryOwner, SourceBackedCertifiedRemoval, SourceBackedCurrentSourceProgress,
    SourceBackedCurrentSourceProgressStage, SourceBackedFailedRoute,
    SourceBackedFailedRouteOutcome, SourceBackedLogicalSourceFailure,
    SourceBackedLogicalSourceFailures, SourceBackedReconciliationDemand,
    SourceBackedRecordCompletion, SourceBackedRecordRejection, SourceBackedRecordRejectionClass,
    SourceBackedRecordRejections, SourceBackedRefreshScope, SourceBackedRevalidationTarget,
    SourceBackedRouteConstructor, SourceBackedRouteError, SourceBackedRouteErrorKind,
    SourceBackedRouteResult, SourceBackedRouteRevalidation, SourceBackedRouteSelection,
    SourceBackedRouteWatchTargets, SourceBackedSelectorAuthority, SourceBackedSourceFailureClass,
    SourceBackedSourceFailures, SourceBackedWatchTargetKind, SourceOwner,
    MAX_RECORDED_SOURCE_BACKED_FAILURES, MAX_SOURCE_BACKED_FAILURE_DETAIL_BYTES,
    MAX_SOURCE_BACKED_FAILURE_SELECTOR_BYTES, MAX_SOURCE_BACKED_ROUTE_CONTROL_BYTES,
};
use ctx_history_capture_runtime::{
    SourceBackedCoordinatorError as RuntimeSourceBackedCoordinatorError,
    SourceBackedGenerationSink as RuntimeSourceBackedGenerationSink, SourceBackedRegistryRoute,
    SourceBackedRouteMetadata as RuntimeSourceBackedRouteMetadata, SourceBackedRouteRegistry,
};

pub type SourceBackedCoordinatorError = RuntimeSourceBackedCoordinatorError<IndexError>;
pub type SourceBackedCoordinatorResult<T> = Result<T, SourceBackedCoordinatorError>;
pub type SourceBackedGenerationSink<'writer> =
    RuntimeSourceBackedGenerationSink<'writer, IndexCaptureLifecycle>;
pub type SourceBackedRouteDriver = ctx_history_provider_runtime::ProviderRouteDriver<
    crate::provider::source_backed::family::CaptureProviderRuntime,
>;

/// Runtime metadata for one selected source route.
pub type SourceBackedRouteMetadata = RuntimeSourceBackedRouteMetadata<ProviderSource>;
pub(in super::super) fn source_backed_failed_route_from_route(
    route: &SourceBackedRoute,
    class: SourceBackedSourceFailureClass,
    carried_forward: bool,
    detail: impl AsRef<str>,
) -> SourceBackedCoordinatorResult<SourceBackedFailedRoute> {
    let route_identity = route.metadata.route_identity.clone().ok_or_else(|| {
        SourceBackedCoordinatorError::InvalidRoute {
            provider: route.metadata.source.provider,
            detail: "failed executable route has no route identity".to_owned(),
        }
    })?;
    Ok(SourceBackedFailedRoute::new(
        route_identity,
        source_backed_source_failure_identity(&route.metadata.source)?,
        route.metadata.source.provider,
        class,
        carried_forward,
        route.metadata.source.path.display().to_string(),
        detail,
    ))
}

#[derive(Debug, Clone)]
pub(in super::super) struct ControlledRouteRetirement {
    pub(in super::super) route_identity: SourceRouteIdentity,
    pub(in super::super) expected_identity: [u8; 32],
}

#[derive(Debug, Clone)]
pub struct SourceBackedRoute {
    pub(in super::super) metadata: SourceBackedRouteMetadata,
    /// Exact bounded discovery inputs from which this executable route was
    /// registered. The watch catalog retains these inputs for provider-neutral
    /// exact refresh reconstruction; grouped routes may retain multiple roots.
    pub(in super::super) registration_sources: Vec<ProviderSource>,
    pub(in super::super) driver: Option<SourceBackedRouteDriver>,
    pub(in super::super) certified_missing_paths: Vec<PathBuf>,
    pub(in super::super) retire_after_success: Vec<SourceRouteIdentity>,
    pub(in super::super) automatic_retire_after_success: Vec<SourceRouteIdentity>,
    pub(in super::super) controlled_retire_after_success: Vec<ControlledRouteRetirement>,
    pub(in super::super) codex_generation_participant: Option<usize>,
}

impl SourceBackedRoute {
    pub(in crate::source_backed) fn apply_provider_root_route_identity(
        &mut self,
        source_root_lineage: Option<[u8; 32]>,
    ) -> SourceBackedCoordinatorResult<()> {
        if self.metadata.selection != Some(SourceBackedRouteSelection::ExplicitManual) {
            return Err(invalid_route(
                self.metadata.source.provider,
                "provider-root identity requires an explicit configured route",
            ));
        }
        self.metadata.route_identity = Some(match source_root_lineage {
            None => automatic_source_backed_route_identity(&self.metadata.source)?,
            Some(lineage) => provider_root_source_backed_route_identity(
                &self.metadata.source,
                self.metadata.certified_source_format,
                lineage,
            )?,
        });
        Ok(())
    }

    pub fn automatic(
        source: ProviderSource,
        selector_authority: SourceBackedSelectorAuthority,
        driver: SourceBackedRouteDriver,
    ) -> SourceBackedCoordinatorResult<Self> {
        let known = validate_executable_route(
            &source,
            SourceBackedRouteSelection::Automatic,
            selector_authority,
        )?;
        let route_identity = automatic_source_backed_route_identity(&source)?;
        Ok(Self {
            metadata: SourceBackedRouteMetadata {
                source: source.clone(),
                certified_source_format: known.certified_source_format,
                selection: Some(SourceBackedRouteSelection::Automatic),
                selector_authority,
                unsupported_reason: None,
                route_identity: Some(route_identity),
                watch_target_kind: known.watch_target_kind,
            },
            registration_sources: vec![source],
            driver: Some(driver),
            certified_missing_paths: Vec::new(),
            retire_after_success: Vec::new(),
            automatic_retire_after_success: Vec::new(),
            controlled_retire_after_success: Vec::new(),
            codex_generation_participant: None,
        })
    }

    pub fn explicit_manual(
        source: ProviderSource,
        selector_authority: SourceBackedSelectorAuthority,
        driver: SourceBackedRouteDriver,
    ) -> SourceBackedCoordinatorResult<Self> {
        let known = validate_executable_route(
            &source,
            SourceBackedRouteSelection::ExplicitManual,
            selector_authority,
        )?;
        let route_identity = source_backed_route_identity(
            &source,
            known.certified_source_format,
            SourceBackedRouteSelection::ExplicitManual,
            selector_authority,
        )?;
        Ok(Self {
            metadata: SourceBackedRouteMetadata {
                source: source.clone(),
                certified_source_format: known.certified_source_format,
                selection: Some(SourceBackedRouteSelection::ExplicitManual),
                selector_authority,
                unsupported_reason: None,
                route_identity: Some(route_identity),
                watch_target_kind: known.watch_target_kind,
            },
            registration_sources: vec![source],
            driver: Some(driver),
            certified_missing_paths: Vec::new(),
            retire_after_success: Vec::new(),
            automatic_retire_after_success: Vec::new(),
            controlled_retire_after_success: Vec::new(),
            codex_generation_participant: None,
        })
    }

    pub fn certified_missing(
        source: ProviderSource,
        selector_authority: SourceBackedSelectorAuthority,
    ) -> SourceBackedCoordinatorResult<Self> {
        let known = validate_executable_route(
            &source,
            SourceBackedRouteSelection::Automatic,
            selector_authority,
        )?;
        let route_identity = automatic_source_backed_route_identity(&source)?;
        let path = source.path.clone();
        Ok(Self {
            metadata: SourceBackedRouteMetadata {
                source: source.clone(),
                certified_source_format: known.certified_source_format,
                selection: Some(SourceBackedRouteSelection::Automatic),
                selector_authority,
                unsupported_reason: None,
                route_identity: Some(route_identity),
                watch_target_kind: known.watch_target_kind,
            },
            registration_sources: vec![source],
            driver: None,
            certified_missing_paths: vec![path],
            retire_after_success: Vec::new(),
            automatic_retire_after_success: Vec::new(),
            controlled_retire_after_success: Vec::new(),
            codex_generation_participant: None,
        })
    }

    /// Represents one explicitly configured path that is currently absent.
    /// The path-derived route identity is the same identity the executable
    /// route will use when the path appears.
    pub fn certified_explicit_missing(
        source: ProviderSource,
        selector_authority: SourceBackedSelectorAuthority,
    ) -> SourceBackedCoordinatorResult<Self> {
        let known = validate_executable_route(
            &source,
            SourceBackedRouteSelection::ExplicitManual,
            selector_authority,
        )?;
        let route_identity = source_backed_route_identity(
            &source,
            known.certified_source_format,
            SourceBackedRouteSelection::ExplicitManual,
            selector_authority,
        )?;
        let path = source.path.clone();
        Ok(Self {
            metadata: SourceBackedRouteMetadata {
                source: source.clone(),
                certified_source_format: known.certified_source_format,
                selection: Some(SourceBackedRouteSelection::ExplicitManual),
                selector_authority,
                unsupported_reason: None,
                route_identity: Some(route_identity),
                watch_target_kind: known.watch_target_kind,
            },
            registration_sources: vec![source],
            driver: None,
            certified_missing_paths: vec![path],
            retire_after_success: Vec::new(),
            automatic_retire_after_success: Vec::new(),
            controlled_retire_after_success: Vec::new(),
            codex_generation_participant: None,
        })
    }

    /// Represents one configured physical route whose path could not be
    /// classified safely during discovery. It retains the same path-derived
    /// identity as the executable route so a warm refresh can carry only this
    /// route while healthy peers continue.
    pub fn unavailable_explicit(
        source: ProviderSource,
        reason: impl Into<String>,
    ) -> SourceBackedCoordinatorResult<Self> {
        let known =
            landed_format_route(source.provider, source.source_format).ok_or_else(|| {
                invalid_route(
                    source.provider,
                    "configured unavailable source has no landed route",
                )
            })?;
        if !known.explicit_manual || known.unsupported_reason.is_some() {
            return Err(invalid_route(
                source.provider,
                "configured unavailable source has no explicit route authority",
            ));
        }
        let selector_authority = SourceBackedSelectorAuthority::ExplicitPath;
        let route_identity = source_backed_route_identity(
            &source,
            known.certified_source_format,
            SourceBackedRouteSelection::ExplicitManual,
            selector_authority,
        )?;
        Ok(Self {
            metadata: SourceBackedRouteMetadata {
                source: source.clone(),
                certified_source_format: known.certified_source_format,
                selection: Some(SourceBackedRouteSelection::ExplicitManual),
                selector_authority,
                unsupported_reason: Some(reason.into()),
                route_identity: Some(route_identity),
                watch_target_kind: known.watch_target_kind,
            },
            registration_sources: vec![source],
            driver: None,
            certified_missing_paths: Vec::new(),
            retire_after_success: Vec::new(),
            automatic_retire_after_success: Vec::new(),
            controlled_retire_after_success: Vec::new(),
            codex_generation_participant: None,
        })
    }

    pub fn unsupported(source: ProviderSource, reason: impl Into<String>) -> Self {
        let certified_source_format = landed_format_route(source.provider, source.source_format)
            .map_or(source.source_format, |route| route.certified_source_format);
        Self {
            metadata: SourceBackedRouteMetadata {
                source,
                certified_source_format,
                selection: None,
                selector_authority: SourceBackedSelectorAuthority::ExplicitPath,
                unsupported_reason: Some(reason.into()),
                route_identity: None,
                watch_target_kind: SourceBackedWatchTargetKind::Path,
            },
            registration_sources: Vec::new(),
            driver: None,
            certified_missing_paths: Vec::new(),
            retire_after_success: Vec::new(),
            automatic_retire_after_success: Vec::new(),
            controlled_retire_after_success: Vec::new(),
            codex_generation_participant: None,
        }
    }

    pub fn metadata(&self) -> &SourceBackedRouteMetadata {
        &self.metadata
    }
}

fn provider_root_source_backed_route_identity(
    source: &ProviderSource,
    certified_source_format: &str,
    source_root_lineage: [u8; 32],
) -> SourceBackedCoordinatorResult<SourceRouteIdentity> {
    let route_role = match (
        source.provider,
        source.source_format,
        source.path.file_name().and_then(std::ffi::OsStr::to_str),
    ) {
        (CaptureProvider::Claude, "claude_projects_jsonl_tree", Some("projects")) => {
            "claude-projects"
        }
        (CaptureProvider::Codex, "codex_session_jsonl_tree", Some("sessions")) => "codex-sessions",
        (CaptureProvider::Codex, "codex_session_jsonl_tree", Some("archived_sessions")) => {
            "codex-archived-sessions"
        }
        (CaptureProvider::Codex, "codex_history_jsonl", Some("history.jsonl")) => {
            "codex-prompt-history"
        }
        _ => {
            return Err(invalid_route(
                source.provider,
                "configured provider route has no stable home-relative role",
            ));
        }
    };
    let mut digest = Sha256::new();
    digest.update(b"ctx.provider-root-route-identity.v1\0");
    digest.update(source.provider.as_str().as_bytes());
    digest.update([0]);
    digest.update(certified_source_format.as_bytes());
    digest.update([0]);
    digest.update(source_root_lineage);
    digest.update([0]);
    digest.update(route_role.as_bytes());
    SourceRouteIdentity::from_sha256(format!("{:x}", digest.finalize())).map_err(|_| {
        invalid_route(
            source.provider,
            "provider-root route identity derivation was invalid",
        )
    })
}

impl SourceBackedRegistryRoute for SourceBackedRoute {
    type Metadata = SourceBackedRouteMetadata;

    fn metadata(&self) -> &Self::Metadata {
        &self.metadata
    }

    fn route_identity(&self) -> Option<&SourceRouteIdentity> {
        self.metadata.route_identity.as_ref()
    }

    fn is_executable(&self) -> bool {
        self.driver.is_some()
    }

    fn has_certified_missing_paths(&self) -> bool {
        !self.certified_missing_paths.is_empty()
    }

    fn uses_parallel_leaf_workers(&self) -> bool {
        self.driver
            .as_ref()
            .is_some_and(|driver| driver.uses_parallel_leaf_workers)
    }

    fn absorb_certified_missing_route(&mut self, mut route: Self) {
        self.certified_missing_paths
            .append(&mut route.certified_missing_paths);
        self.certified_missing_paths.sort();
        self.certified_missing_paths.dedup();
    }
}

#[derive(Debug, Clone, Default)]
pub struct SourceBackedProviderRegistry {
    pub(in super::super) routes: SourceBackedRouteRegistry<SourceBackedRoute>,
    pub(in super::super) codex_generation: Option<Arc<CodexGenerationNormalizationCoordinatorV0>>,
    pub(in super::super) applied_provider_roots: Option<(bool, String, Vec<AppliedProviderRoot>)>,
    pub(in super::super) provider_root_route_retirements: BTreeSet<SourceRouteIdentity>,
}

impl SourceBackedProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, route: SourceBackedRoute) {
        self.routes.register(route);
    }

    pub fn set_applied_provider_roots(
        &mut self,
        automatic_provider_discovery: bool,
        config_digest: String,
        roots: Vec<AppliedProviderRoot>,
    ) -> SourceBackedCoordinatorResult<()> {
        if self.applied_provider_roots.is_some() {
            return Err(SourceBackedCoordinatorError::InvalidRoute {
                provider: CaptureProvider::Unknown,
                detail: "applied provider roots were installed more than once".to_owned(),
            });
        }
        self.applied_provider_roots = Some((automatic_provider_discovery, config_digest, roots));
        Ok(())
    }

    pub fn applied_provider_roots(&self) -> Option<&(bool, String, Vec<AppliedProviderRoot>)> {
        self.applied_provider_roots.as_ref()
    }

    pub fn executable_route_identities(&self) -> Vec<SourceRouteIdentity> {
        self.routes
            .iter()
            .filter(|route| route.driver.is_some())
            .filter_map(|route| route.metadata.route_identity.clone())
            .collect()
    }

    /// Records exact routes retired by a validated provider-root config
    /// transition. Unlike discovery absence, removing a configured root is
    /// direct desired-state authority and does not require missing grace.
    pub fn set_provider_root_route_retirements(
        &mut self,
        routes: impl IntoIterator<Item = SourceRouteIdentity>,
    ) {
        self.provider_root_route_retirements = routes.into_iter().collect();
    }

    pub fn provider_root_route_retirements(&self) -> &BTreeSet<SourceRouteIdentity> {
        &self.provider_root_route_retirements
    }

    /// Binds exact carried base routes to an executable replacement route.
    /// Retirement is applied only after that replacement scans and terminally
    /// revalidates successfully; failed replacements retain the base routes.
    pub fn retire_routes_after_success(
        &mut self,
        replacement: &SourceRouteIdentity,
        retired: impl IntoIterator<Item = SourceRouteIdentity>,
    ) -> SourceBackedCoordinatorResult<()> {
        let route = self
            .routes
            .iter_mut()
            .find(|route| route.metadata.route_identity.as_ref() == Some(replacement))
            .ok_or_else(|| SourceBackedCoordinatorError::InvalidRefreshScope {
                route_id: replacement.as_str().to_owned(),
            })?;
        if route.driver.is_none() {
            return Err(SourceBackedCoordinatorError::InvalidRefreshScope {
                route_id: replacement.as_str().to_owned(),
            });
        }
        route.retire_after_success.extend(retired);
        route.retire_after_success.sort();
        route.retire_after_success.dedup();
        if route
            .retire_after_success
            .binary_search(replacement)
            .is_ok()
        {
            return Err(SourceBackedCoordinatorError::InvalidRefreshScope {
                route_id: replacement.as_str().to_owned(),
            });
        }
        Ok(())
    }

    /// Binds bounded automatic replacement candidates to one automatic route.
    ///
    /// The coordinator activates only candidates present in the locked base
    /// during an exhaustive refresh. Exact-scoped refreshes never authorize
    /// these ownership transfers.
    pub fn retire_automatic_routes_after_success(
        &mut self,
        replacement: &SourceRouteIdentity,
        retired: impl IntoIterator<Item = SourceRouteIdentity>,
    ) -> SourceBackedCoordinatorResult<()> {
        let route = self
            .routes
            .iter_mut()
            .find(|route| route.metadata.route_identity.as_ref() == Some(replacement))
            .ok_or_else(|| SourceBackedCoordinatorError::InvalidRefreshScope {
                route_id: replacement.as_str().to_owned(),
            })?;
        if route.metadata.selection != Some(SourceBackedRouteSelection::Automatic)
            || route.driver.is_none()
        {
            return Err(SourceBackedCoordinatorError::InvalidRefreshScope {
                route_id: replacement.as_str().to_owned(),
            });
        }
        route.automatic_retire_after_success.extend(retired);
        route.automatic_retire_after_success.sort();
        route.automatic_retire_after_success.dedup();
        if route
            .automatic_retire_after_success
            .binary_search(replacement)
            .is_ok()
        {
            return Err(SourceBackedCoordinatorError::InvalidRefreshScope {
                route_id: replacement.as_str().to_owned(),
            });
        }
        Ok(())
    }

    /// Registers stale routes as conditional retirement candidates. A
    /// candidate is authorized only when the replacement's successful
    /// provider-owned control reports the expected stable identity.
    pub fn retire_controlled_routes_after_success(
        &mut self,
        replacement: &SourceRouteIdentity,
        retired: impl IntoIterator<Item = (SourceRouteIdentity, [u8; 32])>,
    ) -> SourceBackedCoordinatorResult<()> {
        let route = self
            .routes
            .iter_mut()
            .find(|route| route.metadata.route_identity.as_ref() == Some(replacement))
            .ok_or_else(|| SourceBackedCoordinatorError::InvalidRefreshScope {
                route_id: replacement.as_str().to_owned(),
            })?;
        if route.metadata.selection != Some(SourceBackedRouteSelection::Automatic)
            || !route
                .driver
                .as_ref()
                .and_then(|driver| driver.route_control_expectation.as_ref())
                .is_some_and(SourceBackedRouteControlExpectation::supports_retirement_identity)
        {
            return Err(SourceBackedCoordinatorError::InvalidRefreshScope {
                route_id: replacement.as_str().to_owned(),
            });
        }
        route
            .controlled_retire_after_success
            .extend(
                retired
                    .into_iter()
                    .map(
                        |(route_identity, expected_identity)| ControlledRouteRetirement {
                            route_identity,
                            expected_identity,
                        },
                    ),
            );
        route
            .controlled_retire_after_success
            .sort_by(|left, right| {
                left.route_identity
                    .cmp(&right.route_identity)
                    .then(left.expected_identity.cmp(&right.expected_identity))
            });
        route
            .controlled_retire_after_success
            .dedup_by(|left, right| {
                left.route_identity == right.route_identity
                    && left.expected_identity == right.expected_identity
            });
        if route
            .controlled_retire_after_success
            .iter()
            .any(|candidate| &candidate.route_identity == replacement)
        {
            return Err(SourceBackedCoordinatorError::InvalidRefreshScope {
                route_id: replacement.as_str().to_owned(),
            });
        }
        Ok(())
    }

    pub fn routes(&self) -> impl ExactSizeIterator<Item = &SourceBackedRouteMetadata> {
        self.routes.routes()
    }

    /// Returns the exact discovery roots declared by one executable route
    /// eligible to satisfy explicit catalog coverage. Automatic routes and
    /// configured provider-root routes are eligible; unrelated manual routes
    /// remain independently owned.
    pub fn catalog_coverage_route_registration_sources(
        &self,
        route_identity: &SourceRouteIdentity,
    ) -> Option<impl ExactSizeIterator<Item = &ProviderSource>> {
        let configured = self
            .applied_provider_roots
            .iter()
            .flat_map(|(_, _, roots)| roots)
            .flat_map(|root| root.routes())
            .any(|route| route == route_identity);
        let route = self.routes.iter().find(|route| {
            route.metadata.route_identity.as_ref() == Some(route_identity)
                && (route.metadata.selection == Some(SourceBackedRouteSelection::Automatic)
                    || configured)
                && route.driver.is_some()
                && !route.registration_sources.is_empty()
        })?;
        Some(route.registration_sources.iter())
    }

    pub fn executable_route_count(&self) -> usize {
        self.routes.executable_route_count()
    }

    /// Returns whether any executable route selected by this exact refresh can
    /// consume the source-scanner half of the coordinated CPU budget.
    pub fn selected_routes_use_parallel_leaf_workers(
        &self,
        scope: &SourceBackedRefreshScope,
    ) -> bool {
        self.routes.selected_routes_use_parallel_leaf_workers(scope)
    }

    pub fn unsupported_route_count(&self) -> usize {
        self.routes.unsupported_route_count()
    }
}

impl
    ctx_history_provider_runtime::ProviderRouteRegistrar<
        crate::provider::source_backed::family::CaptureProviderRuntime,
    > for SourceBackedProviderRegistry
{
    type Error = SourceBackedCoordinatorError;

    fn register_provider_route(
        &mut self,
        registration: ctx_history_provider_runtime::ProviderRouteRegistration<
            crate::provider::source_backed::family::CaptureProviderRuntime,
        >,
    ) -> SourceBackedCoordinatorResult<()> {
        self.register(executable_route(
            registration.source,
            registration.selection,
            registration.selector_authority,
            registration.driver,
        )?);
        Ok(())
    }
}

/// Derives the canonical identity for a source's landed automatic route.
///
/// This intentionally accepts sources that failed registration so callers can
/// match route-local failures to a retained healthy route from the same source.
pub fn automatic_source_backed_route_identity(
    source: &ProviderSource,
) -> SourceBackedCoordinatorResult<SourceRouteIdentity> {
    let known = landed_format_route(source.provider, source.source_format)
        .filter(|route| route.automatic)
        .ok_or_else(|| {
            invalid_route(
                source.provider,
                format!(
                    "source format {:?} has no landed automatic route",
                    source.source_format
                ),
            )
        })?;
    source_backed_route_identity(
        source,
        known.certified_source_format,
        SourceBackedRouteSelection::Automatic,
        known.selector_authority,
    )
}

/// Derives the stable source-scoped failure identity used by refresh receipts
/// and direct unsupported-source diagnostics.
pub fn source_backed_source_failure_identity(
    source: &ProviderSource,
) -> SourceBackedCoordinatorResult<String> {
    let certified_source_format = landed_format_route(source.provider, source.source_format)
        .map_or(source.source_format, |route| route.certified_source_format);
    let mut digest = Sha256::new();
    digest.update(b"ctx.source-failure-identity-v1\0");
    digest.update(source.provider.as_str().as_bytes());
    digest.update([0]);
    digest.update(certified_source_format.as_bytes());
    digest.update([0]);
    let path = source.path.as_os_str().as_encoded_bytes();
    digest.update((path.len() as u64).to_be_bytes());
    digest.update(path);
    Ok(format!("{:x}", digest.finalize()))
}

fn source_backed_route_identity(
    source: &ProviderSource,
    certified_source_format: &str,
    selection: SourceBackedRouteSelection,
    selector_authority: SourceBackedSelectorAuthority,
) -> SourceBackedCoordinatorResult<SourceRouteIdentity> {
    let mut digest = Sha256::new();
    digest.update(b"ctx.source-route-identity-v1\0");
    digest.update(source.provider.as_str().as_bytes());
    digest.update([0]);
    digest.update(certified_source_format.as_bytes());
    digest.update([0]);
    digest.update(match selection {
        SourceBackedRouteSelection::Automatic => b"automatic".as_slice(),
        SourceBackedRouteSelection::ExplicitManual => b"explicit".as_slice(),
    });
    digest.update([0]);
    digest.update(match selector_authority {
        SourceBackedSelectorAuthority::DiscoveredWinner => b"discovered-winner".as_slice(),
        SourceBackedSelectorAuthority::ExplicitPath => b"explicit-path".as_slice(),
        SourceBackedSelectorAuthority::CatalogLineage => b"catalog-lineage".as_slice(),
        SourceBackedSelectorAuthority::ExactCwd => b"exact-cwd".as_slice(),
        SourceBackedSelectorAuthority::NamedSurface => b"named-surface".as_slice(),
        SourceBackedSelectorAuthority::SelectedWithRetainedExplicit => {
            b"selected-with-retained-explicit".as_slice()
        }
    });
    // Discovered-winner routes deliberately keep path-independent identity so
    // moving the selected provider root remains an in-place replacement.
    // Catalog-lineage routes instead represent independently owned catalogs;
    // automatic NanoClaw discovery may therefore register several checkouts.
    if selection == SourceBackedRouteSelection::ExplicitManual
        || selector_authority == SourceBackedSelectorAuthority::CatalogLineage
    {
        let path = source.path.as_os_str().as_encoded_bytes();
        digest.update((path.len() as u64).to_be_bytes());
        digest.update(path);
    } else if source.provider == CaptureProvider::Hermes {
        let profile = ctx_history_provider_hermes::hermes_automatic_profile_name(&source.path)
            .map_err(|error| invalid_route(source.provider, error.to_string()))?;
        if profile != "default" {
            // Hermes discovery intentionally multiplexes independently owned
            // named profiles. Keep the historical default route identity, but
            // give every validated named profile a stable path-independent
            // logical slot so registry de-duplication cannot collapse them.
            digest.update(b"\0hermes-profile\0");
            digest.update((profile.len() as u64).to_be_bytes());
            digest.update(profile.as_bytes());
        }
    }
    index_source_route_identity(SourceRouteIdentity::from_sha256(format!(
        "{:x}",
        digest.finalize()
    )))
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_identity_validation_uses_the_canonical_index_conversion() {
        let error = index_source_route_identity(SourceRouteIdentity::from_sha256("AB".repeat(32)))
            .map_err(SourceBackedCoordinatorError::from)
            .unwrap_err();

        assert!(matches!(
            error,
            SourceBackedCoordinatorError::Index(IndexError::InvalidSourceRouteIdentity)
        ));
    }
}
