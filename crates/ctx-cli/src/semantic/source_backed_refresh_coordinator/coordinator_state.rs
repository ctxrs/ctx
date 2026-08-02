use super::*;
use crate::semantic::dirty_source_routes::{
    DirtySourceRouteAdmission, DirtySourceRoutes, EventWatermark,
};

mod generation_authority;
mod generation_observation;
mod read_model;
mod request_lifecycle;
mod route_frontier;
mod runtime_metadata;
use generation_authority::CoreRefreshTerminalSuccess;
pub(crate) use generation_authority::PinnedCorePublication;
use read_model::{
    SourceBackedRefreshAttempt, SourceBackedRefreshProgress, SourceBackedRefreshState,
};
pub(crate) use read_model::{
    SourceBackedRefreshReceipt, SourceBackedRefreshSourceFailure, SourceBackedRefreshTimings,
};
use route_frontier::RouteFreshnessFrontier;
use runtime_metadata::{
    source_catalog_refresh_runtime_metadata, source_refresh_runtime_metadata,
    SourceRefreshRuntimeMetadata,
};

pub(super) struct SourceBackedRefreshProgressUpdate {
    pub(super) phase: String,
    pub(super) completed_sources: usize,
    pub(super) total_sources: usize,
    pub(super) current_source: Option<String>,
    pub(super) completed_records: Option<u64>,
    pub(super) completed_bytes: Option<u64>,
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
    pub(crate) explicit_source_catalog: Option<&'a ExplicitSourceCatalogAuthority>,
    pub(crate) scope: SourceBackedRefreshScope,
    pub(crate) covered_route_ids: BTreeSet<SourceRouteIdentity>,
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
        (self.report_progress)(SourceBackedRefreshProgressUpdate {
            phase: phase.to_owned(),
            completed_sources,
            total_sources,
            current_source,
            completed_records,
            completed_bytes,
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
    watch_routes_initialized: bool,
    route_freshness_frontier: Option<RouteFreshnessFrontier>,
}

#[derive(Debug, Clone)]
struct ManualAllContinuation {
    predecessor_request_id: String,
    predecessor_finished: bool,
    covered_route_ids: BTreeSet<SourceRouteIdentity>,
    covered_scanned_routes: usize,
    covered_removed_source_count: usize,
    covered_timings: SourceBackedRefreshTimings,
}

impl ManualAllContinuation {
    fn new(predecessor_request_id: String) -> Self {
        Self {
            predecessor_request_id,
            predecessor_finished: false,
            covered_route_ids: BTreeSet::new(),
            covered_scanned_routes: 0,
            covered_removed_source_count: 0,
            covered_timings: SourceBackedRefreshTimings::default(),
        }
    }

    fn invalidate_route(&mut self, route: &SourceRouteIdentity) {
        if self.covered_route_ids.remove(route) && self.covered_route_ids.is_empty() {
            self.covered_scanned_routes = 0;
            self.covered_removed_source_count = 0;
            self.covered_timings = SourceBackedRefreshTimings::default();
        }
    }
}

pub(in crate::semantic) struct CoreRefreshEngine {
    state: Mutex<CoreRefreshEngineState>,
    pub(super) executor: Arc<dyn SourceBackedRefreshExecutor>,
}

pub(in crate::semantic) struct SourceBackedRefreshRun {
    pub(in crate::semantic) job: Value,
    pub(in crate::semantic) did_work: bool,
    pub(in crate::semantic) failed: bool,
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
                watch_routes_initialized: false,
                route_freshness_frontier: None,
            }),
            executor,
        }
    }

    pub(in crate::semantic) fn has_pending_request(&self) -> bool {
        let state = self.lock_state();
        state
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
        self.lock_state()
            .dirty_routes
            .seed_exact_routes(routes, watermark, observed_at_ms);
    }

    pub(in crate::semantic) fn record_watch_routes(
        &self,
        routes: impl IntoIterator<Item = (SourceRouteIdentity, EventWatermark)>,
        observed_at_ms: u64,
    ) {
        let mut state = self.lock_state();
        let mut recorded_routes = BTreeSet::new();
        for (route, watermark) in routes {
            if state.known_route_ids.contains(&route) {
                let recorded =
                    state
                        .dirty_routes
                        .record_event(route.clone(), watermark, observed_at_ms);
                if recorded {
                    recorded_routes.insert(route.clone());
                    for continuation in state.manual_all_continuations.values_mut() {
                        continuation.invalidate_route(&route);
                    }
                }
            }
        }
        let frontier = state.route_freshness_frontier.clone();
        drop(state);
        if let Some(frontier) = frontier {
            frontier.observe_routes(recorded_routes.iter());
        }
    }

    /// Performs the daemon's bounded startup comparison between exact watch
    /// targets and the route snapshots certified by the active Core
    /// generation. Changed or uncertifiable routes enter the normal exact
    /// dirty-route scheduler; matching routes remain clean.
    pub(in crate::semantic) fn reconcile_route_freshness_frontier(
        &self,
        data_root: &Path,
        catalog: &SourceBackedWatchCatalog,
        watcher_watermark: EventWatermark,
        observed_at_ms: u64,
    ) -> Result<usize> {
        let (published, open_warning) = match open_published_generation(data_root) {
            Ok(published) => (published, None),
            Err(error) => (
                None,
                Some(format!(
                    "open active Core publication for route freshness reconciliation: {error:#}"
                )),
            ),
        };
        let reconciliation =
            RouteFreshnessFrontier::reconcile(data_root, catalog, published.as_ref());
        let dirty_count = reconciliation.dirty_routes.len();
        let mut state = self.lock_state();
        state.route_freshness_frontier = Some(reconciliation.frontier);
        let known_dirty = reconciliation
            .dirty_routes
            .into_iter()
            .filter(|route| state.known_route_ids.contains(route))
            .collect::<Vec<_>>();
        let watermark = state.dirty_routes.seed_watermark().max(watcher_watermark);
        state
            .dirty_routes
            .seed_clean_exact_routes(known_dirty, watermark, observed_at_ms);
        drop(state);

        let warnings = open_warning
            .into_iter()
            .chain(reconciliation.warning)
            .collect::<Vec<_>>();
        if warnings.is_empty() {
            Ok(dirty_count)
        } else {
            Err(anyhow!(warnings.join("; ")))
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
        let catalog = load_explicit_source_catalog_authority(data_root)?;
        let request_id = {
            let mut state = self.lock_state();
            if active_attempt_count(&state) != 0 {
                return Ok(false);
            }
            let Some(route) = state.dirty_routes.next_due_route(now_ms) else {
                return Ok(false);
            };
            let refresh_scope = if cold_all && observed_generation.is_none() {
                // A cold generation has no retained routes to carry. Publish
                // the complete startup inventory atomically instead of one
                // transient partial generation per initially dirty route.
                SourceBackedRefreshScope::All
            } else {
                SourceBackedRefreshScope::exact([route])
            };
            let attempt = new_refresh_attempt(
                observed_generation,
                SourceRefreshRuntimeMetadata::periodic(),
                Some(catalog),
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
                    .transpose()?
                    .map_or_else(|| load_explicit_source_catalog_authority(data_root), Ok)?;
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
                    Some(requested_catalog),
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

    fn run_next_with_terminal_success<Execute, Probe, Terminal, Published, Failed>(
        &self,
        execute: Execute,
        probe: Probe,
        terminal: Terminal,
        published: Published,
        failed: Failed,
    ) -> Option<SourceBackedRefreshRun>
    where
        Execute: FnOnce(&str, &Self) -> Result<SourceBackedRefreshPublication>,
        Probe: FnOnce() -> Result<Option<String>>,
        Terminal: FnOnce(SourceBackedRefreshReceipt) -> Result<CoreRefreshTerminalSuccess>,
        Published: FnOnce(&str) -> Result<()>,
        Failed: FnOnce(&str) -> Result<()>,
    {
        let (request_id, previous_generation, requested_catalog, refresh_scope) = {
            let mut state = self.lock_state();
            let request_id = state.active_request_id.clone()?;
            if state
                .manual_all_continuations
                .get(&request_id)
                .is_some_and(|continuation| !continuation.predecessor_finished)
            {
                return None;
            }
            let attempt = find_attempt_mut(&mut state, &request_id)?;
            if attempt.state != SourceBackedRefreshState::Queued {
                return None;
            }
            attempt.state = SourceBackedRefreshState::Running;
            attempt.started_at_ms = Some(utc_now().timestamp_millis());
            attempt.progress.phase = "starting".to_owned();
            (
                request_id,
                attempt.previous_generation.clone(),
                attempt.requested_explicit_source_catalog.clone(),
                attempt.refresh_scope.clone(),
            )
        };

        let execution = execute(&request_id, self);
        let execution_failure_type = execution
            .as_ref()
            .err()
            .and_then(source_backed_refresh_failure_type);
        let observed_generation = probe();
        let (verified, observed_for_status) = match (execution, observed_generation) {
            (Ok(publication), Ok(Some(observed))) if publication.generation_id == observed => {
                let catalog_matches_request = requested_catalog.as_ref().is_none_or(|requested| {
                    requested == &publication.published_explicit_source_catalog
                });
                let verified = if !catalog_matches_request {
                    Err(format!(
                        "source-backed refresh published generation {observed} with an explicit source catalog authority different from the requested authority"
                    ))
                } else {
                    Ok((observed.clone(), publication))
                };
                (verified, Some(observed))
            }
            (Ok(publication), Ok(observed)) => (Err(format!(
                "source-backed refresh returned generation {}, but the verified published generation is {observed:?}",
                publication.generation_id
            )), observed),
            (Ok(publication), Err(error)) => (
                Err(format!(
                    "source-backed refresh returned generation {}, but publication verification failed: {error:#}",
                    publication.generation_id
                )),
                None,
            ),
            (Err(error), Ok(observed)) => {
                (Err(source_backed_refresh_error_summary(&error)), observed)
            }
            (Err(error), Err(probe_error)) => (Err(format!(
                "{}; verifying the retained generation also failed: {probe_error:#}",
                source_backed_refresh_error_summary(&error)
            )), None),
        };
        let continuation = self
            .lock_state()
            .manual_all_continuations
            .get(&request_id)
            .cloned();
        let verified = match verified {
            Ok((observed, mut publication)) => {
                if let Some(continuation) = continuation.as_ref() {
                    aggregate_manual_all_continuation(&mut publication, continuation);
                }
                let receipt = SourceBackedRefreshReceipt::from_verified_publication(
                    previous_generation.clone(),
                    observed.clone(),
                    &publication,
                );
                terminal(receipt)
                    .map(|terminal| (observed, publication, terminal))
                    .map_err(|error| format!("finalize verified Core publication: {error:#}"))
            }
            Err(error) => Err(error),
        };
        let verified = match verified {
            Ok(verified) => Ok(verified),
            Err(error) => match failed(&error) {
                Ok(()) => Err(error),
                Err(record_error) => Err(format!(
                    "{error}; recording the resumable rebuild failure also failed: {record_error:#}"
                )),
            },
        };
        let mut state = self.lock_state();
        state.manual_all_continuations.remove(&request_id);
        let mut newly_published_generation = None;
        let (failed_run, did_work) = match verified {
            Ok((observed, publication, terminal)) => {
                let receipt = terminal.install(&mut state);
                let attempt = find_attempt_mut(&mut state, &request_id)?;
                attempt.finished_at_ms = Some(utc_now().timestamp_millis());
                attempt.progress.current_source = None;
                attempt.progress.completed_records = None;
                attempt.progress.completed_bytes = None;
                attempt.state = SourceBackedRefreshState::Published;
                attempt.published_generation = Some(observed.clone());
                attempt.progress.phase = "published".to_owned();
                attempt.progress.completed_sources = attempt.progress.total_sources;
                attempt.scanned_routes = Some(publication.scanned_routes);
                attempt.unsupported_routes = Some(publication.unsupported_routes);
                attempt.certified_source_count = Some(publication.certified_source_count);
                attempt.certified_source_bytes = Some(publication.certified_source_bytes);
                attempt.receipt = Some(receipt);
                attempt.timings = Some(publication.timings);
                attempt.published_explicit_source_catalog =
                    Some(publication.published_explicit_source_catalog);
                newly_published_generation = Some(observed);
                let did_work = attempt.published_generation != previous_generation;
                (false, did_work)
            }
            Err(error) => {
                let attempt = find_attempt_mut(&mut state, &request_id)?;
                attempt.finished_at_ms = Some(utc_now().timestamp_millis());
                attempt.progress.current_source = None;
                attempt.progress.completed_records = None;
                attempt.progress.completed_bytes = None;
                if observed_for_status.is_some() {
                    attempt.published_generation = observed_for_status.clone();
                }
                attempt.state = SourceBackedRefreshState::Failed;
                attempt.progress.phase = "failed".to_owned();
                attempt.failure_type = execution_failure_type;
                attempt.last_error = Some(error);
                (true, false)
            }
        };
        if newly_published_generation.is_some() {
            state.current_published_generation = newly_published_generation;
        }
        if state.active_request_id.as_deref() == Some(request_id.as_str()) {
            state.active_request_id = state.pending_request_ids.pop_front();
            if let Some(next_request_id) = state.active_request_id.clone() {
                let next_is_manual_continuation = state
                    .manual_all_continuations
                    .contains_key(&next_request_id);
                if let Some(next_attempt) = find_attempt_mut(&mut state, &next_request_id) {
                    if observed_for_status.is_some() && !next_is_manual_continuation {
                        next_attempt.previous_generation = observed_for_status.clone();
                        next_attempt.published_generation = observed_for_status.clone();
                    }
                }
            }
        }
        trim_terminal_attempt_history(&mut state);
        drop(state);

        if !failed_run {
            if let Err(error) = published(
                observed_for_status
                    .as_deref()
                    .expect("verified publication has an observed generation"),
            ) {
                let mut state = self.lock_state();
                if let Some(attempt) = find_attempt_mut(&mut state, &request_id) {
                    attempt.post_publication_error = Some(format!(
                        "finish retryable source-backed publication work: {error:#}"
                    ));
                }
            }
        }
        let job = self.job_status(&request_id)?;
        Some(SourceBackedRefreshRun {
            job,
            did_work,
            failed: failed_run,
            scope: refresh_scope,
        })
    }

    #[cfg(test)]
    pub(super) fn run_next_with<Execute, Probe, Published, Failed>(
        &self,
        execute: Execute,
        probe: Probe,
        published: Published,
        failed: Failed,
    ) -> Option<SourceBackedRefreshRun>
    where
        Execute: FnOnce(&str, &Self) -> Result<SourceBackedRefreshPublication>,
        Probe: FnOnce() -> Result<Option<String>>,
        Published: FnOnce(&str) -> Result<()>,
        Failed: FnOnce(&str) -> Result<()>,
    {
        self.run_next_with_terminal_success(
            execute,
            probe,
            |receipt| Ok(CoreRefreshTerminalSuccess::state_only(receipt)),
            published,
            failed,
        )
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
        let mut covered_route_ids = BTreeSet::new();
        for admission in admissions {
            let retry = !publication_ready
                || attempt
                    .as_ref()
                    .is_none_or(|attempt| attempt.state != SourceBackedRefreshState::Published);
            if retry {
                state.dirty_routes.retryable_failure(&admission, now_ms);
                continue;
            }
            let Some(receipt) = attempt
                .as_ref()
                .and_then(|attempt| attempt.receipt.as_ref())
            else {
                state.dirty_routes.retryable_failure(&admission, now_ms);
                continue;
            };
            if let Some(failure) = receipt
                .source_failures
                .iter()
                .find(|failure| failure.route_identity == admission.route().as_str())
            {
                match failure.class.as_str() {
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
            } else if receipt
                .successful_route_ids
                .iter()
                .any(|route| route == admission.route().as_str())
            {
                if state.dirty_routes.acknowledge(&admission) {
                    covered_route_ids.insert(admission.route().clone());
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
            if covered_route_ids.is_empty() {
                continue;
            }
            continuation
                .covered_route_ids
                .extend(covered_route_ids.iter().cloned());
            continuation.covered_scanned_routes = attempt
                .as_ref()
                .and_then(|attempt| attempt.scanned_routes)
                .unwrap_or_default();
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
        let frontier_publication = publication_ready
            .then(|| {
                state
                    .route_freshness_frontier
                    .clone()
                    .zip(state.pinned_core_publication.as_ref().map(Arc::clone))
            })
            .flatten();
        // Keep the dirty-ledger lock through durable frontier publication.
        // A watcher event arriving after acknowledgement must either wait and
        // leave this older candidate durable, or have invalidated the
        // admission before this point. It must never update the candidate in
        // the gap between acknowledgement and persistence.
        let frontier_error = frontier_publication.and_then(|(frontier, publication)| {
            frontier
                .publish_acknowledged_routes(publication.verified_index_ref(), &covered_route_ids)
                .err()
                .map(|error| (frontier.data_root(), error))
        });
        drop(state);
        if let Some((data_root, error)) = frontier_error {
            let _ =
                crate::semantic::daemon_wakeup::write_degraded_wakeup_receipt(&data_root, &error);
        }
    }

    pub(super) fn lock_state(&self) -> std::sync::MutexGuard<'_, CoreRefreshEngineState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
}

fn source_backed_refresh_failure_type(error: &anyhow::Error) -> Option<&'static str> {
    error.chain().find_map(|cause| {
        if let Some(route) = cause.downcast_ref::<SourceBackedRouteError>() {
            return match route.kind {
                SourceBackedRouteErrorKind::Unsupported => Some("unsupported_schema"),
                SourceBackedRouteErrorKind::InvalidSource => Some("malformed_source"),
                SourceBackedRouteErrorKind::Unavailable => Some("source_unavailable"),
                SourceBackedRouteErrorKind::SourceChanged => Some("source_changed"),
                SourceBackedRouteErrorKind::Internal => None,
            };
        }
        let SourceBackedCoordinatorError::NoUsableSourceRoutes { failed_routes } =
            cause.downcast_ref::<SourceBackedCoordinatorError>()?
        else {
            return None;
        };
        let first = failed_routes.first()?.class;
        if failed_routes.iter().any(|failure| failure.class != first) {
            return Some("source_failures");
        }
        Some(match first {
            SourceBackedSourceFailureClass::Unavailable => "source_unavailable",
            SourceBackedSourceFailureClass::SourceChanged => "source_changed",
            SourceBackedSourceFailureClass::Unreadable => "malformed_source",
            SourceBackedSourceFailureClass::Incompatible => "unsupported_schema",
        })
    })
}

fn source_backed_refresh_error_summary(error: &anyhow::Error) -> String {
    let failed_routes = error.chain().find_map(|cause| {
        let SourceBackedCoordinatorError::NoUsableSourceRoutes { failed_routes } =
            cause.downcast_ref::<SourceBackedCoordinatorError>()?
        else {
            return None;
        };
        Some(failed_routes)
    });
    let Some(failed_routes) = failed_routes else {
        return format!("{error:#}");
    };
    let mut summaries = failed_routes
        .iter()
        .map(|failure| format!("{} ({})", failure.provider.as_str(), failure.class.as_str()))
        .collect::<Vec<_>>();
    summaries.sort();
    summaries.dedup();
    format!(
        "source-backed refresh retained no usable source; failed routes: {}",
        summaries.join(", ")
    )
}

fn find_attempt<'a>(
    state: &'a CoreRefreshEngineState,
    request_id: &str,
) -> Option<&'a SourceBackedRefreshAttempt> {
    state
        .attempts
        .iter()
        .find(|attempt| attempt.request_id == request_id)
}

fn find_attempt_mut<'a>(
    state: &'a mut CoreRefreshEngineState,
    request_id: &str,
) -> Option<&'a mut SourceBackedRefreshAttempt> {
    state
        .attempts
        .iter_mut()
        .find(|attempt| attempt.request_id == request_id)
}

fn coalesce_attempt(
    attempt: &mut SourceBackedRefreshAttempt,
    metadata: SourceRefreshRuntimeMetadata,
) -> Value {
    if metadata.operation == SourceBackedRefreshOperation::Import {
        attempt.operation = SourceBackedRefreshOperation::Import;
        attempt.trigger = metadata.trigger;
        attempt.trigger_provenance = metadata.trigger_provenance;
    }
    attempt.coalesced_requests = attempt.coalesced_requests.saturating_add(1);
    attempt.to_json()
}

fn aggregate_manual_all_continuation(
    publication: &mut SourceBackedRefreshPublication,
    continuation: &ManualAllContinuation,
) {
    if continuation.covered_route_ids.is_empty() {
        return;
    }
    let covered = continuation
        .covered_route_ids
        .iter()
        .map(|route| route.as_str().to_owned());
    publication.selected_route_ids.extend(covered.clone());
    publication.successful_route_ids.extend(covered);
    publication.selected_route_ids.sort();
    publication.selected_route_ids.dedup();
    publication.successful_route_ids.sort();
    publication.successful_route_ids.dedup();
    publication.scanned_routes = publication
        .scanned_routes
        .saturating_add(continuation.covered_scanned_routes);
    publication.current.removed_source_count = publication
        .current
        .removed_source_count
        .saturating_add(continuation.covered_removed_source_count);
    publication.timings.discovery_us = publication
        .timings
        .discovery_us
        .saturating_add(continuation.covered_timings.discovery_us);
    publication.timings.scan_stage_us = publication
        .timings
        .scan_stage_us
        .saturating_add(continuation.covered_timings.scan_stage_us);
    publication.timings.commit_us = publication
        .timings
        .commit_us
        .saturating_add(continuation.covered_timings.commit_us);
}

fn new_refresh_attempt(
    observed_generation: Option<String>,
    metadata: SourceRefreshRuntimeMetadata,
    requested_catalog: Option<ExplicitSourceCatalogAuthority>,
    refresh_scope: SourceBackedRefreshScope,
) -> SourceBackedRefreshAttempt {
    SourceBackedRefreshAttempt {
        request_id: Uuid::now_v7().to_string(),
        state: SourceBackedRefreshState::Queued,
        requested_at_ms: utc_now().timestamp_millis(),
        started_at_ms: None,
        finished_at_ms: None,
        previous_generation: observed_generation.clone(),
        published_generation: observed_generation,
        refresh_scope,
        operation: metadata.operation,
        requested_explicit_source_catalog: requested_catalog,
        published_explicit_source_catalog: None,
        coalesced_requests: 0,
        progress: SourceBackedRefreshProgress::default(),
        scanned_routes: None,
        unsupported_routes: None,
        certified_source_count: None,
        certified_source_bytes: None,
        receipt: None,
        timings: None,
        publication_probe_us: 0,
        daemon_mode: metadata.daemon_mode,
        trigger: metadata.trigger,
        trigger_provenance: metadata.trigger_provenance,
        failure_type: None,
        last_error: None,
        post_publication_error: None,
    }
}

fn active_attempt_count(state: &CoreRefreshEngineState) -> usize {
    state
        .attempts
        .iter()
        .filter(|attempt| attempt.state.is_active())
        .count()
}

fn trim_terminal_attempt_history(state: &mut CoreRefreshEngineState) {
    let mut terminal_count = state
        .attempts
        .iter()
        .filter(|attempt| !attempt.state.is_active())
        .count();
    while terminal_count > SOURCE_REFRESH_ATTEMPT_HISTORY {
        let Some(oldest_terminal) = state
            .attempts
            .iter()
            .position(|attempt| !attempt.state.is_active())
        else {
            break;
        };
        state.attempts.remove(oldest_terminal);
        terminal_count = terminal_count.saturating_sub(1);
    }
}

fn source_route_ledger_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "coordinator_state/frontier_behavior_tests.rs"]
mod frontier_behavior_tests;
