use super::*;

mod generation_authority;
mod generation_observation;
mod read_model;
mod runtime_metadata;
pub(crate) use generation_authority::PinnedCorePublication;
use read_model::{
    SourceBackedRefreshAttempt, SourceBackedRefreshProgress, SourceBackedRefreshState,
};
pub(crate) use read_model::{SourceBackedRefreshReceipt, SourceBackedRefreshTimings};
use runtime_metadata::{
    source_catalog_refresh_runtime_metadata, source_refresh_runtime_metadata,
    SourceRefreshRuntimeMetadata,
};

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

pub(super) struct CoreRefreshEngineState {
    active_request_id: Option<String>,
    pending_request_ids: VecDeque<String>,
    attempts: VecDeque<SourceBackedRefreshAttempt>,
    pinned_core_publication: Option<Arc<PinnedCorePublication>>,
    current_published_generation: Option<String>,
}

pub(in crate::semantic) struct CoreRefreshEngine {
    state: Mutex<CoreRefreshEngineState>,
    pub(super) executor: Arc<dyn SourceBackedRefreshExecutor>,
}

pub(in crate::semantic) struct SourceBackedRefreshRun {
    pub(in crate::semantic) job: Value,
    pub(in crate::semantic) did_work: bool,
    pub(in crate::semantic) failed: bool,
}

#[derive(Debug)]
struct SourceBackedRefreshQueueFull {
    active_pending_requests: usize,
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
                let response = match self.enqueue_with_catalog_metadata(
                    previous_generation,
                    metadata,
                    requested_catalog,
                    mode == "wait",
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
        let verified_index = RefCell::new(None::<Arc<VerifiedIndex>>);
        let publication_probe_attempted = Cell::new(false);
        let request_id_cell = RefCell::new(None::<String>);
        let run = self.run_next_with(
            |request_id, coordinator| {
                request_id_cell.replace(Some(request_id.to_owned()));
                let requested_catalog =
                    coordinator.freeze_requested_explicit_source_catalog(data_root, request_id)?;
                let running_job = coordinator
                    .job_status(request_id)
                    .ok_or_else(|| anyhow!("running source refresh has no job state"))?;
                write_daemon_job_status(
                    &daemon_source_backed_refresh_job_path(data_root),
                    &running_job,
                )?;
                let publication = execute_source_backed_refresh(
                    executor.as_ref(),
                    data_root,
                    request_id,
                    coordinator,
                    Some(&requested_catalog),
                )?;
                let probe_started = StdInstant::now();
                publication_probe_attempted.set(true);
                let pin = Arc::new(
                    open_verified_index(&source_backed_index_root(data_root))
                        .context("verify Core generation after publication")?,
                );
                let verification = verify_source_backed_publication(&publication, &pin);
                coordinator.set_publication_probe_timing(
                    request_id,
                    nonzero_duration_micros(probe_started.elapsed()),
                );
                verified_index.replace(Some(pin));
                verification?;
                Ok(publication)
            },
            || {
                if let Some(verified) = verified_index.borrow().as_ref() {
                    return Ok(Some(verified.generation_id().to_owned()));
                }
                if publication_probe_attempted.get() {
                    bail!(
                        "post-publication verified-index probe already failed in this refresh cycle"
                    );
                }
                let verified = open_published_generation(data_root)?.map(Arc::new);
                let generation_id = verified
                    .as_ref()
                    .map(|index| index.generation_id().to_owned());
                verified_index.replace(verified);
                Ok(generation_id)
            },
            |_| {
                request_id_cell
                    .borrow()
                    .as_deref()
                    .ok_or_else(|| anyhow!("published source refresh has no request ID"))
                    .and_then(|request_id| {
                        self.job_status(request_id)
                            .ok_or_else(|| anyhow!("published source refresh has no job state"))
                    })
                    .and_then(|job| {
                        write_daemon_job_status(
                            &daemon_source_backed_refresh_job_path(data_root),
                            &job,
                        )
                    })
            },
            |_| Ok(()),
        )?;
        if !run.failed {
            let pin = verified_index.into_inner();
            let request_id = run.job.get("request_id").and_then(Value::as_str);
            let receipt = request_id.and_then(|request_id| self.receipt_for_request(request_id));
            let binding = match (pin, receipt) {
                (Some(pin), Some(receipt)) => self.bind_core_publication(receipt, pin),
                _ => Err(anyhow!(
                    "completed Core publication has no exact verified generation pin and receipt"
                )),
            };
            if let Err(error) = binding {
                let mut job = run.job;
                job["post_publication_error"] =
                    Value::String(format!("bind exact Core publication receipt: {error:#}"));
                return Some(SourceBackedRefreshRun {
                    job,
                    did_work: run.did_work,
                    failed: false,
                });
            }
        }
        Some(run)
    }

    fn set_publication_probe_timing(&self, request_id: &str, duration_us: u64) {
        let mut state = self.lock_state();
        if let Some(attempt) = find_attempt_mut(&mut state, request_id) {
            attempt.publication_probe_us = duration_us;
        }
    }

