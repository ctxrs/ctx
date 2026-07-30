use super::*;

mod runtime_metadata;
use runtime_metadata::{
    source_catalog_refresh_runtime_metadata, source_refresh_runtime_metadata,
    SourceRefreshRuntimeMetadata,
};

/// Verified terminal receipt for one daemon-owned source refresh.
#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct SourceBackedRefreshReceipt {
    pub(crate) previous_generation: Option<String>,
    pub(crate) published_generation: String,
    pub(crate) generation_changed: bool,
    pub(crate) current: SourceBackedRefreshCurrent,
}

impl SourceBackedRefreshReceipt {
    pub(crate) fn to_json(&self) -> Value {
        compact_json(json!({
            "previous_generation": self.previous_generation,
            "published_generation": self.published_generation,
            "generation_changed": self.generation_changed,
            "current": self.current.to_json(),
        }))
    }
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub(crate) struct SourceBackedRefreshTimings {
    pub(crate) discovery_us: u64,
    pub(crate) scan_stage_us: u64,
    pub(crate) commit_us: u64,
}

pub(super) struct SourceBackedRefreshProgressUpdate {
    pub(super) phase: String,
    pub(super) completed_sources: usize,
    pub(super) total_sources: usize,
    pub(super) current_source: Option<String>,
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
    pub(super) report_progress: &'a dyn Fn(SourceBackedRefreshProgressUpdate) -> Result<()>,
}

impl SourceBackedRefreshExecution<'_> {
    pub(crate) fn report_progress(
        &self,
        phase: &str,
        completed_sources: usize,
        total_sources: usize,
        current_source: Option<String>,
    ) -> Result<()> {
        (self.report_progress)(SourceBackedRefreshProgressUpdate {
            phase: phase.to_owned(),
            completed_sources,
            total_sources,
            current_source,
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

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum SourceBackedRefreshState {
    Queued,
    Running,
    Published,
    Failed,
}

impl SourceBackedRefreshState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Published => "published",
            Self::Failed => "failed",
        }
    }

    fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }
}

#[derive(Debug, Clone)]
struct SourceBackedRefreshProgress {
    phase: String,
    completed_sources: usize,
    total_sources: usize,
    current_source: Option<String>,
}

impl Default for SourceBackedRefreshProgress {
    fn default() -> Self {
        Self {
            phase: "queued".to_owned(),
            completed_sources: 0,
            total_sources: 0,
            current_source: None,
        }
    }
}

impl SourceBackedRefreshProgress {
    fn to_json(&self) -> Value {
        compact_json(json!({
            "phase": self.phase,
            "completed_sources": self.completed_sources,
            "total_sources": self.total_sources,
            "current_source": self.current_source,
        }))
    }
}

#[derive(Debug, Clone)]
struct SourceBackedRefreshAttempt {
    request_id: String,
    state: SourceBackedRefreshState,
    requested_at_ms: i64,
    started_at_ms: Option<i64>,
    finished_at_ms: Option<i64>,
    previous_generation: Option<String>,
    published_generation: Option<String>,
    requested_explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
    published_explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
    coalesced_requests: u64,
    progress: SourceBackedRefreshProgress,
    scanned_routes: Option<usize>,
    unsupported_routes: Option<usize>,
    certified_source_count: Option<usize>,
    certified_source_bytes: Option<u64>,
    receipt: Option<SourceBackedRefreshReceipt>,
    timings: Option<SourceBackedRefreshTimings>,
    daemon_mode: DaemonMode,
    trigger: &'static str,
    trigger_provenance: &'static str,
    last_error: Option<String>,
    post_publication_error: Option<String>,
}

