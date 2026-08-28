use super::*;
use crate::route_ledger::{DirtySourceRouteAdmission, DirtySourceRoutes, EventWatermark};
use std::sync::atomic::{AtomicU64, Ordering};

mod admission;
#[cfg(test)]
mod admission_tests;
mod attempt_helpers;
mod coverage_contract;
mod durable_queue;
mod generation_authority;
mod generation_observation;
mod progress_model;
mod read_model;
mod request_lifecycle;
mod route_admission;
mod runtime_metadata;
mod startup_observation;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
mod watch_routes;
mod whole_run_eta;
use attempt_helpers::*;
use coverage_contract::{
    PostPublicationRouteCoverageFence, SourceBackedRefreshRouteCoverageCertificate,
};
pub use coverage_contract::{
    SourceBackedRefreshCoverageCertificate, SourceBackedRefreshRun,
    VerifiedSourceRefreshRouteBoundary,
};
use durable_queue::{
    durable_job_json, install_recovered_successors, job_with_queued_successors,
    recover_queued_root, recover_queued_successors,
};
use generation_authority::CoreRefreshTerminalSuccess;
pub use generation_authority::PinnedCorePublication;
use progress_model::{status_progress_total_sources_known, SourceBackedRefreshState};
pub use progress_model::{SourceBackedRefreshProgress, SourceBackedRefreshStage};
use read_model::{
    projected_job_json, projected_status_json, SourceBackedAutomaticRetryCheckpoint,
    SourceBackedRefreshAttempt, SourceBackedRefreshFailureOutcome,
};
use runtime_metadata::{canonical_daemon_mode, SourceRefreshRuntimeMetadata};
pub use runtime_metadata::{RefreshRuntime, RefreshRuntimeMetadata};
use startup_observation::{
    hermes_routes_requiring_control_recovery, overdue_hermes_exact_routes,
    startup_routes_requiring_refresh,
};
#[cfg(test)]
pub(crate) use test_support::TestRefreshJournal;
#[cfg(test)]
use test_support::{
    daemon_source_backed_refresh_job_path, pin_test_active_verified_generation,
    pin_test_published_generation, read_daemon_job_status, status_value, test_refresh_engine,
    test_refresh_engine_with_executor, test_refresh_engine_with_executor_and_admitted_routes,
    test_refresh_runtime, test_refresh_submission, write_daemon_job_status,
};
use whole_run_eta::WholeRunEtaEstimator;

#[derive(Default)]
pub(crate) struct SourceBackedRefreshProgressUpdate {
    pub(super) phase: String,
    pub(super) completed_sources: usize,
    pub(super) total_sources: usize,
    pub(super) total_sources_known: bool,
    pub(super) current_source: Option<String>,
    pub(super) completed_records: Option<u64>,
    pub(super) completed_bytes: Option<u64>,
    pub(super) providers: Vec<String>,
    pub(super) processed_sessions: u64,
    pub(super) processed_messages: u64,
    pub(super) processed_tool_calls: u64,
    pub(super) processed_bytes: u64,
    pub(super) elapsed_millis: Option<u64>,
    pub(super) current_source_progress: Option<SourceBackedCurrentSourceProgress>,
    pub(super) exact_scan_progress: Option<SourceBackedExactScanProgress>,
}
/// Provider-neutral callback boundary for one daemon-serialized refresh.
pub trait SourceBackedRefreshExecutor: Send + Sync {
    fn refresh(
        &self,
        execution: SourceBackedRefreshExecution<'_>,
    ) -> Result<SourceBackedRefreshPublication>;

    #[cfg(test)]
    fn implementation_name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }
}

impl<F> SourceBackedRefreshExecutor for F
where
    F: for<'a> Fn(SourceBackedRefreshExecution<'a>) -> Result<SourceBackedRefreshPublication>
        + Send
        + Sync,
{
    fn refresh(
        &self,
        execution: SourceBackedRefreshExecution<'_>,
    ) -> Result<SourceBackedRefreshPublication> {
        self(execution)
    }
}

