use super::*;

mod completion;
mod exact_scan;
mod execution;
mod execution_prelude;
mod model;
mod ownership;
mod route_content;
mod route_outcomes;

use completion::run_after_successful_publication;
use ctx_history_capture_model::{
    source_level_progress, SharedAttemptHistoryProgress, SourceRecordProgress,
};
pub use ctx_history_capture_model::{
    SourceBackedDetailedRefreshProgress, SourceBackedRefreshProgress,
};
pub use ctx_history_capture_runtime::SourceBackedCertifiedRemoval;
use exact_scan::AttemptExactScanAccounting;
use execution::refresh_source_backed_generation_with_detailed_progress_and_discovery_timing;
use execution_prelude::{
    committed_progress, configured_provider_root_route_ids, discovery_started_progress,
    omit_empty_automatic_route, prepare_refresh, provider_roots_for_publication,
    publication_selected_route_ids, RefreshPrelude,
};
#[cfg(test)]
pub use model::assert_carried_route_failure;
pub use model::{
    SourceBackedPublicationMetadataContext, SourceBackedRefreshReceipt,
    SourceBackedSuccessfulRouteOutcome,
};
use model::{SourceBackedRefreshPlan, SourceBackedVerifiedPublication};
use route_content::source_route_content_fingerprints;
use route_outcomes::successful_route_outcomes_for_snapshot;

/// Keep the in-memory progress stream responsive enough for a live terminal
/// without turning every accepted record into a callback. Durable journal
/// writes are throttled separately by the refresh engine.
const SOURCE_RECORD_PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

type SourceBackedPublicationMetadataFactory<'factory> =
    dyn for<'context> FnMut(
            SourceBackedPublicationMetadataContext<'context>,
        ) -> ctx_history_index::Result<Vec<u8>>
        + 'factory;

#[derive(Debug, Clone, Copy)]
struct SourceBackedRefreshExecutionBudget {
    discovery_duration: Duration,
    work_budget: usize,
}

impl SourceBackedRefreshExecutionBudget {
    const fn new(discovery_duration: Duration, work_budget: usize) -> Self {
        Self {
            discovery_duration,
            work_budget,
        }
    }
}

#[cfg(test)]
use ownership::source_owner_covers_base_source;
use ownership::{
    automatic_carried_route_retirements, capture_staged_source_route_revalidation_receipts,
    require_complete_base_source_ownership, revalidate_staged_source_route,
    BaseSourceOwnershipEvidence,
};

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static BEFORE_SOURCE_BACKED_COMMIT_HOOK: std::cell::RefCell<
        Option<Box<dyn FnOnce()>>,
    > = const { std::cell::RefCell::new(None) };
}

#[doc(hidden)]
#[cfg(any(test, feature = "test-support"))]
pub fn install_before_source_backed_commit_hook_for_test(hook: impl FnOnce() + 'static) {
    BEFORE_SOURCE_BACKED_COMMIT_HOOK.with(|slot| {
        let previous = slot.replace(Some(Box::new(hook)));
        assert!(
            previous.is_none(),
            "source-backed precommit test hooks must not be nested"
        );
    });
}

#[cfg(any(test, feature = "test-support"))]
fn run_before_source_backed_commit_hook() {
    BEFORE_SOURCE_BACKED_COMMIT_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(any(test, feature = "test-support")))]
fn run_before_source_backed_commit_hook() {}

/// Capture-owned executor that can be installed behind the daemon's
/// provider-neutral `SourceBackedRefreshExecutor` callback seam.
#[derive(Debug, Clone)]
pub struct SourceBackedRefreshExecutor {
    registry: SourceBackedProviderRegistry,
    writer_options: WriterOptions,
    discovery_duration: Duration,
    work_budget: usize,
    base_route_controls: BTreeMap<SourceRouteIdentity, Vec<u8>>,
    attempt_history_progress: SharedAttemptHistoryProgress,
}

impl SourceBackedRefreshExecutor {
    pub fn new(registry: SourceBackedProviderRegistry, writer_options: WriterOptions) -> Self {
        Self::with_discovery_duration(registry, writer_options, Duration::ZERO)
    }

