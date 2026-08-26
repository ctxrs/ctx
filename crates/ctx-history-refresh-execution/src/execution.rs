use super::*;

#[cfg(test)]
mod catalog_refresh_admission_tests;
mod family_fallback;
mod publication;
mod registry_merge;
mod route_local;

use family_fallback::{exact_member_family_fallback_required, ExactMemberFallbackRequired};
pub use publication::exclusive_scan_stage_duration;
use publication::{
    encode_publication_metadata, provider_route_results, publication_from_verified_metadata,
    validate_recertified_metadata, ProviderPublicationFacts,
};
pub(super) use registry_merge::build_merged_source_backed_registry;
use registry_merge::{
    build_merged_source_backed_registry_with_automatic_routes, provider_root_publication_scope,
};
use route_local::refresh_all_provider_sources_route_local_with_reconciliation;
pub(super) struct MergedSourceBackedRegistry {
    pub(super) build: ctx_history_capture::SourceBackedAutomaticRegistryBuild,
    reactivated_automatic_routes: BTreeSet<SourceRouteIdentity>,
    previous_explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
    previous_catalog_route_bindings: Vec<ExplicitSourceCatalogRouteBinding>,
    requested_explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
    pub(super) retained_generation: Option<VerifiedIndex>,
    pub(super) requested_catalog_route_bindings: Vec<ExplicitSourceCatalogRouteBinding>,
    previous_route_controls: BTreeMap<SourceRouteIdentity, Vec<u8>>,
}

enum SourceBackedInventoryDisposition {
    AuthoritativeContent,
    AuthoritativeEmpty(Vec<SourceBackedZeroSourceAuthority>),
    UnsupportedOrUnavailable(ZeroSourcePublicationBlocked),
}

#[derive(Clone)]
struct CatalogRefreshAdmission {
    report: DiscoveryReport,
    discovery_duration: StdDuration,
    exact_members: bool,
}

struct PreopenedPublishedState(Mutex<Option<PublishedSourceBackedState>>);

impl PublishedSourceBackedStatePort for PreopenedPublishedState {
    fn open_published_state(&self, _data_root: &Path) -> Result<PublishedSourceBackedState> {
        self.0
            .lock()
            .map_err(|_| anyhow!("preopened published source state lock was poisoned"))?
            .take()
            .ok_or_else(|| anyhow!("preopened published source state was already consumed"))
    }
}

pub(super) fn execute_capture_owned_refresh(
    execution: SourceBackedRefreshExecution<'_>,
) -> Result<SourceBackedRefreshPublication> {
    let mut family_fallback = execution.clone();
    match execute_capture_owned_refresh_once(execution) {
        Err(error)
            if error
                .downcast_ref::<ExactMemberFallbackRequired>()
                .is_some() =>
        {
            family_fallback.reconciliation_demand = SourceBackedReconciliationDemand::Exhaustive;
            family_fallback
                .admitted_refresh_mut()
                .promote_worksets_to_exhaustive();
            execute_capture_owned_refresh_once(family_fallback)
        }
        result => result,
    }
}

fn execute_capture_owned_refresh_once(
    execution: SourceBackedRefreshExecution<'_>,
) -> Result<SourceBackedRefreshPublication> {
    let discovery_context = execution.discovery_context;
    let reconciliation_demand = execution.reconciliation_demand;
    let attempt_history_progress = execution.attempt_history_progress.clone();
    let route_worksets = execution
        .admitted_refresh()
        .route_worksets()
        .iter()
        .filter_map(|(route, workset)| match workset {
            SourceBackedRefreshWorkset::Members(paths) => Some((route.clone(), paths.clone())),
            SourceBackedRefreshWorkset::Exhaustive => None,
        })
        .collect::<BTreeMap<_, _>>();
    execute_capture_owned_refresh_with(
        execution,
        discovery_context,
        move |discovery,
              report,
              discovery_duration,
              request_id,
              operation,
              data_root,
              index_root,
              explicit_source_catalog,
              scope,
              physical_scope,
              exact_catalog_members,
              published_state,
              report_progress| {
            refresh_all_provider_sources_route_local_with_reconciliation(
                discovery,
                report,
                discovery_duration,
                request_id,
                operation,
                reconciliation_demand,
                exact_catalog_members,
                &route_worksets,
                data_root,
                index_root,
                explicit_source_catalog,
                scope,
                physical_scope,
                published_state,
                attempt_history_progress,
                report_progress,
            )
        },
    )
}