#[derive(Debug, Default)]
pub(super) struct CaptureOwnedSourceBackedRefreshExecutor;

impl SourceBackedRefreshExecutor for CaptureOwnedSourceBackedRefreshExecutor {
    fn refresh(
        &self,
        execution: SourceBackedRefreshExecution<'_>,
    ) -> Result<SourceBackedRefreshPublication> {
        ctx_history_refresh_execution::execute_refresh(execution)
    }
}

pub(super) struct CoreRefreshEngineState {
    active_request_id: Option<String>,
    pending_request_ids: VecDeque<String>,
    attempts: VecDeque<SourceBackedRefreshAttempt>,
    pinned_core_publication: Option<Arc<PinnedCorePublication>>,
    current_published_generation: Option<String>,
    dirty_routes: DirtySourceRoutes,
    known_route_ids: BTreeSet<SourceRouteIdentity>,
    watch_catalog: Option<SourceBackedWatchCatalog>,
    watch_catalog_revision: u64,
    watch_uncertain_through: Option<EventWatermark>,
    hermes_routes_requiring_exhaustive_recovery: BTreeSet<SourceRouteIdentity>,
    routes_requiring_exhaustive_reconciliation: BTreeSet<SourceRouteIdentity>,
    route_event_watermarks: BTreeMap<SourceRouteIdentity, EventWatermark>,
    route_worksets: BTreeMap<SourceRouteIdentity, SourceBackedRefreshWorkset>,
    route_retry_intents: BTreeMap<SourceRouteIdentity, Arc<RefreshIntent>>,
    route_admissions: BTreeMap<String, Vec<DirtySourceRouteAdmission>>,
    route_admission_watermarks: BTreeMap<String, BTreeMap<SourceRouteIdentity, EventWatermark>>,
    automatic_retry_checkpoints:
        BTreeMap<SourceRouteIdentity, SourceBackedAutomaticRetryCheckpoint>,
    pending_terminal_persistence: Option<PendingTerminalPersistence>,
    pending_scheduler_retry_root_id: Option<String>,
    unacknowledged_admissions: BTreeMap<String, usize>,
    admission_resolutions_in_flight: BTreeSet<String>,
    watch_routes_initialized: bool,
    last_progress_persisted_request_id: Option<String>,
    last_progress_persisted_at: Option<StdInstant>,
}

struct PendingTerminalPersistence {
    request_id: String,
    terminal_job: Value,
    outcome: PendingTerminalOutcome,
}

enum PendingTerminalOutcome {
    Published {
        terminal: CoreRefreshTerminalSuccess,
        did_work: bool,
    },
    Failed {
        scheduler_retry: bool,
    },
}

impl PendingTerminalPersistence {
    fn did_work(&self) -> bool {
        matches!(
            self.outcome,
            PendingTerminalOutcome::Published { did_work: true, .. }
        )
    }

    fn failed(&self) -> bool {
        matches!(self.outcome, PendingTerminalOutcome::Failed { .. })
    }

    fn scheduler_retry(&self) -> bool {
        matches!(
            self.outcome,
            PendingTerminalOutcome::Failed {
                scheduler_retry: true
            }
        )
    }
}

struct RouteAdmissionFinish {
    coverage_certificate: Option<SourceBackedRefreshCoverageCertificate>,
    durable_request_id: String,
}

impl fmt::Debug for PendingTerminalPersistence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PendingTerminalPersistence")
            .field("request_id", &self.request_id)
            .field("did_work", &self.did_work())
            .field("failed", &self.failed())
            .finish_non_exhaustive()
    }
}

type SourceRefreshAdmissionFence = dyn Fn(
        &DiscoveryContext,
        &dyn RefreshJournal,
        &Path,
        Option<&ExplicitSourceCatalogAuthority>,
    ) -> Result<ctx_history_refresh_execution::AdmittedRefresh>
    + Send
    + Sync;