    pub fn with_discovery_duration(
        registry: SourceBackedProviderRegistry,
        writer_options: WriterOptions,
        discovery_duration: Duration,
    ) -> Self {
        let work_budget = source_backed_refresh_work_budget(writer_options.indexer_threads);
        Self {
            registry,
            writer_options,
            discovery_duration,
            work_budget,
            base_route_controls: BTreeMap::new(),
            attempt_history_progress: SharedAttemptHistoryProgress::default(),
        }
    }

    pub fn with_base_route_controls(
        mut self,
        controls: BTreeMap<SourceRouteIdentity, Vec<u8>>,
    ) -> Self {
        self.base_route_controls = controls;
        self
    }

    /// Attaches transient attempt-local scanner facts supplied by the refresh
    /// engine. Ordinary standalone capture callers retain an isolated handle.
    #[doc(hidden)]
    pub fn with_attempt_history_progress(mut self, progress: SharedAttemptHistoryProgress) -> Self {
        self.attempt_history_progress = progress;
        self
    }

    pub fn registry(&self) -> &SourceBackedProviderRegistry {
        &self.registry
    }

    pub fn refresh(
        &self,
        index_root: impl AsRef<Path>,
        report_progress: impl FnMut(SourceBackedRefreshProgress) -> SourceBackedRouteResult<()>,
    ) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
        let mut report_progress = report_progress;
        refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
            index_root,
            &self.registry,
            self.writer_options.clone(),
            SourceBackedRefreshExecutionBudget::new(self.discovery_duration, self.work_budget),
            (
                SourceBackedRefreshPlan::isolate(SourceBackedRefreshScope::All),
                &self.base_route_controls,
            ),
            move |update| {
                if update.current_source_progress.is_some() {
                    return Ok(());
                }
                report_progress(update.into_legacy())
            },
            None,
        )
    }

    pub fn refresh_with_detailed_progress(
        &self,
        index_root: impl AsRef<Path>,
        report_progress: impl FnMut(SourceBackedDetailedRefreshProgress) -> SourceBackedRouteResult<()>,
    ) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
        refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
            index_root,
            &self.registry,
            self.writer_options.clone(),
            SourceBackedRefreshExecutionBudget::new(self.discovery_duration, self.work_budget),
            (
                SourceBackedRefreshPlan::isolate(SourceBackedRefreshScope::All),
                &self.base_route_controls,
            ),
            report_progress,
            None,
        )
    }

    pub fn refresh_scope(
        &self,
        index_root: impl AsRef<Path>,
        scope: SourceBackedRefreshScope,
        report_progress: impl FnMut(SourceBackedRefreshProgress) -> SourceBackedRouteResult<()>,
    ) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
        let mut report_progress = report_progress;
        refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
            index_root,
            &self.registry,
            self.writer_options.clone(),
            SourceBackedRefreshExecutionBudget::new(self.discovery_duration, self.work_budget),
            (
                SourceBackedRefreshPlan::isolate(scope),
                &self.base_route_controls,
            ),
            move |update| {
                if update.current_source_progress.is_some() {
                    return Ok(());
                }
                report_progress(update.into_legacy())
            },
            None,
        )
    }

    pub fn refresh_scope_with_detailed_progress(
        &self,
        index_root: impl AsRef<Path>,
        scope: SourceBackedRefreshScope,
        report_progress: impl FnMut(SourceBackedDetailedRefreshProgress) -> SourceBackedRouteResult<()>,
    ) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
        refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
            index_root,
            &self.registry,
            self.writer_options.clone(),
            SourceBackedRefreshExecutionBudget::new(self.discovery_duration, self.work_budget),
            (
                SourceBackedRefreshPlan::isolate(scope),
                &self.base_route_controls,
            ),
            report_progress,
            None,
        )
    }

    pub fn refresh_scope_with_detailed_progress_and_reconciliation(
        &self,
        index_root: impl AsRef<Path>,
        scope: SourceBackedRefreshScope,
        reconciliation_demand: SourceBackedReconciliationDemand,
        report_progress: impl FnMut(SourceBackedDetailedRefreshProgress) -> SourceBackedRouteResult<()>,
    ) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
        refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
            index_root,
            &self.registry,
            self.writer_options.clone(),
            SourceBackedRefreshExecutionBudget::new(self.discovery_duration, self.work_budget),
            (
                SourceBackedRefreshPlan::isolate(scope)
                    .with_reconciliation_demand(reconciliation_demand),
                &self.base_route_controls,
            ),
            report_progress,
            None,
        )
    }

    /// Publishes one scope with control-plane metadata bound into the same
    /// opaque Core commit payload. The factory runs only for a pointer-
    /// advancing generation; exact reuse retains the active metadata.
    pub fn refresh_scope_with_detailed_progress_and_publication_metadata(
        &self,
        index_root: impl AsRef<Path>,
        scope: SourceBackedRefreshScope,
        report_progress: impl FnMut(SourceBackedDetailedRefreshProgress) -> SourceBackedRouteResult<()>,
        metadata_factory: impl for<'a> FnMut(
            SourceBackedPublicationMetadataContext<'a>,
        ) -> ctx_history_index::Result<Vec<u8>>,
    ) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
        self.refresh_scope_with_detailed_progress_publication_metadata_and_reconciliation(
            index_root,
            scope,
            SourceBackedReconciliationDemand::Exhaustive,
            report_progress,
            metadata_factory,
        )
    }

    pub fn refresh_scope_with_detailed_progress_publication_metadata_and_reconciliation(
        &self,
        index_root: impl AsRef<Path>,
        scope: SourceBackedRefreshScope,
        reconciliation_demand: SourceBackedReconciliationDemand,
        report_progress: impl FnMut(SourceBackedDetailedRefreshProgress) -> SourceBackedRouteResult<()>,
        metadata_factory: impl for<'a> FnMut(
            SourceBackedPublicationMetadataContext<'a>,
        ) -> ctx_history_index::Result<Vec<u8>>,
    ) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
        self.refresh_scope_with_detailed_progress_publication_metadata_reconciliation_and_worksets(
            index_root,
            scope,
            reconciliation_demand,
            BTreeMap::new(),
            report_progress,
            metadata_factory,
        )
    }

    pub fn refresh_scope_with_detailed_progress_publication_metadata_reconciliation_and_worksets(
        &self,
        index_root: impl AsRef<Path>,
        scope: SourceBackedRefreshScope,
        reconciliation_demand: SourceBackedReconciliationDemand,
        route_worksets: BTreeMap<SourceRouteIdentity, BTreeSet<PathBuf>>,
        report_progress: impl FnMut(SourceBackedDetailedRefreshProgress) -> SourceBackedRouteResult<()>,
        metadata_factory: impl for<'a> FnMut(
            SourceBackedPublicationMetadataContext<'a>,
        ) -> ctx_history_index::Result<Vec<u8>>,
    ) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
        self.refresh_physical_scope_with_detailed_progress_publication_metadata_reconciliation_and_worksets(
            index_root,
            scope.clone(),
            scope,
            reconciliation_demand,
            route_worksets,
            report_progress,
            metadata_factory,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn refresh_physical_scope_with_detailed_progress_publication_metadata_reconciliation_and_worksets(
        &self,
        index_root: impl AsRef<Path>,
        physical_scope: SourceBackedRefreshScope,
        publication_scope: SourceBackedRefreshScope,
        reconciliation_demand: SourceBackedReconciliationDemand,
        route_worksets: BTreeMap<SourceRouteIdentity, BTreeSet<PathBuf>>,
        report_progress: impl FnMut(SourceBackedDetailedRefreshProgress) -> SourceBackedRouteResult<()>,
        mut metadata_factory: impl for<'a> FnMut(
            SourceBackedPublicationMetadataContext<'a>,
        ) -> ctx_history_index::Result<Vec<u8>>,
    ) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
        refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
            index_root,
            &self.registry,
            self.writer_options.clone(),
            SourceBackedRefreshExecutionBudget::new(self.discovery_duration, self.work_budget),
            (
                SourceBackedRefreshPlan::isolate(physical_scope)
                    .with_publication_scope(publication_scope)
                    .with_reconciliation_demand(reconciliation_demand)
                    .with_route_worksets(route_worksets)
                    .with_attempt_history_progress(self.attempt_history_progress.clone()),
                &self.base_route_controls,
            ),
            report_progress,
            Some(&mut metadata_factory),
        )
    }
}