#[doc(hidden)]
pub fn execute_capture_owned_refresh_with<Refresh>(
    execution: SourceBackedRefreshExecution<'_>,
    discovery: &DiscoveryContext,
    refresh_all: Refresh,
) -> Result<SourceBackedRefreshPublication>
where
    Refresh: FnOnce(
        &DiscoveryContext,
        DiscoveryReport,
        StdDuration,
        &str,
        RefreshOperation,
        &Path,
        &Path,
        Option<&ExplicitSourceCatalogAuthority>,
        SourceBackedRefreshScope,
        SourceBackedRefreshScope,
        bool,
        &dyn PublishedSourceBackedStatePort,
        &mut dyn FnMut(CaptureSourceBackedDetailedRefreshProgress) -> SourceBackedRouteResult<()>,
    ) -> Result<SourceBackedRefreshPublication>,
{
    let mut discovery = discovery.clone().with_data_root(execution.data_root);
    let admitted_discovery = execution.admitted_refresh().discovery();
    if let Some(roots) = admitted_discovery.configured_provider_roots() {
        discovery = discovery.with_configured_provider_roots(roots.to_vec());
    }
    if let Some(enabled) = admitted_discovery.automatic_provider_discovery() {
        discovery = discovery.with_automatic_provider_discovery(enabled);
    }
    if source_backed_index_state_exists(execution.index_root)? {
        establish_source_backed_index_privacy(execution.data_root, execution.index_root)?;
    }
    let published_state = execution
        .published_state
        .open_published_state(execution.data_root)?;
    let published_state = PreopenedPublishedState(Mutex::new(Some(published_state)));
    let catalog_admission = catalog_refresh_admission(&execution);
    let report = catalog_admission.report;
    let discovery_duration = catalog_admission.discovery_duration;
    let publication_scope = execution.admitted_refresh().publication_scope();
    let physical_scope =
        SourceBackedRefreshScope::Exact(execution.admitted_refresh().exact_routes().clone());
    validate_provider_source_roots_outside_data_root(execution.data_root, report.sources.iter())
        .context("validate provider roots before source-refresh state writes")?;
    if let Some(authority) = execution.explicit_source_catalog {
        authority
            .validate_source_roots(execution.data_root)
            .context(
                "validate requested explicit provider roots before source-refresh state writes",
            )?;
    }
    establish_source_backed_index_privacy(execution.data_root, execution.index_root)?;
    let mut report_progress = |update: CaptureSourceBackedDetailedRefreshProgress| {
        let progress = update.progress;
        execution
            .report_history_progress_with_total_state(
                progress.phase,
                progress.completed_sources,
                progress.total_sources,
                true,
                progress.current_source,
                progress.completed_records,
                progress.completed_bytes,
                update
                    .current_source_progress
                    .map(SourceBackedCurrentSourceProgress::from_capture),
                progress
                    .providers
                    .into_iter()
                    .map(|provider| provider.as_str().to_owned())
                    .collect(),
                progress.processed_sessions,
                progress.processed_messages,
                progress.processed_tool_calls,
                progress.processed_bytes,
                Some(u64::try_from(progress.elapsed.as_millis()).unwrap_or(u64::MAX)),
                update
                    .exact_scan_progress
                    .map(|exact| SourceBackedExactScanProgress {
                        total_bytes: exact.total_bytes,
                        completed_bytes: exact.completed_bytes,
                    }),
            )
            .map_err(|error| {
                SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    format!("persist daemon source-backed refresh progress: {error:#}"),
                )
            })
    };
    refresh_all(
        &discovery,
        report,
        discovery_duration,
        execution.request_id,
        execution.operation,
        execution.data_root,
        execution.index_root,
        execution.explicit_source_catalog,
        publication_scope,
        physical_scope,
        catalog_admission.exact_members,
        &published_state,
        &mut report_progress,
    )
}

fn source_backed_index_state_exists(index_root: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(index_root) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).context("inspect source-refresh lexical index root"),
    }
}

fn establish_source_backed_index_privacy(data_root: &Path, index_root: &Path) -> Result<()> {
    use ctx_history_platform::platform_security::{
        ensure_private_directory, establish_private_data_root,
    };

    establish_private_data_root(data_root).context("protect source-refresh data root")?;
    let expected_index_root = source_backed_index_root(data_root);
    if index_root == expected_index_root {
        ensure_private_directory(&data_root.join(SEARCH_DIRECTORY))
            .context("protect source-refresh search root")?;
    }
    ensure_private_directory(index_root).context("protect source-refresh lexical index root")?;
    ctx_history_index::ensure_generation_control_state_private(index_root)
        .context("protect source-refresh generation control state")
}

fn catalog_refresh_admission(
    execution: &SourceBackedRefreshExecution<'_>,
) -> CatalogRefreshAdmission {
    let admitted = execution.admitted_refresh();
    let exact_member_report = (execution.reconciliation_demand
        == SourceBackedReconciliationDemand::Incremental)
        .then(|| {
            admitted
                .route_worksets()
                .iter()
                .map(|(route, workset)| match workset {
                    SourceBackedRefreshWorkset::Members(members) => {
                        Some((route.clone(), members.clone()))
                    }
                    SourceBackedRefreshWorkset::Exhaustive => None,
                })
                .collect::<Option<BTreeMap<_, _>>>()
                .and_then(|worksets| {
                    admitted
                        .discovery()
                        .watch_catalog()
                        .exact_member_discovery_report(admitted.exact_routes(), &worksets)
                })
        })
        .flatten();
    CatalogRefreshAdmission {
        report: exact_member_report
            .clone()
            .unwrap_or_else(|| admitted.discovery().report().clone()),
        discovery_duration: admitted.discovery().discovery_duration(),
        exact_members: exact_member_report.is_some(),
    }
}

