use super::*;
use crate::semantic::dirty_source_routes::{
    DirtySourceRouteAdmission, DirtySourceRoutes, EventWatermark,
};

mod attempt_helpers;
mod durable_queue;
mod generation_authority;
mod generation_observation;
mod progress_model;
mod read_model;
mod request_lifecycle;
mod runtime_metadata;
mod startup_observation;
use attempt_helpers::*;
use durable_queue::{
    durable_job_json, install_recovered_successors, job_with_queued_successors,
    recover_queued_root, recover_queued_successors,
};
use generation_authority::CoreRefreshTerminalSuccess;
pub(crate) use generation_authority::PinnedCorePublication;
use progress_model::SourceBackedRefreshState;
pub(crate) use progress_model::{
    SourceBackedCurrentSourceProgress, SourceBackedCurrentSourceProgressStage,
    SourceBackedRefreshProgress, SourceBackedRefreshTimings,
};
use read_model::SourceBackedRefreshAttempt;
pub(super) use read_model::{refresh_scope_from_json, refresh_scope_json};
pub(crate) use read_model::{
    SourceBackedRefreshCatalogRouteOutcome, SourceBackedRefreshReceipt,
    SourceBackedRefreshRecordRejection, SourceBackedRefreshRouteOutcome,
    SourceBackedRefreshRouteResult, SourceBackedRefreshSourceFailure,
};
use runtime_metadata::{
    source_catalog_refresh_runtime_metadata, source_refresh_runtime_metadata,
    SourceRefreshRuntimeMetadata,
};
use startup_observation::startup_routes_requiring_refresh;

pub(super) struct SourceBackedRefreshProgressUpdate {
    pub(super) phase: String,
    pub(super) completed_sources: usize,
    pub(super) total_sources: usize,
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
pub(crate) struct SourceBackedRefreshExecution<'a> {
    pub(crate) data_root: &'a Path,
    pub(crate) index_root: &'a Path,
    pub(crate) request_id: &'a str,
    pub(super) operation: SourceBackedRefreshOperation,
    pub(crate) explicit_source_catalog: Option<&'a ExplicitSourceCatalogAuthority>,
    pub(crate) scope: SourceBackedRefreshScope,
    pub(crate) covered_route_ids: BTreeSet<SourceRouteIdentity>,
    pub(crate) covered_publication: SourceBackedRefreshCoveredPublication,
    pub(super) report_progress: &'a dyn Fn(SourceBackedRefreshProgressUpdate) -> Result<()>,
}

impl SourceBackedRefreshExecution<'_> {
    pub(crate) fn report_progress(
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
    pub(crate) fn report_detailed_progress(
        &self,
        phase: &str,
        completed_sources: usize,
        total_sources: usize,
        current_source: Option<String>,
        completed_records: Option<u64>,
        completed_bytes: Option<u64>,
        current_source_progress: Option<SourceBackedCurrentSourceProgress>,
    ) -> Result<()> {
        (self.report_progress)(SourceBackedRefreshProgressUpdate {
            phase: phase.to_owned(),
            completed_sources,
            total_sources,
            current_source,
            completed_records,
            completed_bytes,
            current_source_progress,
        })
    }
}

/// Provider-neutral callback boundary for one daemon-serialized refresh.
pub(crate) trait SourceBackedRefreshExecutor: Send + Sync {
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
    route_admissions: BTreeMap<String, Vec<DirtySourceRouteAdmission>>,
    manual_all_continuations: BTreeMap<String, ManualAllContinuation>,
    pending_terminal_persistence: Option<PendingTerminalPersistence>,
    pending_scheduler_retry_root_id: Option<String>,
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
    Failed,
}

impl PendingTerminalPersistence {
    fn did_work(&self) -> bool {
        matches!(
            self.outcome,
            PendingTerminalOutcome::Published { did_work: true, .. }
        )
    }

    fn failed(&self) -> bool {
        matches!(self.outcome, PendingTerminalOutcome::Failed)
    }
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

#[derive(Debug, Clone)]
struct ManualAllContinuation {
    predecessor_request_id: String,
    predecessor_finished: bool,
    covered_route_results: BTreeMap<SourceRouteIdentity, SourceBackedRefreshRouteResult>,
    covered_removed_source_count: usize,
    covered_timings: SourceBackedRefreshTimings,
}

impl ManualAllContinuation {
    fn new(predecessor_request_id: String) -> Self {
        Self {
            predecessor_request_id,
            predecessor_finished: false,
            covered_route_results: BTreeMap::new(),
            covered_removed_source_count: 0,
            covered_timings: SourceBackedRefreshTimings::default(),
        }
    }

