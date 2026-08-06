use super::*;
use crate::route_ledger::{DirtySourceRouteAdmission, DirtySourceRoutes, EventWatermark};

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
mod runtime_metadata;
mod startup_observation;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
#[cfg(test)]
type TestStatusWriter = Arc<dyn Fn(&Path, &Value) -> Result<()> + Send + Sync>;
use attempt_helpers::*;
use coverage_contract::{
    ManualAllContinuation, PostPublicationRouteCoverageFence,
    SourceBackedRefreshRouteCoverageCertificate,
};
pub use coverage_contract::{
    SourceBackedRefreshCoverageCertificate, SourceBackedRefreshRun,
    VerifiedSourceRefreshRouteBoundary,
};
use durable_queue::{
    durable_job_json, install_recovered_successors, job_with_queued_successors,
    recover_logical_demand_continuations, recover_queued_root, recover_queued_successors,
};
use generation_authority::CoreRefreshTerminalSuccess;
pub use generation_authority::PinnedCorePublication;
use progress_model::{status_progress_total_sources_known, SourceBackedRefreshState};
pub use progress_model::{
    SourceBackedCurrentSourceProgress, SourceBackedCurrentSourceProgressStage,
    SourceBackedRefreshProgress, SourceBackedRefreshTimings,
};
use read_model::{
    projected_job_json, projected_status_json, source_backed_route_retry_disposition,
    SourceBackedRefreshAttempt, SourceBackedRefreshFailureOutcome,
};
pub(super) use read_model::{refresh_scope_from_json, refresh_scope_json};
pub use read_model::{
    SourceBackedRefreshCatalogRouteOutcome, SourceBackedRefreshReceipt,
    SourceBackedRefreshRecordRejection, SourceBackedRefreshRouteOutcome,
    SourceBackedRefreshRouteResult, SourceBackedRefreshSourceFailure,
};
use runtime_metadata::{canonical_daemon_mode, SourceRefreshRuntimeMetadata};
pub use runtime_metadata::{RefreshRuntime, RefreshRuntimeMetadata};
use startup_observation::startup_routes_requiring_refresh;
#[cfg(test)]
pub(crate) use test_support::TestRefreshJournal;
#[cfg(test)]
use test_support::{
    daemon_source_backed_refresh_job_path, open_test_published_generation,
    pin_test_active_verified_generation, pin_test_published_generation, read_daemon_job_status,
    status_value, test_refresh_engine, test_refresh_engine_with_executor,
    test_refresh_engine_with_status_writer, test_refresh_runtime, test_refresh_submission,
    write_daemon_job_status,
};

pub(crate) struct SourceBackedRefreshProgressUpdate {
    pub(super) phase: String,
    pub(super) completed_sources: usize,
    pub(super) total_sources: usize,
    pub(super) total_sources_known: bool,
    pub(super) current_source: Option<String>,
    pub(super) completed_records: Option<u64>,
    pub(super) completed_bytes: Option<u64>,
    pub(super) current_source_progress: Option<SourceBackedCurrentSourceProgress>,
}

/// Daemon-owned execution context passed to the capture refresh callback.
///
/// The callback owns source/provider discovery and publication. The daemon
/// owns request serialization, progress persistence, and publication
/// verification.
#[allow(dead_code)] // All fields are part of the capture-coordinator callback seam.
pub struct SourceBackedRefreshExecution<'a> {
    pub data_root: &'a Path,
    pub index_root: &'a Path,
    pub request_id: &'a str,
    pub operation: RefreshOperation,
    pub explicit_source_catalog: Option<&'a ExplicitSourceCatalogAuthority>,
    pub scope: SourceBackedRefreshScope,
    pub covered_route_ids: BTreeSet<SourceRouteIdentity>,
    pub covered_publication: SourceBackedRefreshCoveredPublication,
    pub discovery_context: &'a DiscoveryContext,
    pub journal: &'a dyn RefreshJournal,
    pub(super) report_progress: &'a dyn Fn(SourceBackedRefreshProgressUpdate) -> Result<()>,
}