#[allow(clippy::too_many_arguments)]
#[doc(hidden)]
#[cfg(any(test, feature = "test-support"))]
pub fn refresh_all_provider_sources_route_local(
    discovery: &DiscoveryContext,
    report: DiscoveryReport,
    discovery_duration: StdDuration,
    request_id: &str,
    operation: RefreshOperation,
    data_root: &Path,
    index_root: &Path,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    scope: SourceBackedRefreshScope,
    published_state: &dyn PublishedSourceBackedStatePort,
    report_progress: &mut dyn FnMut(
        CaptureSourceBackedDetailedRefreshProgress,
    ) -> SourceBackedRouteResult<()>,
) -> Result<SourceBackedRefreshPublication> {
    let admitted = admitted_refresh_for_test_execution(
        discovery,
        report.clone(),
        discovery_duration,
        data_root,
        explicit_source_catalog,
        scope,
        BTreeMap::new(),
        published_state,
    )?;
    refresh_all_provider_sources_route_local_with_reconciliation(
        discovery,
        report,
        discovery_duration,
        request_id,
        operation,
        SourceBackedReconciliationDemand::Exhaustive,
        false,
        &BTreeMap::new(),
        data_root,
        index_root,
        explicit_source_catalog,
        admitted.publication_scope(),
        SourceBackedRefreshScope::Exact(admitted.exact_routes().clone()),
        published_state,
        Default::default(),
        report_progress,
    )
}

#[allow(clippy::too_many_arguments)]
#[doc(hidden)]
#[cfg(any(test, feature = "test-support"))]
pub fn refresh_all_provider_sources_route_local_with_worksets(
    discovery: &DiscoveryContext,
    report: DiscoveryReport,
    discovery_duration: StdDuration,
    request_id: &str,
    operation: RefreshOperation,
    reconciliation_demand: SourceBackedReconciliationDemand,
    route_worksets: &BTreeMap<SourceRouteIdentity, BTreeSet<PathBuf>>,
    data_root: &Path,
    index_root: &Path,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    scope: SourceBackedRefreshScope,
    published_state: &dyn PublishedSourceBackedStatePort,
    report_progress: &mut dyn FnMut(
        CaptureSourceBackedDetailedRefreshProgress,
    ) -> SourceBackedRouteResult<()>,
) -> Result<SourceBackedRefreshPublication> {
    let admitted = admitted_refresh_for_test_execution(
        discovery,
        report.clone(),
        discovery_duration,
        data_root,
        explicit_source_catalog,
        scope,
        route_worksets
            .iter()
            .map(|(route, members)| {
                (
                    route.clone(),
                    SourceBackedRefreshWorkset::members(members.iter().cloned()),
                )
            })
            .collect(),
        published_state,
    )?;
    refresh_all_provider_sources_route_local_with_reconciliation(
        discovery,
        report,
        discovery_duration,
        request_id,
        operation,
        reconciliation_demand,
        false,
        route_worksets,
        data_root,
        index_root,
        explicit_source_catalog,
        admitted.publication_scope(),
        SourceBackedRefreshScope::Exact(admitted.exact_routes().clone()),
        published_state,
        Default::default(),
        report_progress,
    )
}

#[cfg(any(test, feature = "test-support"))]
#[allow(clippy::too_many_arguments)]
fn admitted_refresh_for_test_execution(
    discovery: &DiscoveryContext,
    report: DiscoveryReport,
    discovery_duration: StdDuration,
    data_root: &Path,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    scope: SourceBackedRefreshScope,
    route_worksets: BTreeMap<SourceRouteIdentity, SourceBackedRefreshWorkset>,
    published_state: &dyn PublishedSourceBackedStatePort,
) -> Result<AdmittedRefresh> {
    let coverage = match &scope {
        SourceBackedRefreshScope::All => AdmittedRefreshCoverage::CompleteCatalog,
        SourceBackedRefreshScope::Exact(_) => AdmittedRefreshCoverage::SelectedRoutes,
    };
    let admitted = source_backed_admitted_discovery_from_report(
        discovery,
        report,
        discovery_duration,
        data_root,
        coverage,
        explicit_source_catalog,
        published_state,
    )?;
    let admitted = match scope {
        SourceBackedRefreshScope::All => admitted,
        SourceBackedRefreshScope::Exact(routes) => admitted.narrow_to(routes)?,
    };
    admitted.with_execution_facts(route_worksets)
}
