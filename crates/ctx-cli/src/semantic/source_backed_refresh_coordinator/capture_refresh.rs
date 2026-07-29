use super::*;

pub(super) fn execute_source_backed_refresh(
    executor: &dyn SourceBackedRefreshExecutor,
    data_root: &Path,
    request_id: &str,
    coordinator: &SourceBackedRefreshCoordinator,
) -> Result<SourceBackedRefreshPublication> {
    let index_root = source_backed_index_root(data_root);
    let report_progress = |update: SourceBackedRefreshProgressUpdate| {
        record_source_backed_refresh_progress(
            data_root,
            coordinator,
            request_id,
            &update.phase,
            update.completed_sources,
            update.total_sources,
            update.current_source,
        )
    };
    executor.refresh(SourceBackedRefreshExecution {
        data_root,
        index_root: &index_root,
        request_id,
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
        &Path,
        &Path,
        &mut dyn FnMut(CaptureSourceBackedRefreshProgress) -> SourceBackedRouteResult<()>,
    ) -> Result<SourceBackedRefreshPublication>,
{
    execution.report_progress("discovering", 0, 0, None)?;
    let mut report_progress = |update: CaptureSourceBackedRefreshProgress| {
        execution
            .report_progress(
                update.phase,
                update.completed_sources,
                update.total_sources,
                update.current_source,
            )
            .map_err(|error| {
                SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    format!("persist daemon source-backed refresh progress: {error:#}"),
                )
            })
    };
    refresh_all(
        discovery,
        execution.data_root,
        execution.index_root,
        &mut report_progress,
    )
}

fn refresh_all_provider_sources(
    discovery: &DiscoveryContext,
    data_root: &Path,
    index_root: &Path,
    report_progress: &mut dyn FnMut(
        CaptureSourceBackedRefreshProgress,
    ) -> SourceBackedRouteResult<()>,
) -> Result<SourceBackedRefreshPublication> {
    let mut build = build_automatic_source_backed_registry(discovery);
    register_explicit_source_catalog_routes(data_root, index_root, &mut build)?;
    let (executor, issues) = build.into_refresh_executor(WriterOptions::default());
    let retained_sources = open_published_generation(data_root)?
        .map(|index| index.manifest().sources.clone())
        .unwrap_or_default();
    reject_blocking_automatic_registry_issues(&issues, &retained_sources)?;
    reject_unowned_retained_source_families(executor.registry(), &retained_sources)?;
    let resolver = Arc::new(executor.registry().resolver_registry());
    let receipt = executor
        .refresh(index_root, report_progress)
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
    let removals = receipt
        .removals
        .iter()
        .cloned()
        .map(|removal| SourceRemoval::new(removal.deletion, removal.inventory))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| anyhow!("build certified Pro source removal: {}", error.message))?;
    let source_manifest = SourceManifest::new(
        receipt.commit.generation_id.clone(),
        receipt.sources.clone(),
        removals,
    )
    .map_err(|error| anyhow!("build Pro source manifest: {}", error.message))?;
    Ok(SourceBackedRefreshPublication {
        generation_id: receipt.commit.generation_id,
        source_manifest: Some(source_manifest),
        resolver: Some(resolver),
        scanned_routes: receipt.scanned_routes,
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

fn nonzero_duration_micros(duration: StdDuration) -> u64 {
    u64::try_from(duration.as_micros())
        .unwrap_or(u64::MAX)
        .max(1)
}

fn source_backed_discovery_context() -> Result<DiscoveryContext> {
    let home = identity::home_dir()
        .context("resolve the user home for source-backed provider discovery")?;
    Ok(DiscoveryContext::from_process(home))
}

pub(super) fn reject_blocking_automatic_registry_issues(
    issues: &[SourceBackedAutomaticRegistryIssue],
    retained_sources: &[CertifiedSource],
) -> Result<()> {
    let mut blocker_count = 0usize;
    let mut blocker_details = Vec::new();
    for issue in issues {
        let SourceBackedAutomaticRegistryIssue::Unavailable { source, reason } = issue else {
            continue;
        };
        let retained_provider_family = retained_sources.iter().any(|retained| {
            let retained = retained.observation().source();
            retained.provider() == source.provider.as_str()
                && retained.source_format() == source.source_format
        });
        let blocks_publication = retained_provider_family
            || match reason {
                SourceBackedAutomaticUnavailableReason::SourceStatus(
                    ProviderSourceStatus::Missing | ProviderSourceStatus::Unknown,
                ) => false,
                SourceBackedAutomaticUnavailableReason::SourceStatus(_)
                | SourceBackedAutomaticUnavailableReason::UnsupportedFormat { .. }
                | SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable { .. }
                | SourceBackedAutomaticUnavailableReason::RegistrationRejected { .. } => {
                    source.exists
                }
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
        "{TERMINAL_COVERAGE_ERROR_CODE}: retained source generation has no current resolver-owning route family for {}{omitted}",
        uncovered.join(", ")
    )
}

fn automatic_registry_issue_reason(reason: &SourceBackedAutomaticUnavailableReason) -> String {
    match reason {
        SourceBackedAutomaticUnavailableReason::SourceStatus(status) => {
            format!("source status is {}", status.as_str())
        }
        SourceBackedAutomaticUnavailableReason::UnsupportedFormat { detail }
        | SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable { detail } => {
            (*detail).to_owned()
        }
        SourceBackedAutomaticUnavailableReason::RegistrationRejected { detail } => detail.clone(),
    }
}

#[allow(dead_code)] // Used by the retained resolver's future batch consumer.
pub(super) fn hydration_failure_queues_refresh(kind: HydrationFailureKind) -> bool {
    matches!(
        kind,
        HydrationFailureKind::TemporarilyUnavailable
            | HydrationFailureKind::ConfirmedDeleted
            | HydrationFailureKind::StaleSourceEvidence
            | HydrationFailureKind::StaleRecordEvidence
            | HydrationFailureKind::MissingRecord
    )
}

fn record_source_backed_refresh_progress(
    data_root: &Path,
    coordinator: &SourceBackedRefreshCoordinator,
    request_id: &str,
    phase: &str,
    completed_sources: usize,
    total_sources: usize,
    current_source: Option<String>,
) -> Result<()> {
    if let Some(job) = coordinator.set_progress(
        request_id,
        phase,
        completed_sources,
        total_sources,
        current_source,
    ) {
        write_daemon_job_status(&daemon_source_backed_refresh_job_path(data_root), &job)?;
    }
    Ok(())
}