    pub(in crate::semantic) fn enqueue_periodic(&self, data_root: &Path) -> Result<Value> {
        let observed_generation = self.observed_published_generation(data_root)?;
        let catalog = load_explicit_source_catalog_authority(data_root)?;
        self.enqueue_with_catalog_metadata(
            observed_generation,
            SourceRefreshRuntimeMetadata::periodic(),
            Some(catalog),
            false,
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

    #[cfg(test)]
    fn enqueue_with_metadata(
        &self,
        observed_generation: Option<String>,
        metadata: SourceRefreshRuntimeMetadata,
    ) -> Value {
        self.enqueue_with_catalog_metadata(observed_generation, metadata, None, false)
            .expect("requests without catalog authority always coalesce")
    }

    fn enqueue_with_catalog_metadata(
        &self,
        observed_generation: Option<String>,
        metadata: SourceRefreshRuntimeMetadata,
        requested_catalog: Option<ExplicitSourceCatalogAuthority>,
        fresh_after_running: bool,
    ) -> Result<Value> {
        let mut state = self.lock_state();
        if let Some(active_request_id) = state.active_request_id.clone() {
            if let Some(active) = find_attempt_mut(&mut state, &active_request_id) {
                if active.state.is_active()
                    && !(fresh_after_running && active.state == SourceBackedRefreshState::Running)
                {
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

        if fresh_after_running || requested_catalog.is_some() {
            let coalesced_request_id = state.pending_request_ids.iter().find_map(|request_id| {
                find_attempt(&state, request_id)
                    .filter(|attempt| {
                        attempt.state.is_active()
                            && attempt.requested_explicit_source_catalog.as_ref()
                                == requested_catalog.as_ref()
                    })
                    .map(|attempt| attempt.request_id.clone())
            });
            if let Some(coalesced_request_id) = coalesced_request_id {
                let attempt = find_attempt_mut(&mut state, &coalesced_request_id)
                    .expect("pending source refresh attempt");
                return Ok(coalesce_attempt(attempt, metadata));
            }
        }

        let active_pending_requests = active_attempt_count(&state);
        if active_pending_requests >= SOURCE_REFRESH_ACTIVE_PENDING_LIMIT {
            return Err(SourceBackedRefreshQueueFull {
                active_pending_requests,
            }
            .into());
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
            publication_probe_us: 0,
            daemon_mode: metadata.daemon_mode,
            trigger: metadata.trigger,
            trigger_provenance: metadata.trigger_provenance,
            failure_type: None,
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
        trim_terminal_attempt_history(&mut state);
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

    fn freeze_requested_explicit_source_catalog(
        &self,
        data_root: &Path,
        request_id: &str,
    ) -> Result<ExplicitSourceCatalogAuthority> {
        if let Some(catalog) = self.requested_explicit_source_catalog(request_id) {
            return Ok(catalog);
        }
        let catalog = load_explicit_source_catalog_authority(data_root)?;
        let mut state = self.lock_state();
        let attempt = find_attempt_mut(&mut state, request_id)
            .ok_or_else(|| anyhow!("source refresh request `{request_id}` is unknown"))?;
        if let Some(existing) = attempt.requested_explicit_source_catalog.as_ref() {
            return Ok(existing.clone());
        }
        attempt.requested_explicit_source_catalog = Some(catalog.clone());
        Ok(catalog)
    }

    fn job_status(&self, request_id: &str) -> Option<Value> {
        let state = self.lock_state();
        find_attempt(&state, request_id).map(SourceBackedRefreshAttempt::job_json)
    }

    fn receipt_for_request(&self, request_id: &str) -> Option<SourceBackedRefreshReceipt> {
        let state = self.lock_state();
        find_attempt(&state, request_id).and_then(|attempt| attempt.receipt.clone())
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
        let mut newly_published_generation = None;
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
                        published_explicit_source_catalog: publication
                            .published_explicit_source_catalog
                            .clone(),
                        current: publication.current,
                    });
                    attempt.timings = Some(publication.timings);
                    attempt.published_explicit_source_catalog =
                        Some(publication.published_explicit_source_catalog);
                    newly_published_generation = Some(observed);
                }
                Err(error) => {
                    attempt.state = SourceBackedRefreshState::Failed;
                    attempt.progress.phase = "failed".to_owned();
                    attempt.failure_type = execution_failure_type;
                    attempt.last_error = Some(error);
                }
            }

            let failed = attempt.state == SourceBackedRefreshState::Failed;
            let did_work = !failed && attempt.published_generation != previous_generation;
            (failed, did_work)
        };
        if newly_published_generation.is_some() {
            state.current_published_generation = newly_published_generation;
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
        })
    }

    pub(super) fn lock_state(&self) -> std::sync::MutexGuard<'_, CoreRefreshEngineState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
}

fn source_backed_refresh_failure_type(error: &anyhow::Error) -> Option<&'static str> {
    error.chain().find_map(|cause| {
        let route = cause.downcast_ref::<SourceBackedRouteError>()?;
        match route.kind {
            SourceBackedRouteErrorKind::Unsupported => Some("unsupported_schema"),
            SourceBackedRouteErrorKind::InvalidSource => Some("malformed_source"),
            _ => None,
        }
    })
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
    if metadata.trigger == "import" {
        attempt.trigger = metadata.trigger;
        attempt.trigger_provenance = metadata.trigger_provenance;
    }
    attempt.coalesced_requests = attempt.coalesced_requests.saturating_add(1);
    attempt.to_json()
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
