use super::*;

pub(super) fn execute_source_backed_refresh(
    executor: &dyn SourceBackedRefreshExecutor,
    data_root: &Path,
    request_id: &str,
    coordinator: &CoreRefreshEngine,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
) -> Result<SourceBackedRefreshPublication> {
    let index_root = source_backed_index_root(data_root);
    let report_progress = |update: SourceBackedRefreshProgressUpdate| {
        record_source_backed_refresh_progress(data_root, coordinator, request_id, update)
    };
    executor.refresh(SourceBackedRefreshExecution {
        data_root,
        index_root: &index_root,
        request_id,
        explicit_source_catalog,
        report_progress: &report_progress,
    })
}

pub(super) fn execute_capture_owned_refresh(
    execution: SourceBackedRefreshExecution<'_>,
) -> Result<SourceBackedRefreshPublication> {
    let discovery = source_backed_discovery_context()?;
    execute_capture_owned_refresh_with(execution, &discovery, refresh_all_provider_sources)
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
    execution.report_progress("discovering", 0, 0, None)?;
    let mut report_progress = |update: CaptureSourceBackedDetailedRefreshProgress| {
        let progress = update.progress;
        execution
            .report_detailed_progress(
                progress.phase,
                progress.completed_sources,
                progress.total_sources,
                progress.current_source,
                update.current_source_progress.map(current_source_progress),
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
        &mut report_progress,
    )
}

pub(super) fn refresh_all_provider_sources(
    discovery: &DiscoveryContext,
    mut report: DiscoveryReport,
    discovery_duration: StdDuration,
    data_root: &Path,
    index_root: &Path,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    report_progress: &mut dyn FnMut(
        CaptureSourceBackedDetailedRefreshProgress,
    ) -> SourceBackedRouteResult<()>,
) -> Result<SourceBackedRefreshPublication> {
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
    let published_explicit_source_catalog = catalog.clone();
    let retained_sources = retained_generation
        .as_ref()
        .map(|index| index.manifest().sources.clone())
        .unwrap_or_default();
    reject_blocking_automatic_registry_issues(&build.issues)?;
    reject_unowned_retained_source_families(&build.registry, &retained_sources)?;
    let (executor, _issues) = build.into_refresh_executor(WriterOptions::default());
    let receipt = executor
        .refresh_with_detailed_progress(index_root, report_progress)
        .context("run capture-owned all-provider source-backed refresh")?;
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
    let source_failures = SourceBackedRefreshSourceFailures {
        failures: receipt
            .source_failures
            .failures()
            .iter()
            .map(|failure| SourceBackedRefreshSourceFailure {
                source_identity: failure.source_identity.clone(),
                provider: failure.provider.as_str().to_owned(),
                class: match failure.class {
                    CaptureSourceBackedSourceFailureClass::Unavailable => {
                        SourceBackedRefreshSourceFailureClass::Unavailable
                    }
                    CaptureSourceBackedSourceFailureClass::SourceChanged => {
                        SourceBackedRefreshSourceFailureClass::SourceChanged
                    }
                    CaptureSourceBackedSourceFailureClass::Unreadable => {
                        SourceBackedRefreshSourceFailureClass::Unreadable
                    }
                    CaptureSourceBackedSourceFailureClass::Incompatible => {
                        SourceBackedRefreshSourceFailureClass::Incompatible
                    }
                },
                carried_forward: failure.carried_forward,
                source_selector: failure.source_selector.clone(),
                detail: failure.detail.clone(),
            })
            .collect(),
        omitted: receipt.source_failures.omitted(),
    };
    validate_source_refresh_results(
        receipt.scanned_routes,
        receipt.successful_routes,
        &source_failures,
        current.source_count,
    )?;
    Ok(SourceBackedRefreshPublication {
        generation_id: receipt.commit.generation_id,
        published_explicit_source_catalog,
        scanned_routes: receipt.scanned_routes,
        successful_routes: receipt.successful_routes,
        source_failures,
        unsupported_routes: receipt.unsupported_routes.len(),
        certified_source_count: receipt.certified_source_count,
        certified_source_bytes: receipt.certified_source_bytes,
        current,
        timings: SourceBackedRefreshTimings {
            discovery_us: nonzero_duration_micros(receipt.discovery_duration),
            scan_stage_us: nonzero_duration_micros(receipt.scan_stage_duration),
            commit_us: nonzero_duration_micros(receipt.commit_duration),
        },
    })
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
    if let Some(job) = coordinator.set_detailed_progress(
        request_id,
        &update.phase,
        update.completed_sources,
        update.total_sources,
        update.current_source,
        update.current_source_progress,
    ) {
        write_daemon_job_status(&daemon_source_backed_refresh_job_path(data_root), &job)?;
    }
    Ok(())
}

fn current_source_progress(
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
