use super::*;

#[cfg(test)]
mod catalog_refresh_admission_tests;
mod family_fallback;
mod publication;
mod registry_merge;

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

#[allow(clippy::too_many_arguments)]
fn refresh_all_provider_sources_route_local_with_reconciliation(
    discovery: &DiscoveryContext,
    report: DiscoveryReport,
    discovery_duration: StdDuration,
    request_id: &str,
    operation: RefreshOperation,
    reconciliation_demand: SourceBackedReconciliationDemand,
    exact_catalog_members: bool,
    route_worksets: &BTreeMap<SourceRouteIdentity, BTreeSet<PathBuf>>,
    data_root: &Path,
    index_root: &Path,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    scope: SourceBackedRefreshScope,
    physical_scope: SourceBackedRefreshScope,
    published_state: &dyn PublishedSourceBackedStatePort,
    attempt_history_progress: ctx_history_capture_model::SharedAttemptHistoryProgress,
    report_progress: &mut dyn FnMut(
        CaptureSourceBackedDetailedRefreshProgress,
    ) -> SourceBackedRouteResult<()>,
) -> Result<SourceBackedRefreshPublication> {
    let SourceBackedRefreshScope::Exact(physical_routes) = &physical_scope else {
        bail!("capture-owned physical refresh scope must contain exact admitted routes");
    };
    if let SourceBackedRefreshScope::Exact(publication_routes) = &scope {
        if publication_routes != physical_routes {
            bail!("selected-route publication does not match physical admission");
        }
    }
    let MergedSourceBackedRegistry {
        mut build,
        reactivated_automatic_routes,
        previous_explicit_source_catalog,
        previous_catalog_route_bindings,
        requested_explicit_source_catalog,
        retained_generation,
        requested_catalog_route_bindings,
        previous_route_controls,
    } = build_merged_source_backed_registry_with_automatic_routes(
        discovery,
        report,
        discovery_duration,
        data_root,
        explicit_source_catalog,
        physical_routes,
        published_state,
    )?;
    // A newly reactivated automatic identity has no same-route base state
    // from which an incremental member scan could carry the unvisited source
    // family. Promote only those ownership transitions to exhaustive route
    // work; ordinary watcher appends retain their member worksets.
    let route_worksets = route_worksets
        .iter()
        .filter(|(route, _)| !reactivated_automatic_routes.contains(*route))
        .map(|(route, members)| (route.clone(), members.clone()))
        .collect::<BTreeMap<_, _>>();
    if scope == SourceBackedRefreshScope::All
        && reconciliation_demand == SourceBackedReconciliationDemand::Exhaustive
    {
        register_automatic_hermes_profile_rename_retirements(
            &mut build,
            retained_generation.as_ref(),
            &previous_catalog_route_bindings,
            &previous_route_controls,
        )?;
    }
    let registration_failure_policy = match &scope {
        SourceBackedRefreshScope::All => AutomaticRegistryAdmissionFailurePolicy::SystemicOnly,
        SourceBackedRefreshScope::Exact(routes) => {
            AutomaticRegistryAdmissionFailurePolicy::ExactRoutes(routes)
        }
    };
    if let Some(registration_failures) =
        automatic_registry_admission_failures(&build.issues, registration_failure_policy)?
    {
        return Err(registration_failures.into());
    }
    let registry_route_failures = if matches!(scope, SourceBackedRefreshScope::All) {
        reject_blocking_automatic_registry_issues(&build.issues)?;
        automatic_registry_route_failures(&build.issues, retained_generation.as_ref())?
    } else {
        Vec::new()
    };
    let route_less_blockers =
        automatic_registry_route_less_blockers(&build.issues, &registry_route_failures);
    let registry_failures =
        terminal_registry_route_failures(registry_route_failures, &build.registry, &physical_scope);
    let previous_nonempty_routes = retained_generation
        .as_ref()
        .map(|generation| {
            generation
                .manifest()
                .source_routes()
                .iter()
                .filter(|route| !route.sources().is_empty())
                .map(|route| route.route_identity().clone())
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let expected_selected_route_ids = match &physical_scope {
        SourceBackedRefreshScope::Exact(routes) => routes
            .iter()
            .map(|route| route.as_str().to_owned())
            .chain(
                registry_failures
                    .iter()
                    .map(|failure| failure.route_identity.as_str().to_owned()),
            )
            .collect::<BTreeSet<_>>(),
        SourceBackedRefreshScope::All => {
            bail!("capture-owned physical refresh scope was not bounded to exact routes")
        }
    };
    if retained_generation.is_none()
        && !registry_failures.is_empty()
        && selected_registry_route_count(&build.registry, &physical_scope) == 0
    {
        return Err(SourceBackedCoordinatorError::NoUsableSourceRoutes {
            failed_routes: SourceBackedSourceFailures::from_failures(
                registry_failures.iter().cloned(),
            ),
        }
        .into());
    }
    let previous_generation = retained_generation
        .as_ref()
        .map(|index| index.generation_id().to_owned());
    // Observation certificates are sampled before parsing. Terminal source
    // revalidation may legitimately accept same-file JSONL growth after the
    // scanned prefix, so sampling later could certify bytes absent from this
    // generation and make restart skip them. A pre-scan token is either bound
    // to the captured state or conservatively forces the next warm refresh.
    let admitted_route_observations = admitted_route_observations(&build.registry, &physical_scope);
    let writer_options = if build
        .registry
        .selected_routes_use_parallel_leaf_workers(&physical_scope)
    {
        source_backed_refresh_writer_options()
    } else {
        WriterOptions::default()
    };
    let publication_scope = provider_root_publication_scope(
        &scope,
        &physical_scope,
        &build.registry,
        retained_generation.as_ref(),
    );
    let (executor, _issues) = build.into_refresh_executor(writer_options);
    let executor = executor
        .with_base_route_controls(previous_route_controls.clone())
        .with_attempt_history_progress(attempt_history_progress);
    let eta_execution_eligible = retained_generation.is_none()
        && scope == SourceBackedRefreshScope::All
        && registry_failures.is_empty();
    let mut report_attempt_progress = |mut update: CaptureSourceBackedDetailedRefreshProgress| {
        if !eta_execution_eligible {
            update.exact_scan_progress = None;
        }
        report_progress(update)
    };
    let mut terminal_coverage_error = None;
    let mut reconciliation_required = false;
    let refresh_result = executor
        .refresh_physical_scope_with_detailed_progress_publication_metadata_reconciliation_and_worksets(
            index_root,
            physical_scope,
            publication_scope,
            reconciliation_demand,
            route_worksets.clone(),
            &mut report_attempt_progress,
            |context| {
                run_after_capture_scan_before_metadata_hook();
                let successful_route_outcomes = context.successful_route_outcomes();
                let failed_routes = context.failed_route_outcomes();
                let source_failures = context.source_failures();
                let complete_inventory_route_ids = context
                    .complete_inventory_route_ids()
                    .cloned()
                    .collect::<BTreeSet<_>>();
                if exact_catalog_members
                    && exact_member_family_fallback_required(
                        &route_worksets,
                        &complete_inventory_route_ids,
                        successful_route_outcomes,
                        &failed_routes,
                    )
                {
                    reconciliation_required = true;
                    return Err(IndexError::PublicationMetadata(
                        ExactMemberFallbackRequired.to_string(),
                    ));
                }
                let route_results = provider_route_results(
                    ProviderPublicationFacts {
                        selected_route_ids: &context
                            .selected_route_ids()
                            .cloned()
                            .collect::<Vec<_>>(),
                        successful_route_outcomes,
                        failed_routes: &failed_routes,
                        source_failures: &source_failures,
                        logical_source_failures: context.logical_source_failures(),
                        record_rejections: context.record_rejections(),
                        snapshot: context.snapshot(),
                    },
                    &registry_failures,
                    &expected_selected_route_ids,
                )
                .map_err(|error| IndexError::PublicationMetadata(format!("{error:#}")))?;
                let current = SourceBackedRefreshCurrent::from_sources(
                    context.snapshot().sources(),
                    context.removed_source_count(),
                )
                .map_err(|error| IndexError::PublicationMetadata(format!("{error:#}")))?;
                let (published_explicit_source_catalog, catalog_route_bindings) =
                    reconcile_published_catalog_witness(
                        context.snapshot(),
                        previous_explicit_source_catalog.as_ref(),
                        &previous_catalog_route_bindings,
                        requested_explicit_source_catalog.as_ref(),
                        &requested_catalog_route_bindings,
                        &route_results,
                    )
                    .map_err(|error| IndexError::PublicationMetadata(format!("{error:#}")))?;
                let mut publication = SourceBackedRefreshPublication {
                    generation_id: context.generation_id().to_owned(),
                    published_explicit_source_catalog,
                    unsupported_routes: route_results
                        .iter()
                        .filter(|result| result.outcome.failure_class() == Some("incompatible"))
                        .count(),
                    certified_source_count: current.source_count,
                    certified_source_bytes: current.certified_source_bytes,
                    current,
                    route_results,
                    zero_source_authority: Vec::new(),
                    catalog_route_bindings,
                    timings: SourceBackedRefreshTimings::default(),
                    verified_index: None,
                };
                publication.zero_source_authority = match classify_inventory_disposition(
                    &publication,
                    &complete_inventory_route_ids,
                    &previous_nonempty_routes,
                    &route_less_blockers,
                ) {
                    SourceBackedInventoryDisposition::AuthoritativeContent => Vec::new(),
                    SourceBackedInventoryDisposition::AuthoritativeEmpty(authority) => authority,
                    SourceBackedInventoryDisposition::UnsupportedOrUnavailable(error) => {
                        let detail = error.to_string();
                        terminal_coverage_error = Some(error);
                        return Err(IndexError::PublicationMetadata(detail));
                    }
                };
                let route_observations = successful_route_outcomes
                    .iter()
                    .filter(|outcome| outcome.logical_source_failure_total == 0)
                    .filter(|outcome| {
                        context
                            .snapshot()
                            .source_route(&outcome.route_identity)
                            .is_some_and(|route| !route.is_missing())
                    })
                    .filter_map(|outcome| {
                        admitted_route_observations
                            .get(&outcome.route_identity)
                            .cloned()
                            .map(|observation| (outcome.route_identity.clone(), observation))
                    })
                    .collect();
                encode_publication_metadata(
                    request_id,
                    operation,
                    &scope,
                    previous_generation.as_deref(),
                    &publication,
                    route_observations,
                    context.route_controls().clone(),
                )
                .map_err(|error| IndexError::PublicationMetadata(format!("{error:#}")))
            },
        );
    let mut receipt = match refresh_result {
        Ok(receipt) => receipt,
        Err(error) => {
            if reconciliation_required {
                return Err(ExactMemberFallbackRequired.into());
            }
            if let Some(error) = terminal_coverage_error {
                return Err(error.into());
            }
            return Err(error).context("run capture-owned source-backed refresh");
        }
    };
    let unsupported_routes = receipt.unsupported_routes.len();
    let (disposition, verified_index) = receipt.take_verified_publication().ok_or_else(|| {
        anyhow!("capture-owned metadata publication returned no exact verified generation")
    })?;
    let timings = SourceBackedRefreshTimings {
        discovery_us: nonzero_duration_micros(receipt.discovery_duration),
        scan_stage_us: nonzero_duration_micros(exclusive_scan_stage_duration(
            receipt.scan_stage_duration,
            receipt.commit_duration,
        )),
        commit_us: nonzero_duration_micros(receipt.commit_duration),
    };
    if disposition == CapturePublicationDisposition::Published {
        let verified_index = Arc::new(verified_index.into_inner().into_verified_index());
        let mut publication = publication_from_verified_metadata(
            request_id,
            operation,
            &scope,
            timings,
            verified_index,
        )?;
        publication.unsupported_routes = unsupported_routes;
        return Ok(publication);
    }
    let current =
        SourceBackedRefreshCurrent::from_sources(&receipt.sources, receipt.removals.len())?;
    if current.source_count != receipt.certified_source_count
        || current.certified_source_bytes != receipt.certified_source_bytes
        || current.indexed_documents != receipt.commit.indexed_documents
    {
        bail!(
            "capture-owned source refresh receipt does not match its retained generation cardinalities"
        );
    }
    let route_results = provider_route_results(
        ProviderPublicationFacts {
            selected_route_ids: &receipt.selected_route_ids,
            successful_route_outcomes: &receipt.successful_route_outcomes,
            failed_routes: &receipt.failed_routes,
            source_failures: &receipt.source_failures,
            logical_source_failures: &receipt.logical_source_failures,
            record_rejections: &receipt.record_rejections,
            snapshot: receipt.commit.snapshot(),
        },
        &registry_failures,
        &expected_selected_route_ids,
    )?;
    let (published_explicit_source_catalog, catalog_route_bindings) =
        reconcile_published_catalog_witness(
            receipt.commit.snapshot(),
            previous_explicit_source_catalog.as_ref(),
            &previous_catalog_route_bindings,
            requested_explicit_source_catalog.as_ref(),
            &requested_catalog_route_bindings,
            &route_results,
        )?;
    let generation_id = std::mem::take(&mut receipt.commit.generation_id);
    let mut publication = SourceBackedRefreshPublication {
        generation_id,
        published_explicit_source_catalog,
        unsupported_routes,
        certified_source_count: receipt.certified_source_count,
        certified_source_bytes: receipt.certified_source_bytes,
        current,
        route_results,
        zero_source_authority: Vec::new(),
        catalog_route_bindings,
        timings,
        verified_index: Some(Arc::new(verified_index.into_inner().into_verified_index())),
    };
    publication.zero_source_authority = match classify_inventory_disposition(
        &publication,
        &receipt
            .complete_inventory_route_ids
            .iter()
            .cloned()
            .collect(),
        &previous_nonempty_routes,
        &route_less_blockers,
    ) {
        SourceBackedInventoryDisposition::AuthoritativeContent => Vec::new(),
        SourceBackedInventoryDisposition::AuthoritativeEmpty(authority) => authority,
        SourceBackedInventoryDisposition::UnsupportedOrUnavailable(error) => {
            return Err(error.into())
        }
    };
    let verified_index = publication
        .verified_index
        .as_ref()
        .ok_or_else(|| anyhow!("reused Core refresh publication lost its exact verified pin"))?;
    let route_control_changed = receipt.route_controls != previous_route_controls;
    if route_control_changed
        || (publication.current.source_count == 0
            && !verify_generation_query_readiness(verified_index)
                .context("decode Core source-refresh publication authority")?
                .is_ready())
    {
        let route_observations = receipt
            .successful_route_outcomes
            .iter()
            .filter(|outcome| outcome.logical_source_failure_total == 0)
            .filter(|outcome| {
                receipt
                    .commit
                    .snapshot()
                    .source_route(&outcome.route_identity)
                    .is_some_and(|route| !route.is_missing())
            })
            .filter_map(|outcome| {
                admitted_route_observations
                    .get(&outcome.route_identity)
                    .cloned()
                    .map(|observation| (outcome.route_identity.clone(), observation))
            })
            .collect();
        let metadata = encode_publication_metadata(
            request_id,
            operation,
            &scope,
            previous_generation.as_deref(),
            &publication,
            route_observations,
            receipt.route_controls.clone(),
        )?;
        let writer = GenerationWriter::open(index_root, WriterOptions::default())?
            .into_writer()
            .map_err(committed_generation_recovery_error)?;
        let recertified = Arc::new(
            writer.republish_current_publication_metadata(&publication.generation_id, metadata)?,
        );
        validate_recertified_metadata(request_id, operation, &scope, &recertified)?;
        publication.verified_index = Some(recertified);
    }
    Ok(publication)
}

fn register_automatic_hermes_profile_rename_retirements(
    build: &mut ctx_history_capture::SourceBackedAutomaticRegistryBuild,
    retained_generation: Option<&VerifiedIndex>,
    previous_catalog_route_bindings: &[ExplicitSourceCatalogRouteBinding],
    previous_route_controls: &BTreeMap<SourceRouteIdentity, Vec<u8>>,
) -> Result<()> {
    let Some(retained_generation) = retained_generation else {
        return Ok(());
    };
    let explicit_route_ids = previous_catalog_route_bindings
        .iter()
        .map(|binding| binding.route_identity.as_str())
        .collect::<BTreeSet<_>>();
    let current_automatic_hermes = build
        .registry
        .routes()
        .filter(|route| {
            route.source.provider == CaptureProvider::Hermes
                && route.selection == Some(SourceBackedRouteSelection::Automatic)
                && route.selector_authority == SourceBackedSelectorAuthority::DiscoveredWinner
        })
        .filter_map(|route| route.route_identity.clone())
        .collect::<BTreeSet<_>>();
    let stale = previous_route_controls
        .iter()
        .filter(|(route, _)| {
            !current_automatic_hermes.contains(*route)
                && !explicit_route_ids.contains(route.as_str())
                && retained_generation.manifest().source_route(route).is_some()
        })
        .filter_map(|(route, control)| {
            ctx_history_capture::hermes_route_control_database_identity(control)
                .map(|database_identity| (route.clone(), database_identity))
        })
        .collect::<Vec<_>>();
    if stale.is_empty() {
        return Ok(());
    }
    for replacement in current_automatic_hermes {
        build
            .registry
            .retire_controlled_routes_after_success(&replacement, stale.clone())?;
    }
    Ok(())
}

fn classify_inventory_disposition(
    publication: &SourceBackedRefreshPublication,
    complete_inventory_routes: &BTreeSet<SourceRouteIdentity>,
    previous_nonempty_routes: &BTreeSet<SourceRouteIdentity>,
    route_less_blockers: &RouteLessRegistryBlockers,
) -> SourceBackedInventoryDisposition {
    if route_less_blockers.total != 0 && publication.route_results.is_empty() {
        return SourceBackedInventoryDisposition::UnsupportedOrUnavailable(
            route_less_blockers.publication_error(),
        );
    }
    if publication.current.source_count != 0 {
        return SourceBackedInventoryDisposition::AuthoritativeContent;
    }
    if route_less_blockers.total != 0 {
        return SourceBackedInventoryDisposition::UnsupportedOrUnavailable(
            route_less_blockers.publication_error(),
        );
    }
    if publication.route_results.is_empty() {
        if complete_inventory_routes.is_empty() && previous_nonempty_routes.is_empty() {
            return SourceBackedInventoryDisposition::AuthoritativeEmpty(Vec::new());
        }
        return SourceBackedInventoryDisposition::UnsupportedOrUnavailable(
            ZeroSourcePublicationBlocked::new(
                "zero-source publication has no terminal route authority for retained or discovered routes",
            ),
        );
    }
    let covered = publication
        .zero_source_authority
        .iter()
        .map(|authority| (authority.route_identity.clone(), authority.kind))
        .collect::<BTreeMap<_, _>>();
    let mut authority = Vec::with_capacity(publication.route_results.len());
    for result in &publication.route_results {
        if !result.outcome.is_success() {
            let source_detail = result
                .source_failures
                .first()
                .map(|failure| format!(": {}", failure.detail))
                .unwrap_or_default();
            return SourceBackedInventoryDisposition::UnsupportedOrUnavailable(
                ZeroSourcePublicationBlocked::new(format!(
                    "zero-source publication route {} did not complete authoritatively{}",
                    result.route_identity, source_detail,
                )),
            );
        }
        let Ok(route_identity) = SourceRouteIdentity::from_sha256(result.route_identity.clone())
        else {
            return SourceBackedInventoryDisposition::UnsupportedOrUnavailable(
                ZeroSourcePublicationBlocked::new(
                    "zero-source publication contains an invalid route identity",
                ),
            );
        };
        let kind = covered
            .get(&route_identity)
            .copied()
            .or_else(|| {
                previous_nonempty_routes
                    .contains(&route_identity)
                    .then_some(SourceBackedZeroSourceAuthorityKind::ConfirmedDeletion)
            })
            .or_else(|| {
                complete_inventory_routes
                    .contains(&route_identity)
                    .then_some(SourceBackedZeroSourceAuthorityKind::CompleteEmptyInventory)
            });
        let Some(kind) = kind else {
            return SourceBackedInventoryDisposition::UnsupportedOrUnavailable(
                ZeroSourcePublicationBlocked::new(format!(
                    "zero-source publication route {} has neither a complete empty inventory nor confirmed deletion",
                    route_identity.as_str(),
                )),
            );
        };
        authority.push(SourceBackedZeroSourceAuthority {
            generation_id: publication.generation_id.clone(),
            route_identity,
            kind,
        });
    }
    authority.sort_by(|left, right| left.route_identity.cmp(&right.route_identity));
    SourceBackedInventoryDisposition::AuthoritativeEmpty(authority)
}