/// Runs every executable route against one writer and publishes one atomic
/// generation. This is the capture-owned executor seam for the daemon.
pub fn refresh_source_backed_generation(
    index_root: impl AsRef<Path>,
    registry: &SourceBackedProviderRegistry,
    writer_options: WriterOptions,
) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
    refresh_source_backed_generation_with_progress(index_root, registry, writer_options, |_| Ok(()))
}

#[cfg(test)]
pub(crate) fn refresh_source_backed_generation_with_work_budget_for_test(
    index_root: impl AsRef<Path>,
    registry: &SourceBackedProviderRegistry,
    writer_options: WriterOptions,
    work_budget: usize,
) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
    refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
        index_root,
        registry,
        writer_options,
        SourceBackedRefreshExecutionBudget::new(Duration::ZERO, work_budget),
        (
            SourceBackedRefreshPlan::isolate(SourceBackedRefreshScope::All),
            &BTreeMap::new(),
        ),
        |_| Ok(()),
        None,
    )
}

#[cfg(test)]
pub(crate) fn refresh_source_backed_generation_with_resource_limits_for_test(
    index_root: impl AsRef<Path>,
    registry: &SourceBackedProviderRegistry,
    writer_options: WriterOptions,
    maximum_live_output_bytes: u64,
    maximum_physical_scratch_bytes: u64,
) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
    let work_budget = source_backed_refresh_work_budget(writer_options.indexer_threads);
    refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
        index_root,
        registry,
        writer_options,
        SourceBackedRefreshExecutionBudget::new(Duration::ZERO, work_budget),
        (
            SourceBackedRefreshPlan::isolate(SourceBackedRefreshScope::All)
                .with_resource_limits(maximum_live_output_bytes, maximum_physical_scratch_bytes),
            &BTreeMap::new(),
        ),
        |_| Ok(()),
        None,
    )
}

