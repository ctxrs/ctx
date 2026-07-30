use super::*;

pub(super) fn execute_source_backed_refresh(
    executor: &dyn SourceBackedRefreshExecutor,
    data_root: &Path,
    request_id: &str,
    coordinator: &SourceBackedRefreshCoordinator,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
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
        &mut dyn FnMut(CaptureSourceBackedRefreshProgress) -> SourceBackedRouteResult<()>,
    ) -> Result<SourceBackedRefreshPublication>,
{
    let discovery = discovery.clone().with_data_root(execution.data_root);
    let discovery_started = StdInstant::now();
    let report = discover_provider_sources_with_context(&discovery);
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
        CaptureSourceBackedRefreshProgress,
    ) -> SourceBackedRouteResult<()>,
) -> Result<SourceBackedRefreshPublication> {
    if let Some(authority) = explicit_source_catalog {
        authority.remove_shadowed_automatic_routes(data_root, &mut report)?;
    } else {
        load_explicit_source_catalog_authority(data_root)?
            .remove_shadowed_automatic_routes(data_root, &mut report)?;
    }
    let mut build =
        build_automatic_source_backed_registry_from_report(discovery, data_root, report);
    build.discovery_duration = discovery_duration;
    if let Some(authority) = explicit_source_catalog {
        authority.register_routes(data_root, index_root, &mut build)?;
    } else {
        register_explicit_source_catalog_routes(data_root, index_root, &mut build)?;
    }
    let retained_generation = open_published_generation(data_root)?;
    let retained_sources = retained_generation
        .as_ref()
        .map(|index| index.manifest().sources.clone())
        .unwrap_or_default();
    reject_blocking_automatic_registry_issues(&build.issues, &retained_sources)?;
    reject_unowned_retained_source_families(&build.registry, &retained_sources)?;
    if build.registry.executable_route_count() == 0 {
        return refresh_without_executable_routes(
            &build.registry,
            index_root,
            retained_generation.as_ref(),
            discovery_duration,
            report_progress,
        );
    }
    let (executor, _issues) = build.into_refresh_executor(WriterOptions::default());
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

fn refresh_without_executable_routes(
    registry: &SourceBackedProviderRegistry,
    index_root: &Path,
    retained_generation: Option<&VerifiedIndex>,
    discovery_duration: StdDuration,
    report_progress: &mut dyn FnMut(
        CaptureSourceBackedRefreshProgress,
    ) -> SourceBackedRouteResult<()>,
) -> Result<SourceBackedRefreshPublication> {
    if let Some(retained) = retained_generation {
        if !retained.manifest().sources.is_empty() || !retained.manifest().removals.is_empty() {
            bail!(
                "{TERMINAL_COVERAGE_ERROR_CODE}: no executable source-backed route can revalidate the retained source or removal authority"
            );
        }
    }

    let commit_started = StdInstant::now();
    let generation = if let Some(retained) = retained_generation {
        retained.generation_id().to_owned()
    } else {
        ctx_history_index::GenerationWriter::open(index_root, WriterOptions::default())?
            .commit(|_| false)?
            .generation_id
    };
    let verified = VerifiedIndex::open(index_root)
        .context("verify empty source-backed generation publication")?;
    if verified.generation_id() != generation
        || !verified.manifest().sources.is_empty()
        || !verified.manifest().removals.is_empty()
    {
        bail!("empty source-backed publication did not verify as an empty generation");
    }
    let commit_duration = commit_started.elapsed();
    report_progress(CaptureSourceBackedRefreshProgress {
        phase: "committed",
        completed_sources: 0,
        total_sources: 0,
        current_source: None,
        stage_duration: commit_duration,
        elapsed: discovery_duration.saturating_add(commit_duration),
        certified_source_count: Some(0),
        certified_source_bytes: Some(0),
    })
    .map_err(|error| anyhow!("report empty source-backed publication progress: {error}"))?;

    let resolver = Arc::new(registry.resolver_registry());
    let source_manifest = SourceManifest::new(generation.clone(), Vec::new(), Vec::new())
        .map_err(|error| anyhow!("build empty Pro source manifest: {}", error.message))?;
    Ok(SourceBackedRefreshPublication {
        generation_id: generation,
        source_manifest: Some(source_manifest),
        resolver: Some(resolver),
        scanned_routes: 0,
        unsupported_routes: registry.unsupported_route_count(),
        certified_source_count: 0,
        certified_source_bytes: 0,
        current: SourceBackedRefreshCurrent::default(),
        timings: SourceBackedRefreshTimings {
            discovery_us: nonzero_duration_micros(discovery_duration),
            scan_stage_us: 1,
            commit_us: nonzero_duration_micros(commit_duration),
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
                SourceBackedAutomaticUnavailableReason::UnsafeRootOverlap { .. } => true,
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
        SourceBackedAutomaticUnavailableReason::UnsafeRootOverlap { detail }
        | SourceBackedAutomaticUnavailableReason::RegistrationRejected { detail } => detail.clone(),
        SourceBackedAutomaticUnavailableReason::UnsupportedFormat { detail }
        | SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable { detail } => {
            (*detail).to_owned()
        }
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