#[cfg(any(test, feature = "test-support"))]
type TestSourceRefreshAdmissionFence = dyn Fn(
        &DiscoveryContext,
        &dyn RefreshJournal,
        &Path,
        Option<&ExplicitSourceCatalogAuthority>,
    ) -> Result<BTreeMap<SourceRouteIdentity, Option<String>>>
    + Send
    + Sync;

pub struct CoreRefreshEngine {
    state: Mutex<CoreRefreshEngineState>,
    request_activity_generation: AtomicU64,
    pub(super) executor: Arc<dyn SourceBackedRefreshExecutor>,
    admission_fence: Arc<SourceRefreshAdmissionFence>,
    pub(super) journal: Arc<dyn RefreshJournal>,
    pub(super) runtime: Arc<dyn RefreshRuntime>,
}

#[derive(Debug)]
struct SourceBackedRefreshQueueFull {
    active_pending_requests: usize,
}

#[derive(Debug)]
struct SourceBackedRefreshIdempotencyConflict {
    request_id: String,
}

impl SourceBackedRefreshQueueFull {
    fn to_json(&self) -> Value {
        compact_json(json!({
            "ok": false,
            "schema_version": 1,
            "owner": "daemon",
            "status": "busy",
            "error_code": "source_refresh_queue_full",
            "reason": "queue_full",
            "retryable": true,
            "active_pending_requests": self.active_pending_requests,
            "max_active_pending_requests": SOURCE_REFRESH_ACTIVE_PENDING_LIMIT,
            "error": self.to_string(),
        }))
    }
}

impl SourceBackedRefreshIdempotencyConflict {
    fn to_json(&self) -> Value {
        compact_json(json!({
            "ok": false,
            "schema_version": 1,
            "owner": "daemon",
            "request_id": self.request_id,
            "request_state": "request_conflict",
            "error_code": "request_id_conflict",
            "reason": "request_id_payload_mismatch",
            "retryable": false,
            "error": self.to_string(),
        }))
    }
}

impl fmt::Display for SourceBackedRefreshQueueFull {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "daemon source refresh queue is full ({}/{} active or pending requests); retry after a refresh finishes",
            self.active_pending_requests,
            SOURCE_REFRESH_ACTIVE_PENDING_LIMIT,
        )
    }
}

impl std::error::Error for SourceBackedRefreshQueueFull {}

impl fmt::Display for SourceBackedRefreshIdempotencyConflict {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "daemon source refresh request ID {} was already admitted for a different request payload",
            self.request_id
        )
    }
}

impl std::error::Error for SourceBackedRefreshIdempotencyConflict {}

impl CoreRefreshEngine {
    pub fn new(journal: Arc<dyn RefreshJournal>, runtime: Arc<dyn RefreshRuntime>) -> Self {
        Self::with_executor(
            journal,
            runtime,
            Arc::new(CaptureOwnedSourceBackedRefreshExecutor),
        )
    }

    pub fn with_executor(
        journal: Arc<dyn RefreshJournal>,
        runtime: Arc<dyn RefreshRuntime>,
        executor: Arc<dyn SourceBackedRefreshExecutor>,
    ) -> Self {
        Self::with_runtime(
            executor,
            Arc::new(source_backed_route_admission_fence),
            journal,
            runtime,
        )
    }

