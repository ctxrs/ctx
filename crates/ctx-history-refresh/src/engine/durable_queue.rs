use super::*;

const DURABLE_PROGRESS_PERSIST_INTERVAL: StdDuration = StdDuration::from_secs(1);

const QUEUED_SUCCESSORS_FIELD: &str = "queued_successors";
const DAEMON_RETRY_FIELDS: [&str; 4] = [
    "retryable",
    "retry_after_ms",
    "consecutive_failures",
    "retry_not_before_at_ms",
];

impl CoreRefreshEngine {
    pub(super) fn persist_job_status(&self, data_root: &Path, request_id: &str) -> Result<()> {
        let state = self.lock_state();
        let requested_attempt = find_attempt(&state, request_id)
            .ok_or_else(|| anyhow!("source refresh request `{request_id}` is unknown"))?;
        let durable_request_id = if let Some(pending) = state.pending_terminal_persistence.as_ref()
        {
            pending.request_id.as_str()
        } else if !requested_attempt.state.is_active() {
            request_id
        } else {
            state
                .pending_scheduler_retry_root_id
                .as_deref()
                .or(state.active_request_id.as_deref())
                .unwrap_or(request_id)
        };
        let job = durable_job_json(&state, durable_request_id)
            .ok_or_else(|| anyhow!("source refresh request `{durable_request_id}` is unknown"))?;
        // Keep the state lock through publication so an admission snapshot
        // cannot overwrite a later terminal snapshot during waiter races.
        self.write_status(data_root, &job)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn persist_job_status_for_test(&self, data_root: &Path, request_id: &str) -> Result<()> {
        self.persist_job_status(data_root, request_id)
    }

    pub(super) fn write_status(&self, data_root: &Path, job: &Value) -> Result<()> {
        self.journal.store(data_root, job)
    }

    pub(super) fn write_durable_admission_status(
        &self,
        data_root: &Path,
        job: &Value,
    ) -> DurableAdmissionPersistence {
        self.journal.store_before_ack(data_root, job)
    }

    pub(crate) fn persist_progress(
        &self,
        data_root: &Path,
        request_id: &str,
        update: SourceBackedRefreshProgressUpdate,
    ) -> Result<()> {
        let mut state = self.lock_state();
        let Some(job) = update_progress(&mut state, request_id, update) else {
            return Ok(());
        };
        let now = StdInstant::now();
        let should_persist = should_persist_progress(
            state.last_progress_persisted_request_id.as_deref(),
            state.last_progress_persisted_at,
            request_id,
            now,
        );
        if !should_persist {
            return Ok(());
        }
        self.write_status(data_root, &job)?;
        state.last_progress_persisted_request_id = Some(request_id.to_owned());
        state.last_progress_persisted_at = Some(now);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn set_progress(
        &self,
        request_id: &str,
        update: SourceBackedRefreshProgressUpdate,
    ) -> Option<Value> {
        let mut state = self.lock_state();
        update_progress(&mut state, request_id, update)
    }

    pub fn persist_retry_status(&self, data_root: &Path, job: Value) -> Result<Value> {
        let request_id = job
            .get("request_id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("source refresh retry status has no request ID"))?
            .to_owned();
        let mut state = self.lock_state();
        find_attempt(&state, &request_id)
            .ok_or_else(|| anyhow!("source refresh retry request `{request_id}` is unknown"))?;
        if durable_queue_entry_count(&state) > SOURCE_REFRESH_ACTIVE_PENDING_LIMIT {
            bail!("source refresh retry queue exceeds its bounded capacity");
        }
        if let Some(authoritative) = pending_terminal_job_json(&state) {
            self.write_status(data_root, &authoritative)?;
            return Ok(authoritative);
        }
        if let Some(authoritative) = authoritative_route_terminal_job(&state, &request_id) {
            // Route retry/block disposition is already part of the engine's
            // durable terminal outcome. Do not let a caller turn it into a
            // second global scheduler retry or replace a canceled logical
            // successor's exact terminal image.
            self.write_status(data_root, &authoritative)?;
            state.pending_scheduler_retry_root_id = None;
            return Ok(authoritative);
        }
        let job = job_with_queued_successors(&state, job);
        // Serialize retry metadata against the same queue authority as IPC
        // admission so an older scheduler snapshot cannot erase a successor.
        self.write_status(data_root, &job)?;
        if state.pending_scheduler_retry_root_id.as_deref() == Some(request_id.as_str()) {
            state.pending_scheduler_retry_root_id = None;
        }
        Ok(job)
    }

    /// Completes the scheduler handoff for a durably terminal admission-fence
    /// failure. This releases queue capacity without resubmitting capture work
    /// or changing the failed logical request's terminal image.
    pub fn complete_retry_admission_handoff(&self, request_id: &str) -> Result<()> {
        let mut state = self.lock_state();
        let attempt = find_attempt(&state, request_id)
            .ok_or_else(|| anyhow!("source refresh request `{request_id}` is unknown"))?;
        let retry_admission = attempt.state == SourceBackedRefreshState::Failed
            && attempt.failure_outcome.as_ref().is_some_and(|outcome| {
                outcome.code == RefreshOutcomeCode::SourceRefreshAdmissionFailed
                    && outcome.retry_advice == Some(RefreshRetryAdvice::RetryAdmission)
            });
        if !retry_admission {
            bail!("source refresh request `{request_id}` has no terminal retry-admission handoff");
        }
        match state.pending_scheduler_retry_root_id.as_deref() {
            None => Ok(()),
            Some(pending) if pending == request_id => {
                state.pending_scheduler_retry_root_id = None;
                Ok(())
            }
            Some(pending) => bail!(
                "source refresh retry-admission handoff belongs to `{pending}`, not `{request_id}`"
            ),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn pending_scheduler_retry_root_for_test(&self) -> Option<String> {
        self.lock_state().pending_scheduler_retry_root_id.clone()
    }

    pub fn persist_scheduler_status(
        &self,
        data_root: &Path,
        scheduler_job: Value,
    ) -> Result<Value> {
        let mut state = self.lock_state();
        if durable_queue_entry_count(&state) > SOURCE_REFRESH_ACTIVE_PENDING_LIMIT {
            bail!("source refresh scheduler queue exceeds its bounded capacity");
        }
        if let Some(authoritative) = pending_terminal_job_json(&state) {
            self.write_status(data_root, &authoritative)?;
            return Ok(authoritative);
        }
        if let Some(request_id) = scheduler_job
            .get("request_id")
            .and_then(Value::as_str)
            .filter(|request_id| find_attempt(&state, request_id).is_some())
        {
            if let Some(authoritative) = authoritative_route_terminal_job(&state, request_id) {
                self.write_status(data_root, &authoritative)?;
                state.pending_scheduler_retry_root_id = None;
                return Ok(authoritative);
            }
        }
        let durable_root = state
            .pending_scheduler_retry_root_id
            .as_deref()
            .or(state.active_request_id.as_deref())
            .map(str::to_owned);
        let job = durable_root
            .as_deref()
            .and_then(|request_id| durable_job_json(&state, request_id))
            .map(|job| overlay_daemon_retry_state(job, &scheduler_job))
            .unwrap_or(scheduler_job);
        // This lock covers both the state recheck and the write. If IPC
        // admission won the lock first, publish its exact queue root; if the
        // scheduler won first, admission will durably supersede this status
        // before acknowledging the request.
        self.write_status(data_root, &job)?;
        if durable_root.as_deref() == state.pending_scheduler_retry_root_id.as_deref() {
            state.pending_scheduler_retry_root_id = None;
        }
        Ok(job)
    }
}

fn should_persist_progress(
    persisted_request_id: Option<&str>,
    persisted_at: Option<StdInstant>,
    request_id: &str,
    now: StdInstant,
) -> bool {
    persisted_request_id != Some(request_id)
        || persisted_at.is_none_or(|persisted_at| {
            now.saturating_duration_since(persisted_at) >= DURABLE_PROGRESS_PERSIST_INTERVAL
        })
}

fn update_progress(
    state: &mut CoreRefreshEngineState,
    request_id: &str,
    update: SourceBackedRefreshProgressUpdate,
) -> Option<Value> {
    let attempt = find_attempt_mut(state, request_id)?;
    if attempt.state != SourceBackedRefreshState::Running {
        return None;
    }
    // Capture reports record/byte counters and detailed source stages through
    // independent callbacks. Compose nonempty fragments only while they still
    // describe the same active source. The completed count distinguishes
    // duplicate display paths, and an empty fragment explicitly clears the
    // active source's optional progress fields.
    let same_active_source = update.current_source.is_some()
        && attempt.progress.phase == update.phase
        && attempt.progress.completed_sources == update.completed_sources
        && attempt.progress.current_source.as_deref() == update.current_source.as_deref();
    let has_source_fragment = update.completed_records.is_some()
        || update.completed_bytes.is_some()
        || update.current_source_progress.is_some();
    let (previous_records, previous_bytes, previous_detail) =
        if same_active_source && has_source_fragment {
            (
                attempt.progress.completed_records,
                attempt.progress.completed_bytes,
                attempt.progress.current_source_progress,
            )
        } else {
            (None, None, None)
        };
    let previous_providers = attempt.progress.providers.clone();
    let previous_processed_sessions = attempt.progress.processed_sessions;
    let previous_processed_messages = attempt.progress.processed_messages;
    let previous_processed_tool_calls = attempt.progress.processed_tool_calls;
    let previous_processed_bytes = attempt.progress.processed_bytes;
    let previous_elapsed_millis = attempt.progress.elapsed_millis;
    attempt.whole_run_eta.update(
        SourceBackedRefreshStage::from_phase(&update.phase),
        update.exact_scan_progress,
        update.elapsed_millis,
    );
    // `committed` means the generation is already usable even though the
    // durable terminal receipt follows as a distinct progress event.
    if update.phase == "committed" {
        attempt.whole_run_eta.clear();
    }
    attempt.progress = SourceBackedRefreshProgress {
        phase: update.phase,
        completed_sources: update.completed_sources,
        total_sources: update.total_sources,
        current_source: update.current_source,
        completed_records: update.completed_records.or(previous_records),
        completed_bytes: update.completed_bytes.or(previous_bytes),
        providers: if update.providers.is_empty() {
            previous_providers
        } else {
            update.providers
        },
        processed_sessions: update.processed_sessions.max(previous_processed_sessions),
        processed_messages: update.processed_messages.max(previous_processed_messages),
        processed_tool_calls: update
            .processed_tool_calls
            .max(previous_processed_tool_calls),
        processed_bytes: update.processed_bytes.max(previous_processed_bytes),
        elapsed_millis: update.elapsed_millis.or(previous_elapsed_millis),
        current_source_progress: update.current_source_progress.or(previous_detail),
    };
    attempt.progress_total_sources_known = update.total_sources_known;
    durable_job_json(state, request_id)
}

fn overlay_daemon_retry_state(mut durable_job: Value, scheduler_job: &Value) -> Value {
    let Some(durable) = durable_job.as_object_mut() else {
        return durable_job;
    };
    for field in DAEMON_RETRY_FIELDS {
        if let Some(value) = scheduler_job.get(field) {
            durable.insert(field.to_owned(), value.clone());
        }
    }
    durable_job
}

fn authoritative_route_terminal_job(
    state: &CoreRefreshEngineState,
    request_id: &str,
) -> Option<Value> {
    let attempt = find_attempt(state, request_id)?;
    let outcome = attempt.failure_outcome.as_ref()?;
    if outcome.affected_routes.is_empty() {
        return None;
    }
    durable_job_json(state, request_id)
}

pub(super) fn durable_job_json(state: &CoreRefreshEngineState, request_id: &str) -> Option<Value> {
    if let Some(pending) = state.pending_terminal_persistence.as_ref() {
        // Every ordinary writer preserves the exact terminal root until the
        // dedicated route-finalization writer commits its markerless image.
        return Some(job_with_queued_successors(
            state,
            pending.terminal_job.clone(),
        ));
    }
    finalized_job_json(state, request_id)
}

pub(super) fn finalized_job_json(
    state: &CoreRefreshEngineState,
    request_id: &str,
) -> Option<Value> {
    projected_job_json(state, request_id).map(|job| job_with_queued_successors(state, job))
}

fn pending_terminal_job_json(state: &CoreRefreshEngineState) -> Option<Value> {
    let pending = state.pending_terminal_persistence.as_ref()?;
    Some(job_with_queued_successors(
        state,
        pending.terminal_job.clone(),
    ))
}

pub(super) fn job_with_queued_successors(state: &CoreRefreshEngineState, mut job: Value) -> Value {
    let root_request_id = job.get("request_id").and_then(Value::as_str);
    let mut successors = Vec::with_capacity(state.pending_request_ids.len().saturating_add(1));
    if let Some(active_request_id) = state
        .active_request_id
        .as_deref()
        .filter(|request_id| Some(*request_id) != root_request_id)
    {
        if let Some(active) = find_attempt(state, active_request_id).filter(|attempt| {
            matches!(
                attempt.state,
                SourceBackedRefreshState::AdmissionPending | SourceBackedRefreshState::Queued
            )
        }) {
            if let Some(job) = projected_job_json(state, &active.request_id) {
                successors.push(job);
            }
        }
    }
    successors.extend(
        state
            .pending_request_ids
            .iter()
            .filter_map(|request_id| find_attempt(state, request_id))
            .filter(|attempt| {
                matches!(
                    attempt.state,
                    SourceBackedRefreshState::AdmissionPending | SourceBackedRefreshState::Queued
                )
            })
            .filter_map(|attempt| projected_job_json(state, &attempt.request_id)),
    );
    let Some(object) = job.as_object_mut() else {
        return job;
    };
    if successors.is_empty() {
        object.remove(QUEUED_SUCCESSORS_FIELD);
    } else {
        object.insert(QUEUED_SUCCESSORS_FIELD.to_owned(), Value::Array(successors));
    }
    job
}

pub(super) fn recover_queued_successors(job: &Value) -> Result<Vec<SourceBackedRefreshAttempt>> {
    let Some(successors) = job.get(QUEUED_SUCCESSORS_FIELD) else {
        return Ok(Vec::new());
    };
    let successors = successors
        .as_array()
        .ok_or_else(|| anyhow!("durable source refresh successors must be an array"))?;
    let root_state = job
        .get("request_state")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("durable source refresh job has no request state"))?;
    match root_state {
        "admission_pending" | "queued" | "running" | "failed" | "published" => {}
        _ => bail!("durable source refresh job has an invalid request state"),
    }
    if successors.len().saturating_add(1) > SOURCE_REFRESH_ACTIVE_PENDING_LIMIT {
        bail!("durable source refresh successor queue exceeds its bounded capacity");
    }
    let root_request_id = job
        .get("request_id")
        .and_then(Value::as_str)
        .filter(|request_id| !request_id.is_empty())
        .ok_or_else(|| anyhow!("durable source refresh job has no request ID"))?;
    let mut request_ids = BTreeSet::from([root_request_id.to_owned()]);
    let mut recovered = Vec::with_capacity(successors.len());
    for successor in successors {
        if successor.get(QUEUED_SUCCESSORS_FIELD).is_some() {
            bail!("durable source refresh successor queue must not be nested");
        }
        let attempt = recover_pending_attempt(
            successor,
            optional_generation(successor.get("previous_generation"))?,
            "successor",
            false,
        )?;
        if !request_ids.insert(attempt.request_id.clone()) {
            bail!("durable source refresh successor request ID is duplicated");
        }
        recovered.push(attempt);
    }
    Ok(recovered)
}

pub(super) fn recover_queued_root(
    job: &Value,
    previous_generation: Option<String>,
) -> Result<SourceBackedRefreshAttempt> {
    recover_pending_attempt(job, previous_generation, "root", true)
}

fn recover_pending_attempt(
    job: &Value,
    previous_generation: Option<String>,
    role: &str,
    is_root: bool,
) -> Result<SourceBackedRefreshAttempt> {
    let request_state = job.get("request_state").and_then(Value::as_str);
    if !(matches!(request_state, Some("admission_pending" | "queued"))
        || is_root && request_state == Some("running"))
    {
        bail!("durable source refresh {role} is not queued");
    }
    let request_id = job
        .get("request_id")
        .and_then(Value::as_str)
        .filter(|request_id| !request_id.is_empty())
        .ok_or_else(|| anyhow!("durable source refresh {role} has no request ID"))?;
    let operation = SourceBackedRefreshOperation::from_request_json(job)
        .with_context(|| format!("recover durable source refresh {role} operation"))?;
    let intent = recover_refresh_intent(job, operation, false, is_root)
        .with_context(|| format!("recover durable source refresh {role} intent"))?;
    let daemon_mode = job
        .get("daemon_mode")
        .and_then(Value::as_str)
        .and_then(canonical_daemon_mode)
        .ok_or_else(|| anyhow!("durable source refresh {role} has invalid daemon mode"))?;
    let trigger = recover_static_job_field(
        job,
        role,
        "trigger",
        &["setup", "search", "periodic", "import"],
    )?;
    let trigger_provenance = recover_static_job_field(
        job,
        role,
        "trigger_provenance",
        &[
            "manual",
            "autostart",
            "setup_command",
            "import_command",
            "automatic_provider",
            "daemon_scheduler",
            "explicit_source_catalog",
        ],
    )?;
    let refresh_scope = refresh_scope_from_json(job.get("refresh_scope"))
        .with_context(|| format!("recover durable source refresh {role} scope"))?;
    let metadata = SourceRefreshRuntimeMetadata {
        operation,
        daemon_mode,
        trigger,
        trigger_provenance,
    };
    let mut attempt = new_refresh_attempt(previous_generation, metadata, intent, refresh_scope);
    attempt.request_id = request_id.to_owned();
    attempt.reconciliation_demand = recover_reconciliation_demand(job, operation)?;
    let _legacy_physical_attempt_id = optional_pending_string(job, "physical_attempt_id")?;
    attempt.state = if request_state == Some("admission_pending") {
        SourceBackedRefreshState::AdmissionPending
    } else {
        SourceBackedRefreshState::Queued
    };
    attempt.request_fingerprint = optional_sha256(job, "request_fingerprint")?;
    attempt.automatic_retry_checkpoints =
        request_lifecycle::recover_automatic_retry_checkpoints(job)?;
    request_lifecycle::rearm_build_changed_automatic_retry_checkpoints(&mut attempt);
    attempt.admission_durability_indeterminate =
        recover_admission_durability(job, &format!("durable source refresh {role}"))?;
    let _legacy_coalesced_into_request_id = job
        .get("coalesced_into_request_id")
        .filter(|value| !value.is_null())
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("durable source refresh {role} has invalid predecessor ID"))
        })
        .transpose()?;
    if let Some(requested_at_ms) = job
        .get("requested_at_ms")
        .or_else(|| job.get("last_run_at_ms"))
    {
        attempt.requested_at_ms = requested_at_ms.as_i64().ok_or_else(|| {
            anyhow!("durable source refresh {role} has invalid request timestamp")
        })?;
    }
    if let Some(coalesced_requests) = job.get("coalesced_requests") {
        attempt.coalesced_requests = coalesced_requests.as_u64().ok_or_else(|| {
            anyhow!("durable source refresh {role} has invalid coalesced request count")
        })?;
    }
    if let Some(legacy_coalesced_logical_demands) = job.get("coalesced_logical_demands") {
        legacy_coalesced_logical_demands.as_u64().ok_or_else(|| {
            anyhow!("durable source refresh {role} has invalid logical demand count")
        })?;
    }
    Ok(attempt)
}

fn optional_pending_string(job: &Value, field: &str) -> Result<Option<String>> {
    match job.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value.clone())),
        Some(_) => bail!("durable source refresh has invalid `{field}`"),
    }
}

