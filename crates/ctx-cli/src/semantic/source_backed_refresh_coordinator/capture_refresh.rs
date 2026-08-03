use super::*;
use sha2::{Digest as _, Sha256};
pub(super) struct SourceBackedRefreshPlan<'a> {
    pub(super) explicit_source_catalog: Option<&'a ExplicitSourceCatalogAuthority>,
    pub(super) operation: SourceBackedRefreshOperation,
    pub(super) scope: SourceBackedRefreshScope,
    pub(super) covered_route_ids: BTreeSet<SourceRouteIdentity>,
}

struct MergedSourceBackedRegistry {
    build: ctx_history_capture::SourceBackedAutomaticRegistryBuild,
    published_explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
    retained_generation: Option<VerifiedIndex>,
    catalog_route_bindings: Vec<crate::commands::import::ExplicitSourceCatalogRouteBinding>,
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
        operation: plan.operation,
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
              request_id,
              operation,
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
                request_id,
                operation,
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
        &str,
        SourceBackedRefreshOperation,
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
        execution.request_id,
        execution.operation,
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
        "test-refresh",
        SourceBackedRefreshOperation::Refresh,
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
    request_id: &str,
    operation: SourceBackedRefreshOperation,
    data_root: &Path,
    index_root: &Path,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    scope: SourceBackedRefreshScope,
    covered_route_ids: &BTreeSet<SourceRouteIdentity>,
    report_progress: &mut dyn FnMut(
        CaptureSourceBackedDetailedRefreshProgress,
    ) -> SourceBackedRouteResult<()>,
) -> Result<SourceBackedRefreshPublication> {
    let MergedSourceBackedRegistry {
        build,
        published_explicit_source_catalog,
        retained_generation,
        catalog_route_bindings,
    } = build_merged_source_backed_registry(
        discovery,
        report,
        discovery_duration,
        data_root,
        explicit_source_catalog,
    )?;
    let registry_failures = if matches!(scope, SourceBackedRefreshScope::All) {
        reject_blocking_automatic_registry_issues(&build.issues)?;
        automatic_registry_route_failures(&build.issues, retained_generation.as_ref())?
    } else {
        Vec::new()
    };
    // `All` is a logical request over every route in this request's registry.
    // Express it to Core as an exact set so routes committed by an earlier
    // request-scoped explicit overlay are carried as read authority instead of
    // becoming automatic roots or accidental deletion decisions.
    let physical_scope = if scope == SourceBackedRefreshScope::All {
        let current_route_ids = build
            .registry
            .watch_catalog()
            .route_ids()
            .cloned()
            .collect::<BTreeSet<_>>();
        SourceBackedRefreshScope::exact(current_route_ids.difference(covered_route_ids).cloned())
    } else {
        scope.clone()
    };
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
    let watch_catalog = build.registry.watch_catalog();
    let previous_generation = retained_generation
        .as_ref()
        .map(|index| index.generation_id().to_owned());
    let (executor, _issues) = build.into_refresh_executor(WriterOptions::default());
    let mut receipt = executor
        .refresh_scope_with_detailed_progress_and_publication_metadata(
            index_root,
            physical_scope,
            report_progress,
            |context| {
                let successful_route_outcomes = context.successful_route_outcomes();
                let failed_routes = context.failed_route_outcomes();
                let source_failures = context.source_failures();
                let route_results = provider_route_results(
                    ProviderPublicationFacts {
                        selected_route_ids: &context
                            .selected_route_ids()
                            .cloned()
                            .collect::<Vec<_>>(),
                        successful_route_outcomes: &successful_route_outcomes,
                        failed_routes: &failed_routes,
                        source_failures: &source_failures,
                        logical_source_failures: context.logical_source_failures(),
                        record_rejections: context.record_rejections(),
                        manifest: context.manifest(),
                    },
                    &registry_failures,
                    &expected_selected_route_ids,
                )
                .map_err(|error| IndexError::PublicationMetadata(format!("{error:#}")))?;
                let current = SourceBackedRefreshCurrent::from_sources(
                    &context.manifest().sources,
                    context.removed_source_count(),
                )
                .map_err(|error| IndexError::PublicationMetadata(format!("{error:#}")))?;
                let publication = SourceBackedRefreshPublication {
                    generation_id: context.generation_id().to_owned(),
                    published_explicit_source_catalog: published_explicit_source_catalog.clone(),
                    unsupported_routes: route_results
                        .iter()
                        .filter(|result| result.outcome.failure_class() == Some("incompatible"))
                        .count(),
                    certified_source_count: current.source_count,
                    certified_source_bytes: current.certified_source_bytes,
                    current,
                    route_results,
                    catalog_route_bindings: catalog_route_bindings.clone(),
                    timings: SourceBackedRefreshTimings::default(),
                    verified_index: None,
                };
                let terminal = SourceBackedRefreshReceipt::from_verified_publication(
                    previous_generation.clone(),
                    context.generation_id().to_owned(),
                    &publication,
                )
                .map_err(|error| IndexError::PublicationMetadata(format!("{error:#}")))?;
                let route_observations = successful_route_outcomes
                    .iter()
                    .filter(|outcome| outcome.logical_source_failure_total == 0)
                    .filter(|outcome| {
                        context
                            .manifest()
                            .source_route(&outcome.route_identity)
                            .is_some_and(|route| route.missing_state().is_none())
                    })
                    .filter_map(|outcome| {
                        watch_catalog
                            .certify_route_observation(&outcome.route_identity)
                            .map(|observation| (outcome.route_identity.clone(), observation))
                    })
                    .collect();
                SourceBackedPublicationMetadata {
                    request_id: request_id.to_owned(),
                    operation,
                    refresh_scope: scope.clone(),
                    receipt: terminal.to_json(),
                    route_observations,
                }
                .encode()
            },
        )
        .context("run capture-owned source-backed refresh")?;
    let (disposition, verified_index) = receipt.take_verified_publication().ok_or_else(|| {
        anyhow!("capture-owned metadata publication returned no exact verified generation")
    })?;
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
            manifest: receipt.commit.manifest(),
        },
        &registry_failures,
        &expected_selected_route_ids,
    )?;
    let selected_route_ids = route_results
        .iter()
        .map(|result| result.route_identity.clone())
        .collect::<BTreeSet<_>>();
    if catalog_route_bindings
        .iter()
        .any(|binding| !selected_route_ids.contains(&binding.route_identity))
    {
        bail!("explicit catalog lineage has no selected terminal route result");
    }
    let mut publication = SourceBackedRefreshPublication {
        generation_id: receipt.commit.generation_id,
        published_explicit_source_catalog,
        unsupported_routes: receipt.unsupported_routes.len(),
        certified_source_count: receipt.certified_source_count,
        certified_source_bytes: receipt.certified_source_bytes,
        current,
        route_results,
        catalog_route_bindings,
        timings: SourceBackedRefreshTimings {
            discovery_us: nonzero_duration_micros(receipt.discovery_duration),
            scan_stage_us: nonzero_duration_micros(receipt.scan_stage_duration),
            commit_us: nonzero_duration_micros(receipt.commit_duration),
        },
        verified_index: Some(Arc::new(verified_index)),
    };
    if disposition == PublicationDisposition::Published {
        let unsupported_routes = publication.unsupported_routes;
        let metadata = SourceBackedPublicationMetadata::decode(
            publication
                .verified_index
                .as_deref()
                .ok_or_else(|| anyhow!("published Core generation lost its exact verified pin"))?,
        )?;
        if metadata.request_id != request_id
            || metadata.operation != operation
            || metadata.refresh_scope != scope
        {
            bail!("published Core source-refresh metadata does not match its exact request");
        }
        let durable = published_refresh_receipt_for_index(
            &metadata.response_value(),
            publication
                .verified_index
                .as_deref()
                .ok_or_else(|| anyhow!("published Core generation lost its exact verified pin"))?,
        )?;
        publication = publication_from_terminal_receipt(
            durable,
            publication.timings,
            publication.verified_index.take(),
        );
        publication.unsupported_routes = unsupported_routes;
    }
    Ok(publication)
}