    fn with_runtime(
        executor: Arc<dyn SourceBackedRefreshExecutor>,
        admission_fence: Arc<SourceRefreshAdmissionFence>,
        journal: Arc<dyn RefreshJournal>,
        runtime: Arc<dyn RefreshRuntime>,
    ) -> Self {
        Self {
            state: Mutex::new(CoreRefreshEngineState {
                active_request_id: None,
                pending_request_ids: VecDeque::new(),
                attempts: VecDeque::new(),
                pinned_core_publication: None,
                current_published_generation: None,
                dirty_routes: DirtySourceRoutes::default(),
                known_route_ids: BTreeSet::new(),
                watch_catalog: None,
                watch_catalog_revision: 0,
                watch_uncertain_through: None,
                hermes_routes_requiring_exhaustive_recovery: BTreeSet::new(),
                routes_requiring_exhaustive_reconciliation: BTreeSet::new(),
                route_event_watermarks: BTreeMap::new(),
                route_worksets: BTreeMap::new(),
                route_retry_intents: BTreeMap::new(),
                route_admissions: BTreeMap::new(),
                route_admission_watermarks: BTreeMap::new(),
                automatic_retry_checkpoints: BTreeMap::new(),
                pending_terminal_persistence: None,
                pending_scheduler_retry_root_id: None,
                unacknowledged_admissions: BTreeMap::new(),
                admission_resolutions_in_flight: BTreeSet::new(),
                watch_routes_initialized: false,
                last_progress_persisted_request_id: None,
                last_progress_persisted_at: None,
            }),
            request_activity_generation: AtomicU64::new(0),
            executor,
            admission_fence,
            journal,
            runtime,
        }
    }

    #[cfg(test)]
    pub fn with_journal_for_test(
        journal: Arc<dyn RefreshJournal>,
        runtime: Arc<dyn RefreshRuntime>,
        executor: Arc<dyn SourceBackedRefreshExecutor>,
    ) -> Self {
        Self::with_executor(journal, runtime, executor)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn with_runtime_for_test(
        journal: Arc<dyn RefreshJournal>,
        runtime: Arc<dyn RefreshRuntime>,
        executor: Arc<dyn SourceBackedRefreshExecutor>,
        admission_fence: Arc<TestSourceRefreshAdmissionFence>,
    ) -> Self {
        let adapted = Arc::new(
            move |discovery: &DiscoveryContext,
                  journal: &dyn RefreshJournal,
                  data_root: &Path,
                  catalog: Option<&ExplicitSourceCatalogAuthority>| {
                let configured_provider_roots = discovery.configured_provider_roots().to_vec();
                let automatic_provider_discovery = discovery.automatic_provider_discovery_enabled();
                admission_fence(discovery, journal, data_root, catalog)
                    .map(admitted_refresh_for_test)
                    .map(|admitted| {
                        admitted
                            .with_configured_provider_roots_for_test(configured_provider_roots)
                            .with_automatic_provider_discovery_for_test(
                                automatic_provider_discovery,
                            )
                    })
            },
        );
        Self::with_runtime(executor, adapted, journal, runtime)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn with_admission_fence_for_test(
        journal: Arc<dyn RefreshJournal>,
        runtime: Arc<dyn RefreshRuntime>,
        admission_fence: Arc<TestSourceRefreshAdmissionFence>,
    ) -> Self {
        Self::with_runtime_for_test(
            journal,
            runtime,
            Arc::new(CaptureOwnedSourceBackedRefreshExecutor),
            admission_fence,
        )
    }

    pub fn has_pending_request(&self) -> bool {
        let state = self.lock_state();
        state.pending_terminal_persistence.is_some()
            || state
                .active_request_id
                .as_deref()
                .and_then(|request_id| find_attempt(&state, request_id))
                .is_some_and(|attempt| attempt.state.is_active())
            || state.pending_request_ids.iter().any(|request_id| {
                find_attempt(&state, request_id).is_some_and(|attempt| attempt.state.is_active())
            })
    }

    /// Monotonic evidence that this process admitted explicit refresh demand.
    ///
    /// A finite worker uses this to observe requests that completed between
    /// IPC admission and its next scheduler-loop sample.
    pub fn request_activity_generation(&self) -> u64 {
        self.request_activity_generation.load(Ordering::Acquire)
    }

    pub(super) fn lock_state(&self) -> std::sync::MutexGuard<'_, CoreRefreshEngineState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
}