    fn invalidate_route(&mut self, route: &SourceRouteIdentity) {
        if self.covered_route_results.remove(route).is_some()
            && self.covered_route_results.is_empty()
        {
            self.covered_removed_source_count = 0;
            self.covered_timings = SourceBackedRefreshTimings::default();
        }
    }

    fn covered_publication(&self) -> SourceBackedRefreshCoveredPublication {
        SourceBackedRefreshCoveredPublication {
            route_results: self.covered_route_results.values().cloned().collect(),
            removed_source_count: self.covered_removed_source_count,
            timings: self.covered_timings,
        }
    }
}

type SourceRefreshStatusWriter = dyn Fn(&Path, &Value) -> Result<()> + Send + Sync;

pub(in crate::semantic) struct CoreRefreshEngine {
    state: Mutex<CoreRefreshEngineState>,
    pub(super) executor: Arc<dyn SourceBackedRefreshExecutor>,
    status_writer: Arc<SourceRefreshStatusWriter>,
}

pub(in crate::semantic) struct SourceBackedRefreshRun {
    pub(in crate::semantic) job: Value,
    pub(in crate::semantic) did_work: bool,
    pub(in crate::semantic) failed: bool,
    pub(in crate::semantic) terminal_persistence_pending: bool,
    pub(in crate::semantic) scope: SourceBackedRefreshScope,
}

#[derive(Debug)]
struct SourceBackedRefreshQueueFull {
    active_pending_requests: usize,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum SourceRefreshAdmissionRequirement {
    /// Observe the terminal result of equivalent work already admitted.
    AttachEquivalent,
    /// Require work whose admission occurs after the currently running attempt.
    FreshAfterAdmittedSnapshot,
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

impl CoreRefreshEngine {
    pub(in crate::semantic) fn new() -> Self {
        Self::with_executor(Arc::new(CaptureOwnedSourceBackedRefreshExecutor))
    }

    pub(in crate::semantic) fn with_executor(
        executor: Arc<dyn SourceBackedRefreshExecutor>,
    ) -> Self {
        Self::with_executor_and_status_writer(executor, Arc::new(write_daemon_job_status))
    }

    fn with_executor_and_status_writer(
        executor: Arc<dyn SourceBackedRefreshExecutor>,
        status_writer: Arc<SourceRefreshStatusWriter>,
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
                route_admissions: BTreeMap::new(),
                manual_all_continuations: BTreeMap::new(),
                pending_terminal_persistence: None,
                pending_scheduler_retry_root_id: None,
                watch_routes_initialized: false,
            }),
            executor,
            status_writer,
        }
    }

    #[cfg(test)]
    pub(in crate::semantic) fn with_status_writer_for_test(
        executor: Arc<dyn SourceBackedRefreshExecutor>,
        status_writer: Arc<SourceRefreshStatusWriter>,
    ) -> Self {
        Self::with_executor_and_status_writer(executor, status_writer)
    }

