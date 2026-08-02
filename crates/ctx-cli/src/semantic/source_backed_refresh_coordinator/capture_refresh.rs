use super::*;
use sha2::{Digest as _, Sha256};

pub(super) struct SourceBackedRefreshPlan<'a> {
    pub(super) explicit_source_catalog: Option<&'a ExplicitSourceCatalogAuthority>,
    pub(super) scope: SourceBackedRefreshScope,
    pub(super) covered_route_ids: BTreeSet<SourceRouteIdentity>,
}

pub(super) fn execute_source_backed_refresh(
    executor: &dyn SourceBackedRefreshExecutor,
    data_root: &Path,
    request_id: &str,
    coordinator: &CoreRefreshEngine,
    plan: SourceBackedRefreshPlan<'_>,
) -> Result<SourceBackedRefreshPublication> {
    let index_root = source_backed_index_root(data_root);
    let report_progress = |update: SourceBackedRefreshProgressUpdate| {
        record_source_backed_refresh_progress(data_root, coordinator, request_id, update)
    };
    executor.refresh(SourceBackedRefreshExecution {
        data_root,
        index_root: &index_root,
        request_id,
        explicit_source_catalog: plan.explicit_source_catalog,
        scope: plan.scope,
        covered_route_ids: plan.covered_route_ids,
        report_progress: &report_progress,
    })
}

pub(super) fn execute_capture_owned_refresh(
    execution: SourceBackedRefreshExecution<'_>,
) -> Result<SourceBackedRefreshPublication> {
    let discovery = source_backed_discovery_context()?;
    execute_capture_owned_refresh_with(
        execution,
        &discovery,
        move |discovery,
              report,
              discovery_duration,
              data_root,
              index_root,
              explicit_source_catalog,
              scope,
              covered_route_ids,
              report_progress| {
            refresh_all_provider_sources_route_local(
                discovery,
                report,
                discovery_duration,
                data_root,
                index_root,
                explicit_source_catalog,
                scope,
                covered_route_ids,
                report_progress,
            )
        },
    )
}