impl SourceBackedRefreshAttempt {
    fn failure_code(&self) -> Option<&'static str> {
        self.last_error
            .as_deref()
            .filter(|error| error.contains(TERMINAL_COVERAGE_ERROR_CODE))
            .map(|_| TERMINAL_COVERAGE_ERROR_CODE)
    }

    fn failure_reason(&self) -> Option<&'static str> {
        self.failure_code()
            .map(|_| "provider_terminal_coverage_unavailable")
    }

    fn to_json(&self) -> Value {
        compact_json(json!({
            "ok": true,
            "schema_version": 1,
            "owner": "daemon",
            "request_id": self.request_id,
            "request_state": self.state.as_str(),
            "requested_at_ms": self.requested_at_ms,
            "started_at_ms": self.started_at_ms,
            "finished_at_ms": self.finished_at_ms,
            "previous_generation": self.previous_generation,
            "published_generation": self.published_generation,
            "requested_explicit_source_catalog": self.requested_explicit_source_catalog
                .as_ref()
                .map(ExplicitSourceCatalogAuthority::to_json),
            "published_explicit_source_catalog": self.published_explicit_source_catalog
                .as_ref()
                .map(ExplicitSourceCatalogAuthority::to_json),
            "generation_changed": self.receipt.as_ref().map(|receipt| receipt.generation_changed),
            "receipt": self.receipt.as_ref().map(SourceBackedRefreshReceipt::to_json),
            "coalesced_requests": self.coalesced_requests,
            "progress": self.progress.to_json(),
            "scanned_routes": self.scanned_routes,
            "unsupported_routes": self.unsupported_routes,
            "certified_source_count": self.certified_source_count,
            "certified_source_bytes": self.certified_source_bytes,
            "timings_us": self.timings.map(SourceBackedRefreshTimings::to_json),
            "daemon_mode": self.daemon_mode.as_str(),
            "trigger": self.trigger,
            "trigger_provenance": self.trigger_provenance,
            "error_code": self.failure_code(),
            "reason": self.failure_reason(),
            "last_error": self.last_error,
            "post_publication_error": self.post_publication_error,
        }))
    }

    fn job_json(&self) -> Value {
        let status = match self.state {
            SourceBackedRefreshState::Published => "completed",
            SourceBackedRefreshState::Failed => "failed",
            SourceBackedRefreshState::Queued | SourceBackedRefreshState::Running => "running",
        };
        compact_json(json!({
            "mode": SourceBackedRefreshMode::Background.as_str(),
            "owner": "daemon",
            "kind": "source_backed",
            "status": status,
            "request_id": self.request_id,
            "request_state": self.state.as_str(),
            "source_count": self.progress.total_sources,
            "last_run_at_ms": self.started_at_ms.unwrap_or(self.requested_at_ms),
            "previous_generation": self.previous_generation,
            "published_generation": self.published_generation,
            "requested_explicit_source_catalog": self.requested_explicit_source_catalog
                .as_ref()
                .map(ExplicitSourceCatalogAuthority::to_json),
            "published_explicit_source_catalog": self.published_explicit_source_catalog
                .as_ref()
                .map(ExplicitSourceCatalogAuthority::to_json),
            "generation_changed": self.receipt.as_ref().map(|receipt| receipt.generation_changed),
            "receipt": self.receipt.as_ref().map(SourceBackedRefreshReceipt::to_json),
            "coalesced_requests": self.coalesced_requests,
            "progress": self.progress.to_json(),
            "scanned_routes": self.scanned_routes,
            "unsupported_routes": self.unsupported_routes,
            "certified_source_count": self.certified_source_count,
            "certified_source_bytes": self.certified_source_bytes,
            "timings_us": self.timings.map(SourceBackedRefreshTimings::to_json),
            "daemon_mode": self.daemon_mode.as_str(),
            "trigger": self.trigger,
            "trigger_provenance": self.trigger_provenance,
            "error_code": self.failure_code(),
            "reason": self.failure_reason(),
            "last_error": self.last_error,
            "post_publication_error": self.post_publication_error,
        }))
    }
}

impl SourceBackedRefreshTimings {
    fn to_json(self) -> Value {
        json!({
            "discovery": self.discovery_us,
            "scan_stage": self.scan_stage_us,
            "commit": self.commit_us,
        })
    }
}

pub(super) struct SourceBackedRefreshCoordinatorState {
    active_request_id: Option<String>,
    pending_request_ids: VecDeque<String>,
    attempts: VecDeque<SourceBackedRefreshAttempt>,
    published_resolvers: HashMap<String, RetainedGenerationResolver>,
    current_published_generation: Option<String>,
}