    pub(in crate::semantic) fn has_pending_request(&self) -> bool {
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

    pub(in crate::semantic) fn initialize_watch_route_authority(
        &self,
        routes: impl IntoIterator<Item = SourceRouteIdentity>,
    ) {
        let routes = routes.into_iter().collect::<BTreeSet<_>>();
        let mut state = self.lock_state();
        state.dirty_routes.retain_exact_routes(&routes);
        for continuation in state.manual_all_continuations.values_mut() {
            for route in &routes {
                continuation.invalidate_route(route);
            }
        }
        state.known_route_ids = routes;
        state.watch_routes_initialized = true;
    }

    #[cfg(test)]
    pub(in crate::semantic) fn reconcile_watch_routes(
        &self,
        routes: impl IntoIterator<Item = SourceRouteIdentity>,
        watermark: EventWatermark,
        observed_at_ms: u64,
    ) {
        let routes = routes.into_iter().collect::<BTreeSet<_>>();
        self.initialize_watch_route_authority(routes.iter().cloned());
        self.schedule_startup_route_reconciliation(routes, watermark, observed_at_ms);
    }

    pub(in crate::semantic) fn schedule_startup_route_reconciliation(
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
        state
            .dirty_routes
            .seed_exact_routes(routes, watermark, observed_at_ms);
    }

    /// Performs the bounded provider-neutral startup preflight. The watcher is
    /// already active when this runs. Only a generation-bound exact
    /// `Unchanged` observation stays clean; every other route enters the
    /// normal fail-closed refresh path.
    pub(in crate::semantic) fn schedule_startup_route_observation(
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
        state
            .dirty_routes
            .seed_exact_routes(dirty, watermark, observed_at_ms);
    }

    pub(in crate::semantic) fn record_watch_routes(
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
                    for continuation in state.manual_all_continuations.values_mut() {
                        continuation.invalidate_route(&route);
                    }
                }
            }
        }
    }

    pub(in crate::semantic) fn watch_routes_initialized(&self) -> bool {
        self.lock_state().watch_routes_initialized
    }

    pub(in crate::semantic) fn next_dirty_route_due_in_ms(&self, now_ms: u64) -> Option<u64> {
        self.lock_state()
            .dirty_routes
            .next_due_at_ms()
            .map(|due| due.saturating_sub(now_ms))
    }

    pub(in crate::semantic) fn has_scheduled_route_work(&self) -> bool {
        self.lock_state().dirty_routes.next_due_at_ms().is_some()
    }

    #[cfg(test)]
    pub(in crate::semantic) fn scheduled_route_ids_for_test(
        &self,
    ) -> BTreeSet<SourceRouteIdentity> {
        self.lock_state().dirty_routes.route_ids()
    }

    /// Projects only durable retained-route missing grace back into the exact
    /// watcher ledger. Healthy routes never enter this safety path.
    pub(in crate::semantic) fn schedule_pending_missing_route_rechecks(
        &self,
        data_root: &Path,
        watcher_watermark: EventWatermark,
        observed_at_ms: u64,
    ) -> Result<usize> {
        let Some(index) = open_published_generation(data_root)? else {
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

    pub(in crate::semantic) fn enqueue_next_scheduled_refresh(
        &self,
        data_root: &Path,
        now_ms: u64,
    ) -> Result<bool> {
        self.enqueue_next_dirty_route_with_cold_all(data_root, now_ms, true)
    }

    #[cfg(test)]
    pub(in crate::semantic) fn enqueue_next_dirty_route(
        &self,
        data_root: &Path,
        now_ms: u64,
    ) -> Result<bool> {
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

    pub(in crate::semantic) fn handle_ipc_request(
        &self,
        data_root: &Path,
        request: &Value,
    ) -> Result<Option<Value>> {
        match request.get("op").and_then(Value::as_str) {
            Some(SOURCE_REFRESH_REQUEST_OP) => {
                let mode = request.get("mode").and_then(Value::as_str).unwrap_or("");
                if !matches!(mode, "background" | "wait") {
                    return Err(anyhow!("invalid daemon source refresh mode `{mode}`"));
                }
                let operation = SourceBackedRefreshOperation::from_request_json(request)?;
                let explicit_catalog = request.get("explicit_source_catalog");
                match (operation, mode, explicit_catalog) {
                    (SourceBackedRefreshOperation::Refresh, _, Some(_)) => {
                        return Err(anyhow!(
                            "refresh operation cannot carry explicit source catalog authority"
                        ))
                    }
                    (SourceBackedRefreshOperation::Import, "background", _) => {
                        return Err(anyhow!(
                            "import operation requires daemon refresh mode `wait`"
                        ))
                    }
                    (SourceBackedRefreshOperation::Import, _, None) => {
                        return Err(anyhow!(
                            "import operation requires explicit source catalog authority"
                        ))
                    }
                    _ => {}
                }
                if operation == SourceBackedRefreshOperation::Refresh && mode == "background" {
                    return Ok(Some(self.background_maintenance_wake_response(data_root)?));
                }
                let requested_catalog = explicit_catalog
                    .map(ExplicitSourceCatalogAuthority::from_json)
                    .transpose()?;
                let admission = match request.get("fresh_after_admitted_snapshot") {
                    None | Some(Value::Bool(false)) => {
                        SourceRefreshAdmissionRequirement::AttachEquivalent
                    }
                    Some(Value::Bool(true)) => {
                        SourceRefreshAdmissionRequirement::FreshAfterAdmittedSnapshot
                    }
                    Some(_) => {
                        return Err(anyhow!(
                            "daemon source refresh fresh-after-admitted-snapshot requirement must be boolean"
                        ))
                    }
                };
                let previous_generation = self.observed_published_generation(data_root)?;
                let metadata = match operation {
                    SourceBackedRefreshOperation::Import => {
                        source_catalog_refresh_runtime_metadata(data_root)
                    }
                    SourceBackedRefreshOperation::Refresh => {
                        source_refresh_runtime_metadata(data_root)
                    }
                };
                let response = match self.enqueue_with_catalog_metadata(
                    previous_generation,
                    metadata,
                    requested_catalog,
                    SourceBackedRefreshScope::All,
                    // Wait controls how the client observes the attempt; it is
                    // not itself a fresh-after-admission barrier.
                    admission,
                ) {
                    Ok(response) => response,
                    Err(error) => {
                        if let Some(queue_full) =
                            error.downcast_ref::<SourceBackedRefreshQueueFull>()
                        {
                            return Ok(Some(queue_full.to_json()));
                        }
                        return Err(error);
                    }
                };
                let request_id = response
                    .get("request_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("queued source refresh has no request ID"))?;
                self.persist_job_status(data_root, request_id)?;
                Ok(Some(response))
            }
            Some(SOURCE_REFRESH_STATUS_OP) => {
                let request_id = request
                    .get("request_id")
                    .and_then(Value::as_str)
                    .filter(|request_id| !request_id.is_empty())
                    .ok_or_else(|| anyhow!("daemon source refresh request ID is missing"))?;
                let status = self
                    .status(request_id)
                    .unwrap_or_else(|| unknown_refresh_request_response(request_id));
                Ok(Some(status))
            }
            _ => Ok(None),
        }
    }

    fn background_maintenance_wake_response(&self, data_root: &Path) -> Result<Value> {
        let published_generation = self.observed_published_generation(data_root)?;
        let metadata = source_refresh_runtime_metadata(data_root);
        Ok(compact_json(json!({
            "ok": true,
            "schema_version": 1,
            "owner": "daemon",
            "request_id": Uuid::now_v7().to_string(),
            "request_state": "queued",
            "previous_generation": published_generation.clone(),
            "published_generation": published_generation,
            "progress": {
                "phase": "maintenance_wake",
                "completed_sources": 0,
                "total_sources": 0,
            },
            "daemon_mode": metadata.daemon_mode.as_str(),
            "trigger": metadata.trigger,
            "trigger_provenance": metadata.trigger_provenance,
            "maintenance_wake": true,
        })))
    }

    fn finish_route_admissions(&self, request_id: &str, publication_ready: bool) {
        let now_ms = source_route_ledger_now_ms();
        let mut state = self.lock_state();
        let Some(admissions) = state.route_admissions.remove(request_id) else {
            for continuation in state.manual_all_continuations.values_mut() {
                if continuation.predecessor_request_id == request_id {
                    continuation.predecessor_finished = true;
                }
            }
            return;
        };
        let attempt = find_attempt(&state, request_id).cloned();
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
        for admission in admissions {
            let retry = !publication_ready
                || attempt
                    .as_ref()
                    .is_none_or(|attempt| attempt.state != SourceBackedRefreshState::Published);
            if retry {
                state.dirty_routes.retryable_failure(&admission, now_ms);
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
            if let Some(failure) = result.outcome.failure_class() {
                match failure {
                    "unavailable" | "source_changed" => {
                        state.dirty_routes.retryable_failure(&admission, now_ms);
                    }
                    "unreadable" | "incompatible" => {
                        state.dirty_routes.permanent_failure(&admission);
                    }
                    _ => {
                        state.dirty_routes.retryable_failure(&admission, now_ms);
                    }
                }
            } else if result.outcome.is_success() {
                if state.dirty_routes.acknowledge(&admission) {
                    covered_route_results.insert(admission.route().clone(), result.clone());
                }
            } else {
                state.dirty_routes.retryable_failure(&admission, now_ms);
            }
        }
        for continuation in state.manual_all_continuations.values_mut() {
            if continuation.predecessor_request_id != request_id {
                continue;
            }
            continuation.predecessor_finished = true;
            if covered_route_results.is_empty() {
                continue;
            }
            continuation
                .covered_route_results
                .extend(covered_route_results.clone());
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
    }

    pub(super) fn lock_state(&self) -> std::sync::MutexGuard<'_, CoreRefreshEngineState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
}
