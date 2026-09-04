use super::*;
use crate::publication::{open_published_generation_for_recovery, PublishedGenerationOpen};
use ctx_history_refresh_execution::PublishedSourceBackedGeneration;

mod catalog_witness;
use catalog_witness::retained_generation_state;

pub(crate) struct RetainedPublishedState<'a> {
    pub(crate) journal: &'a dyn RefreshJournal,
}

impl PublishedSourceBackedStatePort for RetainedPublishedState<'_> {
    fn open_published_state(&self, data_root: &Path) -> Result<PublishedSourceBackedState> {
        let generation = match open_published_generation_for_recovery(data_root, self.journal)? {
            PublishedGenerationOpen::Missing => PublishedSourceBackedGeneration::Missing,
            PublishedGenerationOpen::RebuildRequired => {
                PublishedSourceBackedGeneration::RebuildRequired
            }
            PublishedGenerationOpen::Verified(index) => {
                PublishedSourceBackedGeneration::Verified((*index).into_generation_snapshot())
            }
        };
        let verified_generation = match &generation {
            PublishedSourceBackedGeneration::Verified(generation) => Some(generation),
            PublishedSourceBackedGeneration::Missing
            | PublishedSourceBackedGeneration::RebuildRequired => None,
        };
        let (explicit_source_catalog, catalog_route_bindings, route_controls) =
            retained_generation_state(verified_generation)?;
        Ok(PublishedSourceBackedState {
            generation,
            explicit_source_catalog,
            catalog_route_bindings,
            route_controls,
        })
    }
}

pub(super) fn execute_source_backed_refresh(
    executor: &dyn SourceBackedRefreshExecutor,
    data_root: &Path,
    request_id: &str,
    coordinator: &CoreRefreshEngine,
    intent: &RefreshIntent,
    reconciliation_demand: SourceBackedReconciliationDemand,
    admitted: ctx_history_refresh_execution::AdmittedRefresh,
) -> Result<SourceBackedRefreshPublication> {
    let index_root = source_backed_index_root(data_root);
    let attempt_history_progress = coordinator.attempt_history_progress(request_id)?;
    let discovery_context = coordinator.runtime.discovery_context(data_root)?;
    let published_state = RetainedPublishedState {
        journal: coordinator.journal.as_ref(),
    };
    let report_progress = |update: PhysicalRefreshProgressUpdate| {
        coordinator.persist_progress(
            data_root,
            request_id,
            SourceBackedRefreshProgressUpdate {
                phase: update.phase,
                completed_sources: update.completed_sources,
                total_sources: update.total_sources,
                total_sources_known: update.total_sources_known,
                current_source: update.current_source,
                completed_records: update.completed_records,
                completed_bytes: update.completed_bytes,
                providers: update.providers,
                processed_sessions: update.processed_sessions,
                processed_messages: update.processed_messages,
                processed_tool_calls: update.processed_tool_calls,
                processed_bytes: update.processed_bytes,
                elapsed_millis: update.elapsed_millis,
                current_source_progress: update.current_source_progress,
                exact_scan_progress: update.exact_scan_progress,
            },
        )
    };
    executor.refresh(
        SourceBackedRefreshExecution::new(
            data_root,
            &index_root,
            request_id,
            intent.operation(),
            intent.explicit_source_authority(),
            admitted,
            &discovery_context,
            &published_state,
            &report_progress,
        )
        .with_reconciliation_demand(reconciliation_demand)
        .with_attempt_history_progress(attempt_history_progress),
    )
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) fn refresh_all_provider_sources(
    discovery: &DiscoveryContext,
    report: DiscoveryReport,
    discovery_duration: StdDuration,
    data_root: &Path,
    index_root: &Path,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    scope: SourceBackedRefreshScope,
    report_progress: &mut dyn FnMut(
        CaptureSourceBackedDetailedRefreshProgress,
    ) -> SourceBackedRouteResult<()>,
) -> Result<SourceBackedRefreshPublication> {
    let journal = TestRefreshJournal::default();
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
        &journal,
        report_progress,
    )
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) fn refresh_all_provider_sources_route_local(
    discovery: &DiscoveryContext,
    report: DiscoveryReport,
    discovery_duration: StdDuration,
    request_id: &str,
    operation: SourceBackedRefreshOperation,
    data_root: &Path,
    index_root: &Path,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    scope: SourceBackedRefreshScope,
    journal: &dyn RefreshJournal,
    report_progress: &mut dyn FnMut(
        CaptureSourceBackedDetailedRefreshProgress,
    ) -> SourceBackedRouteResult<()>,
) -> Result<SourceBackedRefreshPublication> {
    let published_state = RetainedPublishedState { journal };
    ctx_history_refresh_execution::refresh_all_provider_sources_route_local(
        discovery,
        report,
        discovery_duration,
        request_id,
        operation,
        data_root,
        index_root,
        explicit_source_catalog,
        scope,
        &published_state,
        report_progress,
    )
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn admitted_refresh_for_test(
    route_observations: BTreeMap<SourceRouteIdentity, Option<String>>,
) -> ctx_history_refresh_execution::AdmittedRefresh {
    ctx_history_refresh_execution::AdmittedRefresh::for_test(
        ctx_history_refresh_execution::AdmittedRefreshCoverage::CompleteCatalog,
        route_observations.keys().cloned().collect(),
        ctx_history_refresh_execution::SourceBackedAdmittedDiscovery::new(
            ctx_history_capture::DiscoveryReport {
                sources: Vec::new(),
                issues: Vec::new(),
            },
            StdDuration::ZERO,
            SourceBackedWatchCatalog::default(),
        ),
    )
    .expect("test admission authority must be valid")
}

/// Resolves one logical all-route request into immutable exact execution
/// authority. Discovery happens once here and cannot be repeated by execution.
pub(super) fn source_backed_route_admission_fence(
    discovery: &DiscoveryContext,
    journal: &dyn RefreshJournal,
    data_root: &Path,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
) -> Result<ctx_history_refresh_execution::AdmittedRefresh> {
    let discovery = discovery.clone().with_data_root(data_root);
    let work_budget =
        source_backed_refresh_work_budget(source_backed_refresh_writer_options().indexer_threads);
    let discovery_started = StdInstant::now();
    let report = discover_provider_sources_with_context_and_work_budget(&discovery, work_budget);
    let discovery_duration = discovery_started.elapsed();
    validate_provider_source_roots_outside_data_root(data_root, report.sources.iter())
        .context("validate provider roots before admitting source refresh demand")?;
    prepare_generation_control_state(data_root)?;
    let published_state = RetainedPublishedState { journal };
    let admitted = ctx_history_refresh_execution::source_backed_admitted_discovery_from_report(
        &discovery,
        report,
        discovery_duration,
        data_root,
        ctx_history_refresh_execution::AdmittedRefreshCoverage::CompleteCatalog,
        explicit_source_catalog,
        &published_state,
    )?;
    Ok(admitted)
}