struct RetainedGenerationResolver {
    resolver: Arc<GenerationBoundSourceBackedResolver>,
    retired_at: Option<StdInstant>,
}

impl SourceBackedRefreshCoordinatorState {
    fn install_resolver(&mut self, resolver: Arc<GenerationBoundSourceBackedResolver>) {
        let generation_id = resolver.generation_id.clone();
        let now = StdInstant::now();
        if let Some(previous) = self.current_published_generation.as_deref() {
            if previous != generation_id {
                if let Some(retained) = self.published_resolvers.get_mut(previous) {
                    retained.retired_at.get_or_insert(now);
                }
            }
        }
        self.published_resolvers.insert(
            generation_id.clone(),
            RetainedGenerationResolver {
                resolver,
                retired_at: None,
            },
        );
        self.current_published_generation = Some(generation_id);
        self.prune_retired_resolvers(now, SOURCE_RESOLVER_RETIREMENT_GRACE);
    }

    fn prune_retired_resolvers(&mut self, now: StdInstant, grace: StdDuration) {
        self.published_resolvers.retain(|_, retained| {
            retained.retired_at.is_none_or(|retired_at| {
                now.saturating_duration_since(retired_at) < grace
                    || Arc::strong_count(&retained.resolver) > 1
            })
        });
    }
}

pub(in crate::semantic) struct SourceBackedRefreshCoordinator {
    state: Mutex<SourceBackedRefreshCoordinatorState>,
    pub(super) executor: Arc<dyn SourceBackedRefreshExecutor>,
}

pub(in crate::semantic) struct SourceBackedRefreshRun {
    pub(in crate::semantic) job: Value,
    pub(in crate::semantic) did_work: bool,
    pub(in crate::semantic) failed: bool,
}

/// One leaseable resolver whose identity is inseparable from the verified
/// lexical generation that installed it.
#[derive(Debug)]
#[allow(dead_code)] // Query IPC consumes this seam in the batch-hydration lane.
pub(crate) struct GenerationBoundSourceBackedResolver {
    generation_id: String,
    source_manifest: Option<SourceManifest>,
    resolver: Arc<SourceBackedResolverRegistry>,
}

#[allow(dead_code)] // Query IPC consumes this seam in the batch-hydration lane.
impl GenerationBoundSourceBackedResolver {
    pub(crate) fn generation_id(&self) -> &str {
        &self.generation_id
    }

    pub(crate) fn resolver(&self) -> &SourceBackedResolverRegistry {
        self.resolver.as_ref()
    }

    pub(crate) fn source_manifest(&self) -> Option<&SourceManifest> {
        self.source_manifest.as_ref()
    }
}

#[derive(Debug, Clone, Eq, Error, PartialEq)]
#[allow(dead_code)] // Query IPC exposes this typed failure in the hydration lane.
pub(crate) enum SourceBackedResolverAccessError {
    #[error("daemon has no resolver retained for source-backed generation {requested_generation}")]
    Missing { requested_generation: String },
    #[error(
        "daemon resolver generation mismatch: requested {requested_generation}, retained {retained_generation}"
    )]
    GenerationMismatch {
        requested_generation: String,
        retained_generation: String,
    },
}

impl SourceBackedRefreshCoordinator {
    pub(in crate::semantic) fn new() -> Self {
        Self::with_executor(Arc::new(CaptureOwnedSourceBackedRefreshExecutor))
    }