fn optional_sha256(job: &Value, field: &str) -> Result<Option<String>> {
    job.get(field)
        .filter(|value| !value.is_null())
        .map(|value| {
            value
                .as_str()
                .filter(|value| is_sha256_identity(value))
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("durable source refresh has invalid `{field}`"))
        })
        .transpose()
}

fn recover_static_job_field(
    job: &Value,
    role: &str,
    field: &str,
    accepted: &[&'static str],
) -> Result<&'static str> {
    let value = job
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("durable source refresh {role} has no `{field}`"))?;
    accepted
        .iter()
        .copied()
        .find(|accepted| *accepted == value)
        .ok_or_else(|| anyhow!("durable source refresh {role} has invalid `{field}`"))
}

pub(super) fn install_recovered_successors(
    state: &mut CoreRefreshEngineState,
    successors: Vec<SourceBackedRefreshAttempt>,
) -> Result<()> {
    for successor in successors {
        if find_attempt(state, &successor.request_id).is_some() {
            bail!("durable source refresh successor conflicts with an active request");
        }
        let request_id = successor.request_id.clone();
        if state.active_request_id.is_none() {
            state.active_request_id = Some(request_id);
        } else {
            state.pending_request_ids.push_back(request_id);
        }
        state.attempts.push_back(successor);
    }
    Ok(())
}

#[cfg(test)]
mod cadence_tests {
    use super::*;

    #[test]
    fn durable_progress_is_throttled_independently_of_live_updates() {
        assert_eq!(DURABLE_PROGRESS_PERSIST_INTERVAL, StdDuration::from_secs(1));
        let started = StdInstant::now();
        assert!(should_persist_progress(None, None, "request-a", started));
        assert!(should_persist_progress(
            Some("request-a"),
            Some(started),
            "request-b",
            started + StdDuration::from_millis(1),
        ));
        assert!(!should_persist_progress(
            Some("request-a"),
            Some(started),
            "request-a",
            started + StdDuration::from_millis(999),
        ));
        assert!(should_persist_progress(
            Some("request-a"),
            Some(started),
            "request-a",
            started + DURABLE_PROGRESS_PERSIST_INTERVAL,
        ));
    }
}