pub(super) fn execute_capture_owned_refresh_with<Refresh>(
    execution: SourceBackedRefreshExecution<'_>,
    discovery: &DiscoveryContext,
    refresh_all: Refresh,
) -> Result<SourceBackedRefreshPublication>
where
    Refresh: FnOnce(
        &DiscoveryContext,
        DiscoveryReport,
        StdDuration,
        &Path,
        &Path,
        Option<&ExplicitSourceCatalogAuthority>,
        SourceBackedRefreshScope,
        &BTreeSet<SourceRouteIdentity>,
        &mut dyn FnMut(CaptureSourceBackedDetailedRefreshProgress) -> SourceBackedRouteResult<()>,
    ) -> Result<SourceBackedRefreshPublication>,
{
    let discovery = discovery.clone().with_data_root(execution.data_root);
    let work_budget = source_backed_refresh_work_budget(WriterOptions::default().indexer_threads);
    let discovery_started = StdInstant::now();
    let report = discover_provider_sources_with_context_and_work_budget(&discovery, work_budget);
    let discovery_duration = discovery_started.elapsed();
    validate_provider_source_roots_outside_data_root(execution.data_root, report.sources.iter())
        .context("validate provider roots before source-refresh state writes")?;
    if let Some(authority) = execution.explicit_source_catalog {
        authority
            .validate_source_roots(execution.data_root)
            .context(
                "validate requested explicit provider roots before source-refresh state writes",
            )?;
    } else {
        validate_explicit_source_catalog_roots(execution.data_root)
            .context("validate explicit provider roots before source-refresh state writes")?;
    }
    execution.report_progress("discovering", 0, 0, None, None, None)?;
    let mut report_progress = |update: CaptureSourceBackedDetailedRefreshProgress| {
        let progress = update.progress;
        execution
            .report_detailed_progress(
                progress.phase,
                progress.completed_sources,
                progress.total_sources,
                progress.current_source,
                progress.completed_records,
                progress.completed_bytes,
                update
                    .current_source_progress
                    .map(daemon_current_source_progress),
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
        execution.data_root,
        execution.index_root,
        execution.explicit_source_catalog,
        execution.scope.clone(),
        &execution.covered_route_ids,
        &mut report_progress,
    )
}

// This is the capture-provider boundary; keeping its independent authorities
// explicit makes test injection and ownership visible at the call site.
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub(super) fn refresh_all_provider_sources(
    discovery: &DiscoveryContext,
    report: DiscoveryReport,
    discovery_duration: StdDuration,
    data_root: &Path,
    index_root: &Path,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    scope: SourceBackedRefreshScope,
    covered_route_ids: &BTreeSet<SourceRouteIdentity>,
    report_progress: &mut dyn FnMut(
        CaptureSourceBackedDetailedRefreshProgress,
    ) -> SourceBackedRouteResult<()>,
) -> Result<SourceBackedRefreshPublication> {
    refresh_all_provider_sources_route_local(
        discovery,
        report,
        discovery_duration,
        data_root,
        index_root,
        explicit_source_catalog,
        scope,
        covered_route_ids,
        report_progress,
    )
}

#[allow(clippy::too_many_arguments)]
fn refresh_all_provider_sources_route_local(
    discovery: &DiscoveryContext,
    report: DiscoveryReport,
    discovery_duration: StdDuration,
    data_root: &Path,
    index_root: &Path,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    scope: SourceBackedRefreshScope,
    covered_route_ids: &BTreeSet<SourceRouteIdentity>,
    report_progress: &mut dyn FnMut(
        CaptureSourceBackedDetailedRefreshProgress,
    ) -> SourceBackedRouteResult<()>,
) -> Result<SourceBackedRefreshPublication> {
    let (build, published_explicit_source_catalog, retained_generation, catalog_route_bindings) =
        build_merged_source_backed_registry(
            discovery,
            report,
            discovery_duration,
            data_root,
            explicit_source_catalog,
        )?;
    let retained_sources = retained_generation
        .as_ref()
        .map(|index| index.manifest().sources.clone())
        .unwrap_or_default();
    let registry_failures = if matches!(scope, SourceBackedRefreshScope::All) {
        reject_blocking_automatic_registry_issues(&build.issues)?;
        reject_unowned_retained_source_families(&build.registry, &retained_sources)?;
        automatic_registry_route_failures(&build.issues)?
    } else {
        Vec::new()
    };
    let physical_scope = if scope == SourceBackedRefreshScope::All && !covered_route_ids.is_empty()
    {
        let current_route_ids = build
            .registry
            .watch_catalog()
            .route_ids()
            .cloned()
            .collect::<BTreeSet<_>>();
        SourceBackedRefreshScope::exact(current_route_ids.difference(covered_route_ids).cloned())
    } else {
        scope
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
    let (executor, _issues) = build.into_refresh_executor(WriterOptions::default());
    let receipt = executor
        .refresh_scope_with_detailed_progress(index_root, physical_scope, report_progress)
        .context("run capture-owned source-backed refresh")?;
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
    let selected_route_ids = receipt
        .selected_route_ids
        .iter()
        .chain(
            registry_failures
                .iter()
                .map(|failure| &failure.route_identity),
        )
        .map(|identity| identity.as_str().to_owned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut source_failures = receipt.source_failures.clone();
    source_failures.extend(registry_failures.iter().cloned());
    let failed_route_outcomes: Vec<SourceBackedRefreshRouteFailure> = receipt
        .failed_routes
        .iter()
        .map(|failure| SourceBackedRefreshRouteFailure {
            route_identity: failure.route_identity.as_str().to_owned(),
            source_identity: failure.source_identity.clone(),
            provider: failure.provider.as_str().to_owned(),
            class: failure.class.as_str().to_owned(),
            carried_forward: failure.carried_forward,
        })
        .chain(
            registry_failures
                .iter()
                .map(|failure| SourceBackedRefreshRouteFailure {
                    route_identity: failure.route_identity.as_str().to_owned(),
                    source_identity: failure.source_identity.clone(),
                    provider: failure.provider.as_str().to_owned(),
                    class: failure.class.as_str().to_owned(),
                    carried_forward: failure.carried_forward,
                }),
        )
        .collect();
    let failed_by_route = failed_route_outcomes
        .iter()
        .map(|failure| (failure.route_identity.as_str(), failure))
        .collect::<BTreeMap<_, _>>();
    let successful_routes = receipt
        .successful_route_ids
        .iter()
        .map(|identity| identity.as_str())
        .collect::<BTreeSet<_>>();
    let successful_route_changes = receipt
        .successful_route_outcomes
        .iter()
        .map(|outcome| (outcome.route_identity.as_str(), outcome.changed))
        .collect::<BTreeMap<_, _>>();
    if successful_route_changes
        .keys()
        .copied()
        .collect::<BTreeSet<_>>()
        != successful_routes
    {
        bail!("capture-owned source refresh receipt has inconsistent successful route outcomes");
    }
    let catalog_route_outcomes = catalog_route_bindings
        .into_iter()
        .map(|binding| {
            let failure = failed_by_route.get(binding.route_identity.as_str());
            let changed = successful_route_changes
                .get(binding.route_identity.as_str())
                .copied();
            let outcome = if failure.is_some() {
                "failed"
            } else if successful_routes.contains(binding.route_identity.as_str()) {
                "succeeded"
            } else {
                "not_selected"
            }
            .to_owned();
            SourceBackedRefreshCatalogRouteOutcome {
                catalog_lineage: binding.catalog_lineage,
                route_identity: binding.route_identity,
                outcome,
                failure_class: failure.map(|failure| failure.class.clone()),
                changed,
            }
        })
        .collect();
    Ok(SourceBackedRefreshPublication {
        generation_id: receipt.commit.generation_id,
        published_explicit_source_catalog,
        scanned_routes: receipt.scanned_routes,
        unsupported_routes: receipt.unsupported_routes.len(),
        certified_source_count: receipt.certified_source_count,
        certified_source_bytes: receipt.certified_source_bytes,
        current,
        selected_route_ids,
        successful_route_ids: receipt
            .successful_route_ids
            .iter()
            .map(|identity| identity.as_str().to_owned())
            .collect(),
        successful_route_changes: successful_route_changes
            .into_iter()
            .map(|(identity, changed)| (identity.to_owned(), changed))
            .collect(),
        failed_route_outcomes,
        catalog_route_outcomes,
        source_failures: source_failures
            .failures()
            .iter()
            .map(|failure| SourceBackedRefreshSourceFailure {
                route_identity: failure.route_identity.as_str().to_owned(),
                source_identity: failure.source_identity.clone(),
                provider: failure.provider.as_str().to_owned(),
                class: failure.class.as_str().to_owned(),
                carried_forward: failure.carried_forward,
                source_selector: failure.source_selector.clone(),
                detail: failure.detail.clone(),
            })
            .collect(),
        timings: SourceBackedRefreshTimings {
            discovery_us: nonzero_duration_micros(receipt.discovery_duration),
            scan_stage_us: nonzero_duration_micros(receipt.scan_stage_duration),
            commit_us: nonzero_duration_micros(receipt.commit_duration),
        },
    })
}

fn daemon_current_source_progress(
    progress: CaptureSourceBackedCurrentSourceProgress,
) -> SourceBackedCurrentSourceProgress {
    SourceBackedCurrentSourceProgress {
        stage: match progress.stage {
            CaptureSourceBackedCurrentSourceProgressStage::SourceFamilyCopy => {
                SourceBackedCurrentSourceProgressStage::SourceFamilyCopy
            }
            CaptureSourceBackedCurrentSourceProgressStage::OnlineBackup => {
                SourceBackedCurrentSourceProgressStage::OnlineBackup
            }
            CaptureSourceBackedCurrentSourceProgressStage::LogicalFingerprint => {
                SourceBackedCurrentSourceProgressStage::LogicalFingerprint
            }
            CaptureSourceBackedCurrentSourceProgressStage::LogicalScan => {
                SourceBackedCurrentSourceProgressStage::LogicalScan
            }
        },
        snapshot_pages_completed: progress.snapshot_pages_completed,
        snapshot_pages_total: progress.snapshot_pages_total,
        snapshot_bytes_completed: progress.snapshot_bytes_completed,
        snapshot_bytes_total: progress.snapshot_bytes_total,
        logical_rows_scanned: progress.logical_rows_scanned,
        logical_certified_bytes: progress.logical_certified_bytes,
    }
}

pub(super) fn source_backed_watch_catalog(data_root: &Path) -> Result<SourceBackedWatchCatalog> {
    let discovery = source_backed_discovery_context()?.with_data_root(data_root);
    let work_budget = source_backed_refresh_work_budget(WriterOptions::default().indexer_threads);
    let discovery_started = StdInstant::now();
    let report = discover_provider_sources_with_context_and_work_budget(&discovery, work_budget);
    let discovery_duration = discovery_started.elapsed();
    validate_provider_source_roots_outside_data_root(data_root, report.sources.iter())
        .context("validate provider roots before deriving source watch catalog")?;
    validate_explicit_source_catalog_roots(data_root)
        .context("validate explicit provider roots before deriving source watch catalog")?;
    let (build, _, _, _) = build_merged_source_backed_registry(
        &discovery,
        report,
        discovery_duration,
        data_root,
        None,
    )?;
    Ok(build.registry.watch_catalog())
}

fn build_merged_source_backed_registry(
    discovery: &DiscoveryContext,
    mut report: DiscoveryReport,
    discovery_duration: StdDuration,
    data_root: &Path,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
) -> Result<(
    ctx_history_capture::SourceBackedAutomaticRegistryBuild,
    ExplicitSourceCatalogAuthority,
    Option<VerifiedIndex>,
    Vec<crate::commands::import::ExplicitSourceCatalogRouteBinding>,
)> {
    let loaded_catalog;
    let catalog = if let Some(authority) = explicit_source_catalog {
        authority
    } else {
        loaded_catalog = load_explicit_source_catalog_authority(data_root)?;
        &loaded_catalog
    };
    catalog.prepare_discovery_report(data_root, &mut report)?;
    let mut build =
        build_automatic_source_backed_registry_from_report(discovery, data_root, report);
    build.discovery_duration = discovery_duration;
    let retained_generation = open_published_generation(data_root)?;
    let catalog_route_bindings = catalog.register_routes_after_discovery_merge(
        data_root,
        retained_generation.as_ref(),
        &mut build,
    )?;
    Ok((
        build,
        catalog.clone(),
        retained_generation,
        catalog_route_bindings,
    ))
}

fn source_backed_discovery_context() -> Result<DiscoveryContext> {
    let home = identity::home_dir()
        .context("resolve the user home for source-backed provider discovery")?;
    Ok(DiscoveryContext::from_process(home))
}

/// Rejects only registry issues whose unsafe root makes route-local execution
/// incapable of establishing a safe publication boundary.
pub(super) fn reject_blocking_automatic_registry_issues(
    issues: &[SourceBackedAutomaticRegistryIssue],
) -> Result<()> {
    let mut blocker_count = 0usize;
    let mut blocker_details = Vec::new();
    for issue in issues {
        let SourceBackedAutomaticRegistryIssue::Unavailable { source, reason } = issue else {
            continue;
        };
        if !matches!(
            reason,
            SourceBackedAutomaticUnavailableReason::UnsafeRootOverlap { .. }
        ) {
            continue;
        }
        blocker_count = blocker_count.saturating_add(1);
        if blocker_details.len() < SOURCE_REFRESH_BUILD_ISSUE_LIMIT {
            blocker_details.push(format!(
                "{} {}: {}",
                source.provider.as_str(),
                source.path.display(),
                automatic_registry_issue_reason(reason),
            ));
        }
    }
    if blocker_count == 0 {
        return Ok(());
    }
    let omitted = blocker_count.saturating_sub(blocker_details.len());
    let omitted = if omitted == 0 {
        String::new()
    } else {
        format!("; {omitted} additional systemic safety issue(s) omitted")
    };
    Err(anyhow!(
        "{TERMINAL_COVERAGE_ERROR_CODE}: capture automatic registry has {blocker_count} systemic safety issue(s): {}{omitted}",
        blocker_details.join("; ")
    ))
}

pub(super) fn automatic_registry_route_failures(
    issues: &[SourceBackedAutomaticRegistryIssue],
) -> Result<Vec<ctx_history_capture::SourceBackedFailedRoute>> {
    let mut failures = BTreeMap::new();
    for issue in issues {
        let SourceBackedAutomaticRegistryIssue::Unavailable { source, reason } = issue else {
            continue;
        };
        let Some(class) = automatic_registry_issue_failure_class(source, reason) else {
            continue;
        };
        let route_identity = automatic_registry_issue_route_identity(source)?;
        failures.entry(route_identity.clone()).or_insert_with(|| {
            ctx_history_capture::SourceBackedFailedRoute::new(
                route_identity,
                automatic_registry_issue_source_identity(source),
                source.provider,
                class,
                false,
                source.path.display().to_string(),
                automatic_registry_issue_reason(reason),
            )
        });
    }
    Ok(failures.into_values().collect())
}

fn automatic_registry_issue_failure_class(
    source: &ctx_history_capture::ProviderSource,
    reason: &SourceBackedAutomaticUnavailableReason,
) -> Option<SourceBackedSourceFailureClass> {
    match reason {
        SourceBackedAutomaticUnavailableReason::UnsafeRootOverlap { .. }
        | SourceBackedAutomaticUnavailableReason::SourceStatus(
            ProviderSourceStatus::Missing | ProviderSourceStatus::Unknown,
        ) => None,
        SourceBackedAutomaticUnavailableReason::SourceStatus(_) if source.exists => {
            Some(SourceBackedSourceFailureClass::Unavailable)
        }
        SourceBackedAutomaticUnavailableReason::UnsupportedFormat { .. }
        | SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable { .. }
        | SourceBackedAutomaticUnavailableReason::RegistrationRejected { .. }
            if source.exists =>
        {
            Some(SourceBackedSourceFailureClass::Incompatible)
        }
        SourceBackedAutomaticUnavailableReason::SourceStatus(_)
        | SourceBackedAutomaticUnavailableReason::UnsupportedFormat { .. }
        | SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable { .. }
        | SourceBackedAutomaticUnavailableReason::RegistrationRejected { .. } => None,
    }
}

fn automatic_registry_issue_route_identity(
    source: &ctx_history_capture::ProviderSource,
) -> Result<SourceRouteIdentity> {
    SourceRouteIdentity::from_sha256(automatic_registry_issue_identity(
        b"ctx.automatic-registry-failure-route-v1\0",
        source,
    ))
    .map_err(Into::into)
}

fn automatic_registry_issue_source_identity(
    source: &ctx_history_capture::ProviderSource,
) -> String {
    automatic_registry_issue_identity(b"ctx.source-failure-identity-v1\0", source)
}

fn automatic_registry_issue_identity(
    domain: &[u8],
    source: &ctx_history_capture::ProviderSource,
) -> String {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update(source.provider.as_str().as_bytes());
    digest.update([0]);
    digest.update(source.source_format.as_bytes());
    digest.update([0]);
    let path = source.path.as_os_str().as_encoded_bytes();
    digest.update((path.len() as u64).to_be_bytes());
    digest.update(path);
    format!("{:x}", digest.finalize())
}

fn selected_registry_route_count(
    registry: &SourceBackedProviderRegistry,
    scope: &SourceBackedRefreshScope,
) -> usize {
    registry
        .routes()
        .filter(|route| route.selection.is_some())
        .filter(|route| match scope {
            SourceBackedRefreshScope::All => true,
            SourceBackedRefreshScope::Exact(selected) => route
                .route_identity
                .as_ref()
                .is_some_and(|identity| selected.contains(identity)),
        })
        .count()
}

fn reject_unowned_retained_source_families(
    registry: &SourceBackedProviderRegistry,
    retained_sources: &[CertifiedSource],
) -> Result<()> {
    let mut uncovered = retained_sources
        .iter()
        .filter_map(|retained| {
            let source = retained.observation().source();
            let covered = registry.routes().any(|route| {
                route.selection.is_some()
                    && route.source.provider.as_str() == source.provider()
                    && route.certified_source_format == source.source_format()
            });
            (!covered).then(|| format!("{} {}", source.provider(), source.source_format()))
        })
        .collect::<Vec<_>>();
    uncovered.sort();
    uncovered.dedup();
    if uncovered.is_empty() {
        return Ok(());
    }
    let omitted = uncovered
        .len()
        .saturating_sub(SOURCE_REFRESH_BUILD_ISSUE_LIMIT);
    uncovered.truncate(SOURCE_REFRESH_BUILD_ISSUE_LIMIT);
    let omitted = if omitted == 0 {
        String::new()
    } else {
        format!("; {omitted} additional uncovered provider family/families omitted")
    };
    bail!(
        "{TERMINAL_COVERAGE_ERROR_CODE}: retained source generation has no current executable route family for {}{omitted}",
        uncovered.join(", ")
    )
}

fn automatic_registry_issue_reason(reason: &SourceBackedAutomaticUnavailableReason) -> String {
    match reason {
        SourceBackedAutomaticUnavailableReason::SourceStatus(status) => {
            format!("source status is {}", status.as_str())
        }
        SourceBackedAutomaticUnavailableReason::UnsafeRootOverlap { detail }
        | SourceBackedAutomaticUnavailableReason::RegistrationRejected { detail } => detail.clone(),
        SourceBackedAutomaticUnavailableReason::UnsupportedFormat { detail }
        | SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable { detail } => {
            (*detail).to_owned()
        }
    }
}

fn record_source_backed_refresh_progress(
    data_root: &Path,
    coordinator: &CoreRefreshEngine,
    request_id: &str,
    update: SourceBackedRefreshProgressUpdate,
) -> Result<()> {
    if let Some(job) = coordinator.set_progress(request_id, update) {
        write_daemon_job_status(&daemon_source_backed_refresh_job_path(data_root), &job)?;
    }
    Ok(())
}