    pub(in crate::semantic) fn with_executor(
        executor: Arc<dyn SourceBackedRefreshExecutor>,
    ) -> Self {
        Self {
            state: Mutex::new(SourceBackedRefreshCoordinatorState {
                active_request_id: None,
                pending_request_ids: VecDeque::new(),
                attempts: VecDeque::new(),
                published_resolvers: HashMap::new(),
                current_published_generation: None,
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

    pub(in crate::semantic) fn recover_published_resolver(&self, data_root: &Path) -> Result<()> {
        let Some((generation_id, resolver)) = recover_capture_owned_resolver(data_root)? else {
            return Ok(());
        };
        self.install_recovered_resolver(generation_id, resolver);
        Ok(())
    }

    pub(super) fn install_recovered_resolver(
        &self,
        generation_id: String,
        resolver: Arc<SourceBackedResolverRegistry>,
    ) {
        let mut state = self.lock_state();
        state.install_resolver(Arc::new(GenerationBoundSourceBackedResolver {
            generation_id,
            source_manifest: None,
            resolver,
        }));
    }

    fn observed_published_generation(&self, data_root: &Path) -> Result<Option<String>> {
        let retained = {
            let state = self.lock_state();
            state.current_published_generation.clone()
        };
        if retained.is_some() {
            return Ok(retained);
        }
        if let Some(generation_id) = retained_generation_hint(data_root)? {
            return Ok(Some(generation_id));
        }
        published_generation_id(data_root)
    }

    pub(in crate::semantic) fn retained_published_generation(
        &self,
    ) -> Option<Arc<GenerationBoundSourceBackedResolver>> {
        let state = self.lock_state();
        state
            .current_published_generation
            .as_deref()
            .and_then(|generation_id| state.published_resolvers.get(generation_id))
            .map(|retained| Arc::clone(&retained.resolver))
    }

    /// Returns the resolver only when it is bound to the caller's exact
    /// lexical generation. Missing or stale daemon state queues the same
    /// provider-wide refresh path and remains a typed failure.
    #[allow(dead_code)] // Query IPC consumes this seam in the batch-hydration lane.
    pub(crate) fn resolver_for_generation(
        &self,
        data_root: &Path,
        generation_id: &str,
    ) -> std::result::Result<
        Arc<GenerationBoundSourceBackedResolver>,
        SourceBackedResolverAccessError,
    > {
        let result = {
            let mut state = self.lock_state();
            state.prune_retired_resolvers(StdInstant::now(), SOURCE_RESOLVER_RETIREMENT_GRACE);
            if let Some(retained) = state.published_resolvers.get(generation_id) {
                return Ok(Arc::clone(&retained.resolver));
            }
            match state.current_published_generation.as_ref() {
                Some(retained_generation) => SourceBackedResolverAccessError::GenerationMismatch {
                    requested_generation: generation_id.to_owned(),
                    retained_generation: retained_generation.clone(),
                },
                None => SourceBackedResolverAccessError::Missing {
                    requested_generation: generation_id.to_owned(),
                },
            }
        };
        self.enqueue_with_metadata(
            Some(generation_id.to_owned()),
            source_refresh_runtime_metadata(data_root),
        );
        Err(result)
    }

    /// Preserves the capture resolver's typed failure while arranging for
    /// source state invalidations to be repaired by the daemon refresh loop.
    /// A future batch hydration worker can call this once for a failed batch;
    /// this method performs no hydration and has no legacy fallback.
    #[allow(dead_code)] // Query IPC consumes this seam in the batch-hydration lane.
    pub(crate) fn handle_hydration_failure(
        &self,
        data_root: &Path,
        generation_id: &str,
        failure: HydrationFailure,
    ) -> HydrationFailure {
        let retained_generation_matches = {
            let state = self.lock_state();
            state
                .current_published_generation
                .as_deref()
                .is_some_and(|retained| retained == generation_id)
        };
        if hydration_failure_queues_refresh(failure.kind) && retained_generation_matches {
            self.enqueue_with_metadata(
                Some(generation_id.to_owned()),
                source_refresh_runtime_metadata(data_root),
            );
        }
        failure
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
                let requested_catalog = request
                    .get("explicit_source_catalog")
                    .map(ExplicitSourceCatalogAuthority::from_json)
                    .transpose()?;
                let previous_generation = self.observed_published_generation(data_root)?;
                let metadata = if requested_catalog.is_some() {
                    source_catalog_refresh_runtime_metadata(data_root)
                } else {
                    source_refresh_runtime_metadata(data_root)
                };
                let response = self.enqueue_with_catalog_metadata(
                    previous_generation,
                    metadata,
                    requested_catalog,
                )?;
                let request_id = response
                    .get("request_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("queued source refresh has no request ID"))?;
                if let Some(job) = self.job_status(request_id) {
                    write_daemon_job_status(
                        &daemon_source_backed_refresh_job_path(data_root),
                        &job,
                    )?;
                }
                Ok(Some(response))
            }
            Some(SOURCE_REFRESH_STATUS_OP) => {
                let request_id = request
                    .get("request_id")
                    .and_then(Value::as_str)
                    .filter(|request_id| !request_id.is_empty())
                    .ok_or_else(|| anyhow!("daemon source refresh request ID is missing"))?;
                let status = self.status(request_id).ok_or_else(|| {
                    anyhow!("daemon source refresh request `{request_id}` is unknown")
                })?;
                Ok(Some(status))
            }
            _ => Ok(None),
        }
    }

    pub(in crate::semantic) fn run_next(&self, data_root: &Path) -> Option<SourceBackedRefreshRun> {
        let executor = Arc::clone(&self.executor);
        self.run_next_with(
            |request_id, coordinator| {
                let requested_catalog = coordinator.requested_explicit_source_catalog(request_id);
                let publication = execute_source_backed_refresh(
                    executor.as_ref(),
                    data_root,
                    request_id,
                    coordinator,
                    requested_catalog.as_ref(),
                )?;
                Ok(publication)
            },
            || published_generation_id(data_root),
            |generation_id| complete_verified_source_epoch(data_root, generation_id),
            |_| Ok(()),
        )
    }

    pub(in crate::semantic) fn enqueue_periodic(&self, data_root: &Path) -> Result<Value> {
        let observed_generation = self.observed_published_generation(data_root)?;
        let catalog = load_explicit_source_catalog_authority(data_root)?;
        self.enqueue_with_catalog_metadata(
            observed_generation,
            SourceRefreshRuntimeMetadata::periodic(),
            Some(catalog),
        )
    }

    #[cfg(test)]
    pub(super) fn enqueue(&self, observed_generation: Option<String>) -> Value {
        self.enqueue_with_metadata(observed_generation, SourceRefreshRuntimeMetadata::default())
    }

    #[cfg(test)]
    pub(in crate::semantic) fn enqueue_for_test(
        &self,
        observed_generation: Option<String>,
    ) -> Value {
        self.enqueue(observed_generation)
    }

    fn enqueue_with_metadata(
        &self,
        observed_generation: Option<String>,
        metadata: SourceRefreshRuntimeMetadata,
    ) -> Value {
        self.enqueue_with_catalog_metadata(observed_generation, metadata, None)
            .expect("requests without catalog authority always coalesce")
    }

    fn enqueue_with_catalog_metadata(
        &self,
        observed_generation: Option<String>,
        metadata: SourceRefreshRuntimeMetadata,
        requested_catalog: Option<ExplicitSourceCatalogAuthority>,
    ) -> Result<Value> {
        let mut state = self.lock_state();
        if let Some(active_request_id) = state.active_request_id.clone() {
            if let Some(active) = find_attempt_mut(&mut state, &active_request_id) {
                if active.state.is_active() {
                    if requested_catalog.is_none() {
                        return Ok(coalesce_attempt(active, metadata));
                    }
                    if let Some(requested_catalog) = requested_catalog.as_ref() {
                        let upgrades_queued_automatic =
                            active.requested_explicit_source_catalog.is_none()
                                && active.state == SourceBackedRefreshState::Queued;
                        if upgrades_queued_automatic {
                            active.requested_explicit_source_catalog =
                                Some(requested_catalog.clone());
                        }
                        if active.requested_explicit_source_catalog.as_ref()
                            == Some(requested_catalog)
                        {
                            return Ok(coalesce_attempt(active, metadata));
                        }
                        // A running refresh is immutable. Preserve both catalog
                        // authorities by serializing the newer one as a successor.
                    }
                }
            }
        }

        if let Some(requested_catalog) = requested_catalog.as_ref() {
            let coalesced_request_id = state.pending_request_ids.iter().find_map(|request_id| {
                find_attempt(&state, request_id)
                    .filter(|attempt| {
                        attempt.state.is_active()
                            && attempt.requested_explicit_source_catalog.as_ref()
                                == Some(requested_catalog)
                    })
                    .map(|attempt| attempt.request_id.clone())
            });
            if let Some(coalesced_request_id) = coalesced_request_id {
                let attempt = find_attempt_mut(&mut state, &coalesced_request_id)
                    .expect("pending source refresh attempt");
                return Ok(coalesce_attempt(attempt, metadata));
            }
        }

        let attempt = SourceBackedRefreshAttempt {
            request_id: Uuid::now_v7().to_string(),
            state: SourceBackedRefreshState::Queued,
            requested_at_ms: utc_now().timestamp_millis(),
            started_at_ms: None,
            finished_at_ms: None,
            previous_generation: observed_generation.clone(),
            published_generation: observed_generation,
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
            daemon_mode: metadata.daemon_mode,
            trigger: metadata.trigger,
            trigger_provenance: metadata.trigger_provenance,
            last_error: None,
            post_publication_error: None,
        };
        let response = attempt.to_json();
        if state
            .active_request_id
            .as_deref()
            .and_then(|request_id| find_attempt(&state, request_id))
            .is_some_and(|attempt| attempt.state.is_active())
        {
            state
                .pending_request_ids
                .push_back(attempt.request_id.clone());
        } else {
            state.active_request_id = Some(attempt.request_id.clone());
        }
        state.attempts.push_back(attempt);
        trim_attempt_history(&mut state);
        Ok(response)
    }

    pub(super) fn status(&self, request_id: &str) -> Option<Value> {
        let state = self.lock_state();
        find_attempt(&state, request_id).map(SourceBackedRefreshAttempt::to_json)
    }

    fn requested_explicit_source_catalog(
        &self,
        request_id: &str,
    ) -> Option<ExplicitSourceCatalogAuthority> {
        let state = self.lock_state();
        find_attempt(&state, request_id)
            .and_then(|attempt| attempt.requested_explicit_source_catalog.clone())
    }

    fn job_status(&self, request_id: &str) -> Option<Value> {
        let state = self.lock_state();
        find_attempt(&state, request_id).map(SourceBackedRefreshAttempt::job_json)
    }

    pub(super) fn set_progress(
        &self,
        request_id: &str,
        phase: &str,
        completed_sources: usize,
        total_sources: usize,
        current_source: Option<String>,
    ) -> Option<Value> {
        let mut state = self.lock_state();
        let attempt = find_attempt_mut(&mut state, request_id)?;
        if attempt.state != SourceBackedRefreshState::Running {
            return None;
        }
        attempt.progress = SourceBackedRefreshProgress {
            phase: phase.to_owned(),
            completed_sources,
            total_sources,
            current_source,
        };
        Some(attempt.job_json())
    }

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
        let (request_id, previous_generation, requested_catalog) = {
            let mut state = self.lock_state();
            let request_id = state.active_request_id.clone()?;
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
            )
        };

        let execution = execute(&request_id, self);
        let observed_generation = probe();
        let (verified, observed_for_status) = match (execution, observed_generation) {
            (Ok(publication), Ok(Some(observed))) if publication.generation_id == observed => {
                let manifest_generation_matches = publication
                    .source_manifest
                    .as_ref()
                    .is_none_or(|manifest| manifest.core_generation_id == observed);
                let production_handoff_complete =
                    publication.resolver.is_some() && publication.source_manifest.is_some();
                let verified = if !manifest_generation_matches {
                    Err(format!(
                        "source-backed refresh published generation {observed} with a source manifest for another generation"
                    ))
                } else if production_handoff_complete || cfg!(test) {
                    Ok((observed.clone(), publication))
                } else {
                    Err(format!(
                        "source-backed refresh published generation {observed} without its resolver registry or source manifest"
                    ))
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
            (Err(error), Ok(observed)) => (Err(format!("{error:#}")), observed),
            (Err(error), Err(probe_error)) => (Err(format!(
                "{error:#}; verifying the retained generation also failed: {probe_error:#}"
            )), None),
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
        let mut installed_resolver = None;
        let (failed_run, did_work) = {
            let attempt = find_attempt_mut(&mut state, &request_id)?;
            attempt.finished_at_ms = Some(utc_now().timestamp_millis());
            attempt.progress.current_source = None;
            if observed_for_status.is_some() {
                attempt.published_generation = observed_for_status.clone();
            }

            match verified {
                Ok((observed, publication)) => {
                    let generation_changed =
                        previous_generation.as_deref() != Some(observed.as_str());
                    attempt.state = SourceBackedRefreshState::Published;
                    attempt.published_generation = Some(observed.clone());
                    attempt.progress.phase = "published".to_owned();
                    attempt.progress.completed_sources = attempt.progress.total_sources;
                    attempt.scanned_routes = Some(publication.scanned_routes);
                    attempt.unsupported_routes = Some(publication.unsupported_routes);
                    attempt.certified_source_count = Some(publication.certified_source_count);
                    attempt.certified_source_bytes = Some(publication.certified_source_bytes);
                    attempt.receipt = Some(SourceBackedRefreshReceipt {
                        previous_generation: previous_generation.clone(),
                        published_generation: observed.clone(),
                        generation_changed,
                        current: publication.current,
                    });
                    attempt.timings = Some(publication.timings);
                    let source_manifest = publication.source_manifest;
                    attempt.published_explicit_source_catalog = requested_catalog.clone();
                    installed_resolver = publication.resolver.map(|resolver| {
                        Arc::new(GenerationBoundSourceBackedResolver {
                            generation_id: observed.clone(),
                            source_manifest,
                            resolver,
                        })
                    });
                }
                Err(error) => {
                    attempt.state = SourceBackedRefreshState::Failed;
                    attempt.progress.phase = "failed".to_owned();
                    attempt.last_error = Some(error);
                }
            }

            let failed = attempt.state == SourceBackedRefreshState::Failed;
            let did_work = !failed && attempt.published_generation != previous_generation;
            (failed, did_work)
        };
        if let Some(resolver) = installed_resolver {
            state.install_resolver(resolver);
        }
        if state.active_request_id.as_deref() == Some(request_id.as_str()) {
            state.active_request_id = state.pending_request_ids.pop_front();
            if let Some(next_request_id) = state.active_request_id.clone() {
                if let Some(next_attempt) = find_attempt_mut(&mut state, &next_request_id) {
                    if observed_for_status.is_some() {
                        next_attempt.previous_generation = observed_for_status.clone();
                        next_attempt.published_generation = observed_for_status.clone();
                    }
                }
            }
        }
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
        })
    }

    #[cfg(test)]
    pub(super) fn prune_retired_resolvers_for_test(&self) {
        self.lock_state()
            .prune_retired_resolvers(StdInstant::now(), StdDuration::ZERO);
    }

    #[cfg(test)]
    pub(super) fn has_retained_resolver_for_test(&self, generation_id: &str) -> bool {
        self.lock_state()
            .published_resolvers
            .contains_key(generation_id)
    }

    pub(super) fn lock_state(
        &self,
    ) -> std::sync::MutexGuard<'_, SourceBackedRefreshCoordinatorState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
}

fn find_attempt<'a>(
    state: &'a SourceBackedRefreshCoordinatorState,
    request_id: &str,
) -> Option<&'a SourceBackedRefreshAttempt> {
    state
        .attempts
        .iter()
        .find(|attempt| attempt.request_id == request_id)
}

fn find_attempt_mut<'a>(
    state: &'a mut SourceBackedRefreshCoordinatorState,
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
    if metadata.trigger == "import" {
        attempt.trigger = metadata.trigger;
        attempt.trigger_provenance = metadata.trigger_provenance;
    }
    attempt.coalesced_requests = attempt.coalesced_requests.saturating_add(1);
    attempt.to_json()
}

fn trim_attempt_history(state: &mut SourceBackedRefreshCoordinatorState) {
    while state.attempts.len() > SOURCE_REFRESH_ATTEMPT_HISTORY {
        if state
            .attempts
            .front()
            .is_some_and(|attempt| attempt.state.is_active())
        {
            break;
        }
        state.attempts.pop_front();
    }
}