impl SourceBackedRefreshExecution<'_> {
    pub fn report_progress(
        &self,
        phase: &str,
        completed_sources: usize,
        total_sources: usize,
        current_source: Option<String>,
        completed_records: Option<u64>,
        completed_bytes: Option<u64>,
    ) -> Result<()> {
        self.report_detailed_progress(
            phase,
            completed_sources,
            total_sources,
            current_source,
            completed_records,
            completed_bytes,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn report_detailed_progress(
        &self,
        phase: &str,
        completed_sources: usize,
        total_sources: usize,
        current_source: Option<String>,
        completed_records: Option<u64>,
        completed_bytes: Option<u64>,
        current_source_progress: Option<SourceBackedCurrentSourceProgress>,
    ) -> Result<()> {
        self.report_detailed_progress_with_total_state(
            phase,
            completed_sources,
            total_sources,
            true,
            current_source,
            completed_records,
            completed_bytes,
            current_source_progress,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn report_detailed_progress_with_total_state(
        &self,
        phase: &str,
        completed_sources: usize,
        total_sources: usize,
        total_sources_known: bool,
        current_source: Option<String>,
        completed_records: Option<u64>,
        completed_bytes: Option<u64>,
        current_source_progress: Option<SourceBackedCurrentSourceProgress>,
    ) -> Result<()> {
        (self.report_progress)(SourceBackedRefreshProgressUpdate {
            phase: phase.to_owned(),
            completed_sources,
            total_sources,
            total_sources_known,
            current_source,
            completed_records,
            completed_bytes,
            current_source_progress,
        })
    }
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
        execute_capture_owned_refresh(execution)
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
    route_event_watermarks: BTreeMap<SourceRouteIdentity, EventWatermark>,
    route_admissions: BTreeMap<String, Vec<DirtySourceRouteAdmission>>,
    route_admission_watermarks: BTreeMap<String, BTreeMap<SourceRouteIdentity, EventWatermark>>,
    manual_all_continuations: BTreeMap<String, ManualAllContinuation>,
    pending_terminal_persistence: Option<PendingTerminalPersistence>,
    pending_scheduler_retry_root_id: Option<String>,
    unacknowledged_admissions: BTreeMap<String, usize>,
    admission_resolutions_in_flight: BTreeSet<String>,
    watch_routes_initialized: bool,
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
    ) -> Result<BTreeMap<SourceRouteIdentity, Option<String>>>
    + Send
    + Sync;

pub struct CoreRefreshEngine {
    state: Mutex<CoreRefreshEngineState>,
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

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum SourceRefreshAdmissionRequirement {
    /// Observe the terminal result of equivalent work already admitted.
    AttachEquivalent,
    /// Require work whose admission occurs after the currently running attempt.
    FreshAfterAdmittedSnapshot,
}

struct SourceRefreshLogicalDemand {
    admission: SourceRefreshAdmissionRequirement,
    route_observations: BTreeMap<SourceRouteIdentity, Option<String>>,
    request_id: Option<String>,
    request_fingerprint: Option<String>,
    admission_pending: bool,
}

impl SourceRefreshAdmissionRequirement {
    fn requires_successor(self, state: SourceBackedRefreshState) -> bool {
        self == Self::FreshAfterAdmittedSnapshot && state == SourceBackedRefreshState::Running
    }
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
                route_event_watermarks: BTreeMap::new(),
                route_admissions: BTreeMap::new(),
                route_admission_watermarks: BTreeMap::new(),
                manual_all_continuations: BTreeMap::new(),
                pending_terminal_persistence: None,
                pending_scheduler_retry_root_id: None,
                unacknowledged_admissions: BTreeMap::new(),
                admission_resolutions_in_flight: BTreeSet::new(),
                watch_routes_initialized: false,
            }),
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
        admission_fence: Arc<SourceRefreshAdmissionFence>,
    ) -> Self {
        Self::with_runtime(executor, admission_fence, journal, runtime)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn with_admission_fence_for_test(
        journal: Arc<dyn RefreshJournal>,
        runtime: Arc<dyn RefreshRuntime>,
        admission_fence: Arc<SourceRefreshAdmissionFence>,
    ) -> Self {
        Self::with_runtime(
            Arc::new(CaptureOwnedSourceBackedRefreshExecutor),
            admission_fence,
            journal,
            runtime,
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

    pub fn initialize_watch_route_authority(
        &self,
        routes: impl IntoIterator<Item = SourceRouteIdentity>,
    ) {
        let routes = routes.into_iter().collect::<BTreeSet<_>>();
        let mut state = self.lock_state();
        state.dirty_routes.retain_exact_routes(&routes);
        state
            .route_event_watermarks
            .retain(|route, _| routes.contains(route));
        for continuation in state.manual_all_continuations.values_mut() {
            for route in &routes {
                continuation.invalidate_route(route);
            }
        }
        state.known_route_ids = routes;
        state.watch_routes_initialized = true;
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn reconcile_watch_routes(
        &self,
        routes: impl IntoIterator<Item = SourceRouteIdentity>,
        watermark: EventWatermark,
        observed_at_ms: u64,
    ) {
        let routes = routes.into_iter().collect::<BTreeSet<_>>();
        self.initialize_watch_route_authority(routes.iter().cloned());
        self.schedule_startup_route_reconciliation(routes, watermark, observed_at_ms);
    }

    pub fn schedule_startup_route_reconciliation(
        &self,
        routes: impl IntoIterator<Item = SourceRouteIdentity>,
        watermark: EventWatermark,
        observed_at_ms: u64,
    ) {
        let mut state = self.lock_state();
        let routes = routes
            .into_iter()
            .filter(|route| state.known_route_ids.contains(route))
            .collect::<Vec<_>>();
        for route in &routes {
            state
                .route_event_watermarks
                .entry(route.clone())
                .and_modify(|current| *current = (*current).max(watermark))
                .or_insert(watermark);
        }
        state
            .dirty_routes
            .seed_exact_routes(routes, watermark, observed_at_ms);
    }

    /// Performs the bounded provider-neutral startup preflight. The watcher is
    /// already active when this runs. Only a generation-bound exact
    /// `Unchanged` observation stays clean; every other route enters the
    /// normal fail-closed refresh path.
    pub fn schedule_startup_route_observation(
        &self,
        catalog: &SourceBackedWatchCatalog,
        watermark: EventWatermark,
        observed_at_ms: u64,
    ) {
        self.schedule_startup_route_observation_with_budget(
            catalog,
            watermark,
            observed_at_ms,
            SOURCE_REFRESH_STARTUP_OBSERVATION_BUDGET,
        );
    }

    fn schedule_startup_route_observation_with_budget(
        &self,
        catalog: &SourceBackedWatchCatalog,
        watermark: EventWatermark,
        observed_at_ms: u64,
        budget: StdDuration,
    ) {
        let authority = self.pinned_core_publication();
        let metadata = authority.as_deref().and_then(|authority| {
            SourceBackedPublicationMetadata::decode(authority.verified_index_ref()).ok()
        });
        let missing_routes = authority
            .as_deref()
            .map(|authority| {
                authority
                    .verified_index_ref()
                    .manifest()
                    .source_routes()
                    .iter()
                    .filter(|snapshot| snapshot.missing_state().is_some())
                    .map(|snapshot| snapshot.route_identity().clone())
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        let dirty = startup_routes_requiring_refresh(
            catalog,
            metadata
                .as_ref()
                .map(|metadata| &metadata.route_observations),
            &missing_routes,
            budget,
        );
        let mut state = self.lock_state();
        let dirty = dirty
            .into_iter()
            .filter(|route| state.known_route_ids.contains(route))
            .collect::<Vec<_>>();
        for route in &dirty {
            state
                .route_event_watermarks
                .entry(route.clone())
                .and_modify(|current| *current = (*current).max(watermark))
                .or_insert(watermark);
        }
        state
            .dirty_routes
            .seed_exact_routes(dirty, watermark, observed_at_ms);
    }

    pub fn record_watch_routes(
        &self,
        routes: impl IntoIterator<Item = (SourceRouteIdentity, EventWatermark)>,
        observed_at_ms: u64,
    ) {
        let mut state = self.lock_state();
        for (route, watermark) in routes {
            if state.known_route_ids.contains(&route) {
                let recorded =
                    state
                        .dirty_routes
                        .record_event(route.clone(), watermark, observed_at_ms);
                if recorded {
                    state
                        .route_event_watermarks
                        .insert(route.clone(), watermark);
                    for continuation in state.manual_all_continuations.values_mut() {
                        continuation.invalidate_route(&route);
                    }
                }
            }
        }
    }

    pub fn watch_routes_initialized(&self) -> bool {
        self.lock_state().watch_routes_initialized
    }

    pub fn next_dirty_route_due_in_ms(&self, now_ms: u64) -> Option<u64> {
        self.lock_state()
            .dirty_routes
            .next_due_at_ms()
            .map(|due| due.saturating_sub(now_ms))
    }

    pub fn has_scheduled_route_work(&self) -> bool {
        self.lock_state().dirty_routes.next_due_at_ms().is_some()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn scheduled_route_ids_for_test(&self) -> BTreeSet<SourceRouteIdentity> {
        self.lock_state().dirty_routes.route_ids()
    }

    #[cfg(test)]
    pub fn set_route_event_watermark_for_test(
        &self,
        route: SourceRouteIdentity,
        watermark: EventWatermark,
    ) {
        self.lock_state()
            .route_event_watermarks
            .insert(route, watermark);
    }

    #[cfg(test)]
    pub fn route_event_watermark_for_test(
        &self,
        route: &SourceRouteIdentity,
    ) -> Option<EventWatermark> {
        self.lock_state().route_event_watermarks.get(route).copied()
    }

    /// Projects only durable retained-route missing grace back into the exact
    /// watcher ledger. Healthy routes never enter this safety path.
    pub fn schedule_pending_missing_route_rechecks(
        &self,
        data_root: &Path,
        watcher_watermark: EventWatermark,
        observed_at_ms: u64,
    ) -> Result<usize> {
        let Some(index) = open_published_generation(data_root, self.journal.as_ref())? else {
            return Ok(0);
        };
        let generation_id = index.generation_id().to_owned();
        let pending = index
            .manifest()
            .source_routes()
            .iter()
            .filter(|route| route.missing_state().is_some())
            .map(|route| route.route_identity().clone())
            .collect::<Vec<_>>();

        let mut state = self.lock_state();
        if !state.watch_routes_initialized {
            return Ok(0);
        }
        if state
            .current_published_generation
            .as_deref()
            .is_some_and(|current| current != generation_id.as_str())
        {
            // Publication advanced after this safety read. The next safety
            // pass will inspect its exact active manifest.
            return Ok(0);
        }
        let pending = pending
            .into_iter()
            .filter(|route| state.known_route_ids.contains(route))
            .collect::<Vec<_>>();
        let watermark = state.dirty_routes.seed_watermark().max(watcher_watermark);
        Ok(state
            .dirty_routes
            .seed_clean_exact_routes(pending, watermark, observed_at_ms))
    }

    pub fn enqueue_next_scheduled_refresh(&self, data_root: &Path, now_ms: u64) -> Result<bool> {
        self.enqueue_next_dirty_route_with_cold_all(data_root, now_ms, true)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn enqueue_next_dirty_route(&self, data_root: &Path, now_ms: u64) -> Result<bool> {
        self.enqueue_next_dirty_route_with_cold_all(data_root, now_ms, false)
    }

    fn enqueue_next_dirty_route_with_cold_all(
        &self,
        data_root: &Path,
        now_ms: u64,
        cold_all: bool,
    ) -> Result<bool> {
        let observed_generation = self.observed_published_generation(data_root)?;
        let request_id = {
            let mut state = self.lock_state();
            if durable_queue_entry_count(&state) != 0 {
                return Ok(false);
            }
            let routes = state
                .dirty_routes
                .due_routes(now_ms, SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT);
            if routes.is_empty() {
                return Ok(false);
            }
            let refresh_scope = if cold_all && observed_generation.is_none() {
                // A cold generation has no retained routes to carry. Publish
                // the complete startup inventory atomically instead of one
                // transient partial generation per initially dirty route.
                SourceBackedRefreshScope::All
            } else {
                SourceBackedRefreshScope::Exact(routes)
            };
            let attempt = new_refresh_attempt(
                observed_generation,
                SourceRefreshRuntimeMetadata::periodic(),
                None,
                refresh_scope,
            );
            let request_id = attempt.request_id.clone();
            state.active_request_id = Some(request_id.clone());
            state.attempts.push_back(attempt);
            trim_terminal_attempt_history(&mut state);
            request_id
        };
        self.persist_job_status(data_root, &request_id)?;
        Ok(true)
    }

    fn background_maintenance_wake_response(
        &self,
        data_root: &Path,
        request_id: String,
    ) -> Result<Value> {
        let published_generation = self.observed_published_generation(data_root)?;
        let metadata = self
            .runtime
            .metadata(data_root, SourceBackedRefreshOperation::Refresh);
        Ok(compact_json(json!({
            "ok": true,
            "schema_version": 1,
            "owner": "daemon",
            "request_id": request_id,
            "logical_request_id": request_id,
            "request_state": "queued",
            "logical_phase": "waiting",
            "previous_generation": published_generation.clone(),
            "published_generation": published_generation,
            "progress": {
                "phase": "maintenance_wake",
                "completed_sources": 0,
                "total_sources": 0,
                "total_sources_known": false,
            },
            "daemon_mode": metadata.daemon_mode.as_str(),
            "trigger": metadata.trigger,
            "trigger_provenance": metadata.trigger_provenance,
            "maintenance_wake": true,
        })))
    }

    fn finish_route_admissions(
        &self,
        request_id: &str,
        publication_ready: bool,
        post_publication_fence: Option<&PostPublicationRouteCoverageFence>,
    ) -> RouteAdmissionFinish {
        let mut state = self.lock_state();
        Self::finish_route_admissions_locked(
            &mut state,
            request_id,
            publication_ready,
            post_publication_fence,
        )
    }

    fn finish_route_admissions_and_persist(
        &self,
        data_root: &Path,
        request_id: &str,
        publication_ready: bool,
        post_publication_fence: Option<&PostPublicationRouteCoverageFence>,
    ) -> Result<RouteAdmissionFinish> {
        let mut state = self.lock_state();
        let finish = Self::finish_route_admissions_locked(
            &mut state,
            request_id,
            publication_ready,
            post_publication_fence,
        );
        let job = durable_job_json(&state, &finish.durable_request_id).ok_or_else(|| {
            anyhow!(
                "source refresh request `{}` disappeared during route finalization",
                finish.durable_request_id
            )
        })?;
        if let Err(error) = self.write_status(data_root, &job) {
            if finish.durable_request_id != request_id {
                state.pending_terminal_persistence = Some(PendingTerminalPersistence {
                    request_id: finish.durable_request_id.clone(),
                    terminal_job: job,
                    outcome: PendingTerminalOutcome::Failed {
                        scheduler_retry: false,
                    },
                });
            }
            return Err(error);
        }
        Ok(finish)
    }

    fn finish_route_admissions_locked(
        state: &mut CoreRefreshEngineState,
        request_id: &str,
        publication_ready: bool,
        post_publication_fence: Option<&PostPublicationRouteCoverageFence>,
    ) -> RouteAdmissionFinish {
        let now_ms = source_route_ledger_now_ms();
        let admissions = state
            .route_admissions
            .remove(request_id)
            .unwrap_or_default();
        let retained_predecessor_event_watermarks =
            state.route_admission_watermarks.remove(request_id);
        if let Some(predecessor_event_watermarks) = retained_predecessor_event_watermarks.as_ref() {
            for continuation in state.manual_all_continuations.values_mut() {
                if continuation.predecessor_request_id == request_id {
                    continuation.predecessor_event_watermarks =
                        predecessor_event_watermarks.clone();
                }
            }
        }
        let predecessor_event_watermarks =
            retained_predecessor_event_watermarks.unwrap_or_default();
        let current_event_watermarks = state.route_event_watermarks.clone();
        let attempt = find_attempt(state, request_id).cloned();
        let route_results = attempt
            .as_ref()
            .and_then(|attempt| attempt.receipt.as_ref())
            .map(|receipt| {
                receipt
                    .route_results
                    .iter()
                    .map(|result| (result.route_identity.as_str(), result))
                    .collect::<BTreeMap<_, _>>()
            });
        let mut covered_route_results = BTreeMap::new();
        let mut certified_routes = BTreeMap::new();
        for admission in admissions {
            let terminal_failed = !publication_ready
                || attempt
                    .as_ref()
                    .is_none_or(|attempt| attempt.state != SourceBackedRefreshState::Published);
            if terminal_failed {
                let blocked = attempt
                    .as_ref()
                    .and_then(|attempt| attempt.failure_outcome.as_ref())
                    .is_some_and(|outcome| outcome.blocked_routes.contains(admission.route()));
                if blocked {
                    state.dirty_routes.permanent_failure(&admission);
                } else {
                    state.dirty_routes.retryable_failure(&admission, now_ms);
                }
                continue;
            }
            let Some(result) = route_results
                .as_ref()
                .and_then(|results| results.get(admission.route().as_str()))
                .copied()
            else {
                state.dirty_routes.retryable_failure(&admission, now_ms);
                continue;
            };
            if let Some(retryable) = source_backed_route_retry_disposition(result) {
                if retryable {
                    state.dirty_routes.retryable_failure(&admission, now_ms);
                } else {
                    state.dirty_routes.permanent_failure(&admission);
                }
                continue;
            }
            if result.outcome.is_success() {
                let verified_boundary = attempt.as_ref().and_then(|attempt| {
                    let observation = attempt.route_observations.get(admission.route())?;
                    let admitted_watermark = predecessor_event_watermarks
                        .get(admission.route())
                        .copied()?;
                    let published_generation = attempt.published_generation.as_deref()?;
                    let covered_through =
                        post_publication_fence.map_or(admitted_watermark, |fence| {
                            fence.certified_boundary(
                                admission.route(),
                                admitted_watermark,
                                observation,
                            )
                        });
                    VerifiedSourceRefreshRouteBoundary::new(
                        request_id,
                        published_generation,
                        admission.route(),
                        covered_through,
                        observation,
                    )
                    .map(|boundary| (boundary, observation.clone()))
                });
                let acknowledged = match verified_boundary.as_ref() {
                    Some((boundary, _)) => state
                        .dirty_routes
                        .acknowledge_generation_coverage(&admission, boundary),
                    None => state.dirty_routes.acknowledge(&admission),
                };
                if acknowledged {
                    covered_route_results.insert(admission.route().clone(), result.clone());
                    if let Some((boundary, observation)) = verified_boundary {
                        certified_routes.insert(
                            admission.route().clone(),
                            SourceBackedRefreshRouteCoverageCertificate {
                                observation,
                                admitted_watermark: boundary.covered_through(),
                            },
                        );
                    }
                }
            } else {
                state.dirty_routes.retryable_failure(&admission, now_ms);
            }
        }
        if attempt
            .as_ref()
            .is_some_and(|attempt| attempt.state == SourceBackedRefreshState::Failed)
        {
            let durable_request_id = Self::terminalize_failed_predecessor_demands(
                state,
                request_id,
                attempt.as_ref().expect("failed predecessor snapshot"),
            )
            .unwrap_or_else(|| request_id.to_owned());
            if attempt
                .as_ref()
                .and_then(|attempt| attempt.failure_outcome.as_ref())
                .is_some_and(|outcome| !outcome.affected_routes.is_empty())
                && state.pending_scheduler_retry_root_id.as_deref() == Some(request_id)
            {
                state.pending_scheduler_retry_root_id = None;
            }
            return RouteAdmissionFinish {
                coverage_certificate: None,
                durable_request_id,
            };
        }
        for continuation in state.manual_all_continuations.values_mut() {
            if continuation.predecessor_request_id != request_id {
                continue;
            }
            continuation.predecessor_finished = true;
            if let Some(attempt) = attempt.as_ref() {
                if let Some(receipt) = attempt.receipt.as_ref() {
                    for (route, admission_observation) in &continuation.admission_route_observations
                    {
                        let covered = !continuation.invalidated_routes.contains(route)
                            && continuation.admission_event_watermarks.get(route)
                                == continuation.predecessor_event_watermarks.get(route)
                            && admission_observation.as_ref().is_some_and(|admitted| {
                                attempt.route_observations.get(route) == Some(admitted)
                                    && receipt.route_results.iter().any(|result| {
                                        result.route_identity == route.as_str()
                                            && source_backed_route_retry_disposition(result)
                                                .is_none()
                                    })
                            });
                        if covered {
                            if let Some(result) = receipt
                                .route_results
                                .iter()
                                .find(|result| result.route_identity == route.as_str())
                            {
                                continuation
                                    .covered_route_results
                                    .insert(route.clone(), result.clone());
                            }
                        }
                    }
                }
            }
            if covered_route_results.is_empty() {
                if continuation.covered_route_results.is_empty() {
                    continue;
                }
            } else {
                for (route, result) in &covered_route_results {
                    if continuation.invalidated_routes.contains(route) {
                        continue;
                    }
                    if attempt
                        .as_ref()
                        .is_some_and(|attempt| attempt.route_observations.contains_key(route))
                    {
                        // A predecessor with a provider-certified observation
                        // cannot be covered by a later indeterminate sample.
                        // The ledger path is only for route kinds that were
                        // indeterminate throughout the same successful pass.
                        continue;
                    }
                    if continuation
                        .admission_route_observations
                        .contains_key(route)
                        && !continuation.ledger_eligible_routes.contains(route)
                    {
                        continue;
                    }
                    // Legacy watcher-ledger admissions are a second exact
                    // coverage proof for routes outside the catalog-derived
                    // fence. Keep them in the durable logical demand, but do
                    // not let them override an indeterminate or mismatched
                    // catalog observation for the same route.
                    continuation
                        .admission_route_observations
                        .insert(route.clone(), None);
                    if let Some(watermark) = current_event_watermarks.get(route).copied() {
                        continuation
                            .admission_event_watermarks
                            .insert(route.clone(), watermark);
                    }
                    if let Some(watermark) = predecessor_event_watermarks.get(route).copied() {
                        continuation
                            .predecessor_event_watermarks
                            .insert(route.clone(), watermark);
                    }
                    if continuation.admission_event_watermarks.get(route)
                        != continuation.predecessor_event_watermarks.get(route)
                    {
                        continue;
                    }
                    continuation
                        .covered_route_results
                        .insert(route.clone(), result.clone());
                }
            }
            continuation.covered_removed_source_count = attempt
                .as_ref()
                .and_then(|attempt| attempt.receipt.as_ref())
                .map(|receipt| receipt.current.removed_source_count)
                .unwrap_or_default();
            continuation.covered_timings = attempt
                .as_ref()
                .and_then(|attempt| attempt.timings)
                .unwrap_or_default();
        }
        let coverage_certificate = attempt
            .filter(|attempt| {
                publication_ready && attempt.state == SourceBackedRefreshState::Published
            })
            .and_then(|attempt| {
                Some(SourceBackedRefreshCoverageCertificate {
                    request_id: request_id.to_owned(),
                    published_generation: attempt.published_generation.clone()?,
                    routes: certified_routes,
                })
            });
        RouteAdmissionFinish {
            coverage_certificate,
            durable_request_id: request_id.to_owned(),
        }
    }

    fn terminalize_failed_predecessor_demands(
        state: &mut CoreRefreshEngineState,
        predecessor_request_id: &str,
        predecessor: &SourceBackedRefreshAttempt,
    ) -> Option<String> {
        let dependent_request_ids = state
            .manual_all_continuations
            .iter()
            .filter(|(_, continuation)| {
                continuation.predecessor_request_id == predecessor_request_id
            })
            .map(|(request_id, _)| request_id.clone())
            .collect::<Vec<_>>();
        debug_assert!(
            dependent_request_ids.len() <= 1,
            "one predecessor must have at most one exact broad successor"
        );
        let mut durable_request_id = None;
        for request_id in dependent_request_ids {
            let fallback_routes = find_attempt(state, &request_id)
                .and_then(|attempt| match &attempt.refresh_scope {
                    SourceBackedRefreshScope::All => None,
                    SourceBackedRefreshScope::Exact(routes) => Some(routes.clone()),
                })
                .unwrap_or_default();
            let failure_outcome = predecessor.failure_outcome.clone().unwrap_or_else(|| {
                SourceBackedRefreshFailureOutcome::new(
                    "source_refresh_failed",
                    "internal",
                    true,
                    fallback_routes,
                    Some("retry_request"),
                )
            });
            if let Some(logical) = find_attempt_mut(state, &request_id) {
                logical.state = SourceBackedRefreshState::Failed;
                logical.finished_at_ms = predecessor
                    .finished_at_ms
                    .or_else(|| Some(utc_now().timestamp_millis()));
                logical.published_generation = predecessor.published_generation.clone();
                logical.progress = predecessor.progress.clone();
                logical.progress.phase = "failed".to_owned();
                logical.progress_total_sources_known = predecessor.progress_total_sources_known;
                logical.physical_attempt_id = Some(predecessor_request_id.to_owned());
                logical.failure_type = predecessor.failure_type;
                logical.failure_outcome = Some(failure_outcome);
                logical.last_error = predecessor.last_error.as_ref().map(|detail| {
                    format!("physical predecessor `{predecessor_request_id}` failed: {detail}")
                });
            }
            state.manual_all_continuations.remove(&request_id);
            state.admission_resolutions_in_flight.remove(&request_id);
            state.unacknowledged_admissions.remove(&request_id);
            state.route_admissions.remove(&request_id);
            state.route_admission_watermarks.remove(&request_id);
            state
                .pending_request_ids
                .retain(|pending| pending != &request_id);
            if state.active_request_id.as_deref() == Some(request_id.as_str()) {
                state.active_request_id = state.pending_request_ids.pop_front();
            }
            durable_request_id.get_or_insert(request_id);
        }
        durable_request_id
    }

    fn restore_route_dispositions_locked(
        state: &mut CoreRefreshEngineState,
        retryable_routes: &BTreeSet<SourceRouteIdentity>,
        blocked_routes: &BTreeSet<SourceRouteIdentity>,
    ) {
        let routes = retryable_routes
            .union(blocked_routes)
            .cloned()
            .collect::<BTreeSet<_>>();
        if routes.is_empty() {
            return;
        }
        let now_ms = source_route_ledger_now_ms();
        let watermark = state.dirty_routes.seed_watermark();
        for route in &routes {
            state
                .route_event_watermarks
                .entry(route.clone())
                .and_modify(|current| *current = (*current).max(watermark))
                .or_insert(watermark);
        }
        state
            .dirty_routes
            .seed_exact_routes(routes, watermark, now_ms);
        state.dirty_routes.block_exact_routes(blocked_routes.iter());
    }

    pub(super) fn lock_state(&self) -> std::sync::MutexGuard<'_, CoreRefreshEngineState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
}