fn publication_from_terminal_receipt(
    receipt: SourceBackedRefreshReceipt,
    timings: SourceBackedRefreshTimings,
    verified_index: Option<Arc<VerifiedIndex>>,
) -> SourceBackedRefreshPublication {
    let unsupported_routes = receipt
        .route_results
        .iter()
        .filter(|result| result.outcome.failure_class() == Some("incompatible"))
        .count();
    SourceBackedRefreshPublication {
        generation_id: receipt.published_generation,
        published_explicit_source_catalog: receipt.published_explicit_source_catalog,
        unsupported_routes,
        certified_source_count: receipt.current.source_count,
        certified_source_bytes: receipt.current.certified_source_bytes,
        current: receipt.current,
        route_results: receipt.route_results,
        catalog_route_bindings: receipt.catalog_route_bindings,
        timings,
        verified_index,
    }
}

struct ProviderPublicationFacts<'a> {
    selected_route_ids: &'a [SourceRouteIdentity],
    successful_route_outcomes: &'a [SourceBackedSuccessfulRouteOutcome],
    failed_routes: &'a [SourceBackedFailedRouteOutcome],
    source_failures: &'a SourceBackedSourceFailures,
    logical_source_failures: &'a SourceBackedLogicalSourceFailures,
    record_rejections: &'a SourceBackedRecordRejections,
    manifest: &'a GenerationManifest,
}