pub fn refresh_source_backed_generation_with_progress(
    index_root: impl AsRef<Path>,
    registry: &SourceBackedProviderRegistry,
    writer_options: WriterOptions,
    report_progress: impl FnMut(SourceBackedRefreshProgress) -> SourceBackedRouteResult<()>,
) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
    let mut report_progress = report_progress;
    let work_budget = source_backed_refresh_work_budget(writer_options.indexer_threads);
    refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
        index_root,
        registry,
        writer_options,
        SourceBackedRefreshExecutionBudget::new(Duration::ZERO, work_budget),
        (
            SourceBackedRefreshPlan::isolate(SourceBackedRefreshScope::All),
            &BTreeMap::new(),
        ),
        move |update| {
            if update.current_source_progress.is_some() {
                return Ok(());
            }
            report_progress(update.into_legacy())
        },
        None,
    )
}

pub fn refresh_source_backed_generation_with_detailed_progress(
    index_root: impl AsRef<Path>,
    registry: &SourceBackedProviderRegistry,
    writer_options: WriterOptions,
    report_progress: impl FnMut(SourceBackedDetailedRefreshProgress) -> SourceBackedRouteResult<()>,
) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
    let work_budget = source_backed_refresh_work_budget(writer_options.indexer_threads);
    refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
        index_root,
        registry,
        writer_options,
        SourceBackedRefreshExecutionBudget::new(Duration::ZERO, work_budget),
        (
            SourceBackedRefreshPlan::isolate(SourceBackedRefreshScope::All),
            &BTreeMap::new(),
        ),
        report_progress,
        None,
    )
}

pub fn refresh_source_backed_generation_for_routes(
    index_root: impl AsRef<Path>,
    registry: &SourceBackedProviderRegistry,
    writer_options: WriterOptions,
    route_identities: impl IntoIterator<Item = SourceRouteIdentity>,
) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
    let work_budget = source_backed_refresh_work_budget(writer_options.indexer_threads);
    refresh_source_backed_generation_with_detailed_progress_and_discovery_timing(
        index_root,
        registry,
        writer_options,
        SourceBackedRefreshExecutionBudget::new(Duration::ZERO, work_budget),
        (
            SourceBackedRefreshPlan::isolate(SourceBackedRefreshScope::exact(route_identities)),
            &BTreeMap::new(),
        ),
        |_| Ok(()),
        None,
    )
}

fn bounded_source_failures<'a>(
    failures: impl IntoIterator<Item = &'a SourceBackedFailedRoute>,
) -> SourceBackedSourceFailures {
    SourceBackedSourceFailures::from_failures(failures.into_iter().cloned())
}

fn source_missing_observation_time() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

#[cfg(test)]
mod ownership_tests;
