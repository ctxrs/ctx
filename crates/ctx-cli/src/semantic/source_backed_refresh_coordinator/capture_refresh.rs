use super::*;

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
        &mut dyn FnMut(CaptureSourceBackedRefreshProgress) -> SourceBackedRouteResult<()>,
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
    let mut report_progress = |update: CaptureSourceBackedRefreshProgress| {
        execution
            .report_progress(
                update.phase,
                update.completed_sources,
                update.total_sources,
                update.current_source,
                update.completed_records,
                update.completed_bytes,
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
        CaptureSourceBackedRefreshProgress,
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
        CaptureSourceBackedRefreshProgress,
    ) -> SourceBackedRouteResult<()>,
) -> Result<SourceBackedRefreshPublication> {
    let (build, published_explicit_source_catalog, retained_generation) =
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
    if matches!(scope, SourceBackedRefreshScope::All) {
        reject_blocking_automatic_registry_issues(&build.issues)?;
        reject_unowned_retained_source_families(&build.registry, &retained_sources)?;
    }
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
    let (executor, _issues) = build.into_refresh_executor(WriterOptions::default());
    let receipt = executor
        .refresh_scope(index_root, physical_scope, report_progress)
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
    Ok(SourceBackedRefreshPublication {
        generation_id: receipt.commit.generation_id,
        published_explicit_source_catalog,
        scanned_routes: receipt.scanned_routes,
        unsupported_routes: receipt.unsupported_routes.len(),
        certified_source_count: receipt.certified_source_count,
        certified_source_bytes: receipt.certified_source_bytes,
        current,
        selected_route_ids: receipt
            .selected_route_ids
            .iter()
            .map(|identity| identity.as_str().to_owned())
            .collect(),
        successful_route_ids: receipt
            .successful_route_ids
            .iter()
            .map(|identity| identity.as_str().to_owned())
            .collect(),
        source_failures: receipt
            .failed_routes
            .iter()
            .map(|failure| SourceBackedRefreshSourceFailure {
                route_identity: failure.route_identity.as_str().to_owned(),
                source_identity: failure.source_identity.clone(),
                provider: failure.provider.as_str().to_owned(),
                class: failure.class.as_str().to_owned(),
                carried_forward: failure.carried_forward,
            })
            .collect(),
        timings: SourceBackedRefreshTimings {
            discovery_us: nonzero_duration_micros(receipt.discovery_duration),
            scan_stage_us: nonzero_duration_micros(receipt.scan_stage_duration),
            commit_us: nonzero_duration_micros(receipt.commit_duration),
        },
    })
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
    let (build, _, _) = build_merged_source_backed_registry(
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
    catalog.register_routes_after_discovery_merge(
        data_root,
        retained_generation.as_ref(),
        &mut build,
    )?;
    Ok((build, catalog.clone(), retained_generation))
}

fn source_backed_discovery_context() -> Result<DiscoveryContext> {
    let home = identity::home_dir()
        .context("resolve the user home for source-backed provider discovery")?;
    Ok(DiscoveryContext::from_process(home))
}

pub(super) fn reject_blocking_automatic_registry_issues(
    issues: &[SourceBackedAutomaticRegistryIssue],
) -> Result<()> {
    // Missing automatic roots are not evidence that another explicit route for
    // the same provider family is unavailable. Exact retained-source coverage
    // is enforced below by the registered routes and again by publication
    // recertification, so a genuinely unowned retained source still fails
    // closed without rejecting a distinct explicit root at this coarse layer.
    let mut blocker_count = 0usize;
    let mut blocker_details = Vec::new();
    for issue in issues {
        let SourceBackedAutomaticRegistryIssue::Unavailable { source, reason } = issue else {
            continue;
        };
        let blocks_publication = match reason {
            SourceBackedAutomaticUnavailableReason::SourceStatus(
                ProviderSourceStatus::Missing | ProviderSourceStatus::Unknown,
            ) => false,
            SourceBackedAutomaticUnavailableReason::UnsafeRootOverlap { .. } => true,
            SourceBackedAutomaticUnavailableReason::SourceStatus(_)
            | SourceBackedAutomaticUnavailableReason::UnsupportedFormat { .. }
            | SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable { .. }
            | SourceBackedAutomaticUnavailableReason::RegistrationRejected { .. } => source.exists,
        };
        if !blocks_publication {
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
        format!("; {omitted} additional blocking issue(s) omitted")
    };
    Err(anyhow!(
        "{TERMINAL_COVERAGE_ERROR_CODE}: capture automatic registry has {blocker_count} blocking detected or retained-provider issue(s): {}{omitted}",
        blocker_details.join("; ")
    ))
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