fn provider_route_results(
    facts: ProviderPublicationFacts<'_>,
    registry_failures: &[SourceBackedFailedRoute],
    expected_selected_route_ids: &BTreeSet<String>,
) -> Result<Vec<SourceBackedRefreshRouteResult>> {
    let selected_route_ids = facts
        .selected_route_ids
        .iter()
        .chain(
            registry_failures
                .iter()
                .map(|failure| &failure.route_identity),
        )
        .map(|identity| identity.as_str().to_owned())
        .collect::<BTreeSet<_>>();
    if selected_route_ids.len()
        != facts
            .selected_route_ids
            .len()
            .saturating_add(registry_failures.len())
        || &selected_route_ids != expected_selected_route_ids
    {
        bail!("capture-owned source refresh receipt omitted, duplicated, or added selected route outcomes");
    }
    let mut source_failures = facts.source_failures.clone();
    source_failures.extend(registry_failures.iter().cloned());
    let failed_route_outcomes = facts
        .failed_routes
        .iter()
        .map(|failure| {
            (
                failure.route_identity.as_str().to_owned(),
                (failure.class.as_str().to_owned(), failure.carried_forward),
            )
        })
        .chain(registry_failures.iter().map(|failure| {
            (
                failure.route_identity.as_str().to_owned(),
                (failure.class.as_str().to_owned(), failure.carried_forward),
            )
        }))
        .collect::<BTreeMap<_, _>>();
    if failed_route_outcomes.len()
        != facts
            .failed_routes
            .len()
            .saturating_add(registry_failures.len())
    {
        bail!("capture-owned source refresh receipt contains duplicate failed routes");
    }
    let successful_route_changes = facts
        .successful_route_outcomes
        .iter()
        .map(|outcome| {
            (
                outcome.route_identity.as_str().to_owned(),
                (outcome.changed, outcome.logical_source_failure_total),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let failed_routes = failed_route_outcomes
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    if successful_route_changes.len() != facts.successful_route_outcomes.len()
        || !successful_route_changes
            .keys()
            .all(|route| selected_route_ids.contains(route))
        || !successful_route_changes
            .keys()
            .all(|route| !failed_routes.contains(route))
        || successful_route_changes
            .len()
            .saturating_add(failed_routes.len())
            != selected_route_ids.len()
    {
        bail!("capture-owned source refresh receipt has an incomplete or overlapping terminal route-result partition");
    }
    let successful_route_rejections = facts
        .successful_route_outcomes
        .iter()
        .map(|outcome| {
            Ok((
                outcome.route_identity.as_str().to_owned(),
                committed_route_rejected_records(facts.manifest, &outcome.route_identity)?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut diagnostics_by_route = BTreeMap::<String, Vec<_>>::new();
    for failure in source_failures.failures() {
        diagnostics_by_route
            .entry(failure.route_identity.as_str().to_owned())
            .or_default()
            .push(SourceBackedRefreshSourceFailure {
                route_identity: failure.route_identity.as_str().to_owned(),
                source_identity: failure.source_identity.clone(),
                provider: failure.provider.as_str().to_owned(),
                class: failure.class.as_str().to_owned(),
                carried_forward: failure.carried_forward,
                source_selector: failure.source_selector.clone(),
                detail: failure.detail.clone(),
            });
    }
    for failure in facts.logical_source_failures.failures() {
        let source_identity = source_key_identity(&failure.source);
        diagnostics_by_route
            .entry(failure.route_identity.as_str().to_owned())
            .or_default()
            .push(SourceBackedRefreshSourceFailure {
                route_identity: failure.route_identity.as_str().to_owned(),
                source_identity: source_identity.clone(),
                provider: failure.source.provider().to_owned(),
                class: failure.class.as_str().to_owned(),
                carried_forward: failure.carried_forward,
                source_selector: format!("logical-source:{source_identity}"),
                detail: failure.detail.clone(),
            });
    }
    let mut rejections_by_route = BTreeMap::<String, Vec<_>>::new();
    for rejection in facts.record_rejections.rejections() {
        let route_identity = rejection.route_identity.as_str().to_owned();
        rejections_by_route
            .entry(route_identity.clone())
            .or_default()
            .push(SourceBackedRefreshRecordRejection {
                route_identity,
                source_identity: source_key_identity(&rejection.source),
                provider: rejection.provider.as_str().to_owned(),
                source_selector: rejection.source_selector.clone(),
                line: rejection.line_number,
                payload_type: rejection
                    .payload_type
                    .clone()
                    .unwrap_or_else(|| "unspecified".to_owned()),
                class: rejection.class.as_str().to_owned(),
                detail: rejection.detail.clone(),
            });
    }
    let route_results = selected_route_ids
        .iter()
        .map(|route_identity| {
            let mut result = successful_route_changes
                .get(route_identity)
                .copied()
                .map(|(changed, source_failure_total)| {
                    let mut result =
                        SourceBackedRefreshRouteResult::succeeded(route_identity.clone(), changed);
                    result.source_failure_total = source_failure_total;
                    result
                })
                .or_else(|| {
                    failed_route_outcomes
                        .get(route_identity)
                        .map(|(class, carried)| {
                            SourceBackedRefreshRouteResult::failed(
                                route_identity.clone(),
                                class.clone(),
                                *carried,
                            )
                        })
                })
                .ok_or_else(|| anyhow!("selected route has no terminal outcome"))?;
            result.source_failures = diagnostics_by_route
                .remove(route_identity)
                .unwrap_or_default();
            result.rejected_record_total = successful_route_rejections
                .get(route_identity)
                .copied()
                .unwrap_or_default();
            result.rejection_diagnostics = rejections_by_route
                .remove(route_identity)
                .unwrap_or_default();
            result.validate_source_failures()?;
            Ok(result)
        })
        .collect::<Result<Vec<_>>>()?;
    if !diagnostics_by_route.is_empty() || !rejections_by_route.is_empty() {
        bail!("capture-owned source refresh diagnostics name an unselected route");
    }
    Ok(route_results)
}

fn source_key_identity(source: &ctx_history_core::SourceKey) -> String {
    source
        .identity()
        .digest()
        .into_iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn committed_route_rejected_records(
    manifest: &GenerationManifest,
    route_identity: &SourceRouteIdentity,
) -> Result<u64> {
    let Some(route) = manifest.source_route(route_identity) else {
        return Ok(0);
    };
    route.sources().iter().try_fold(0_u64, |total, source| {
        let certificate = manifest
            .sources
            .binary_search_by_key(&source.identity().digest(), |candidate| {
                candidate.observation().source().identity().digest()
            })
            .ok()
            .and_then(|index| manifest.sources.get(index))
            .filter(|candidate| candidate.observation().source().exact_descriptor_eq(source))
            .ok_or_else(|| {
                anyhow!(
                    "committed route {} names a source without an exact certificate",
                    route_identity.as_str()
                )
            })?;
        total
            .checked_add(certificate.counts().rejected_records)
            .ok_or_else(|| anyhow!("committed route rejected-record total overflow"))
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
    let mut build =
        build_automatic_source_backed_registry_from_report(&discovery, data_root, report);
    build.discovery_duration = discovery_duration;
    Ok(build.registry.watch_catalog())
}

fn build_merged_source_backed_registry(
    discovery: &DiscoveryContext,
    mut report: DiscoveryReport,
    discovery_duration: StdDuration,
    data_root: &Path,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
) -> Result<MergedSourceBackedRegistry> {
    if let Some(catalog) = explicit_source_catalog {
        catalog.prepare_discovery_report(data_root, &mut report)?;
    }
    let mut build =
        build_automatic_source_backed_registry_from_report(discovery, data_root, report);
    build.discovery_duration = discovery_duration;
    let retained_generation = open_published_generation(data_root)?;
    let catalog_route_bindings = explicit_source_catalog
        .map(|catalog| {
            catalog.register_routes_after_discovery_merge(
                data_root,
                retained_generation.as_ref(),
                &mut build,
            )
        })
        .transpose()?
        .unwrap_or_default();
    Ok(MergedSourceBackedRegistry {
        build,
        published_explicit_source_catalog: explicit_source_catalog.cloned(),
        retained_generation,
        catalog_route_bindings,
    })
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
    retained_generation: Option<&VerifiedIndex>,
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
        let carried_forward = retained_generation.is_some_and(|index| {
            index
                .manifest()
                .source_route(&route_identity)
                .is_some_and(|route| !route.sources().is_empty())
        });
        failures.entry(route_identity.clone()).or_insert_with(|| {
            ctx_history_capture::SourceBackedFailedRoute::new(
                route_identity,
                automatic_registry_issue_source_identity(source),
                source.provider,
                class,
                carried_forward,
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
    let metadata = source_backed_route_inventory()
        .iter()
        .find(|route| {
            route.provider == source.provider && route.source_format == source.source_format
        })
        .filter(|route| route.automatic)
        .ok_or_else(|| {
            anyhow!(
                "automatic registry issue for {}/{} has no prior executable route contract",
                source.provider.as_str(),
                source.source_format,
            )
        })?;
    let mut digest = Sha256::new();
    digest.update(b"ctx.source-route-identity-v1\0");
    digest.update(source.provider.as_str().as_bytes());
    digest.update([0]);
    digest.update(metadata.certified_source_format.as_bytes());
    digest.update([0]);
    digest.update(b"automatic");
    digest.update([0]);
    digest.update(match metadata.selector_authority {
        SourceBackedSelectorAuthority::DiscoveredWinner => b"discovered-winner".as_slice(),
        SourceBackedSelectorAuthority::ExplicitPath => b"explicit-path".as_slice(),
        SourceBackedSelectorAuthority::CatalogLineage => b"catalog-lineage".as_slice(),
        SourceBackedSelectorAuthority::ExactCwd => b"exact-cwd".as_slice(),
        SourceBackedSelectorAuthority::NamedSurface => b"named-surface".as_slice(),
        SourceBackedSelectorAuthority::SelectedWithRetainedExplicit => {
            b"selected-with-retained-explicit".as_slice()
        }
    });
    SourceRouteIdentity::from_sha256(format!("{:x}", digest.finalize())).map_err(Into::into)
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
        coordinator.write_status(data_root, &job)?;
    }
    Ok(())
}
