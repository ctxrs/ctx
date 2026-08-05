use super::*;

const QUEUED_SUCCESSORS_FIELD: &str = "queued_successors";
const LOGICAL_DEMAND_FIELD: &str = "logical_demand";
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
        let requested_terminal = !requested_attempt.state.is_active();
        let durable_request_id = if requested_terminal {
            request_id
        } else {
            state
                .pending_scheduler_retry_root_id
                .as_deref()
                .or_else(|| {
                    state
                        .pending_terminal_persistence
                        .as_ref()
                        .map(|pending| pending.request_id.as_str())
                })
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
        self.write_status(data_root, &job)
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
        let job = job_with_queued_successors(&state, job);
        // Serialize retry metadata against the same queue authority as IPC
        // admission so an older scheduler snapshot cannot erase a successor.
        self.write_status(data_root, &job)?;
        if state.pending_scheduler_retry_root_id.as_deref() == Some(request_id.as_str()) {
            state.pending_scheduler_retry_root_id = None;
        }
        Ok(job)
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
        let durable_root = state
            .pending_scheduler_retry_root_id
            .as_deref()
            .or_else(|| {
                state
                    .pending_terminal_persistence
                    .as_ref()
                    .map(|pending| pending.request_id.as_str())
            })
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

fn update_progress(
    state: &mut CoreRefreshEngineState,
    request_id: &str,
    update: SourceBackedRefreshProgressUpdate,
) -> Option<Value> {
    let attempt = find_attempt_mut(state, request_id)?;
    if attempt.state != SourceBackedRefreshState::Running {
        return None;
    }
    attempt.progress = SourceBackedRefreshProgress {
        phase: update.phase,
        completed_sources: update.completed_sources,
        total_sources: update.total_sources,
        current_source: update.current_source,
        completed_records: update.completed_records,
        completed_bytes: update.completed_bytes,
        current_source_progress: update.current_source_progress,
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

pub(super) fn durable_job_json(state: &CoreRefreshEngineState, request_id: &str) -> Option<Value> {
    projected_job_json(state, request_id).map(|job| job_with_queued_successors(state, job))
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
                successors.push(job_with_logical_demand(state, job));
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
            .filter_map(|attempt| projected_job_json(state, &attempt.request_id))
            .map(|job| job_with_logical_demand(state, job)),
    );
    let Some(object) = job.as_object_mut() else {
        return job;
    };
    if successors.is_empty() {
        object.remove(QUEUED_SUCCESSORS_FIELD);
    } else {
        object.insert(QUEUED_SUCCESSORS_FIELD.to_owned(), Value::Array(successors));
    }
    job_with_logical_demand(state, job)
}

fn job_with_logical_demand(state: &CoreRefreshEngineState, mut job: Value) -> Value {
    let request_id = job
        .get("request_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let Some(object) = job.as_object_mut() else {
        return job;
    };
    match request_id
        .as_deref()
        .and_then(|request_id| state.manual_all_continuations.get(request_id))
    {
        Some(continuation) => {
            object.insert(LOGICAL_DEMAND_FIELD.to_owned(), continuation.to_json());
        }
        None => {
            object.remove(LOGICAL_DEMAND_FIELD);
        }
    }
    job
}

pub(super) fn recover_logical_demand_continuations(
    job: &Value,
) -> Result<BTreeMap<String, ManualAllContinuation>> {
    let mut recovered = BTreeMap::new();
    let mut recover = |candidate: &Value| -> Result<()> {
        let Some(value) = candidate.get(LOGICAL_DEMAND_FIELD) else {
            return Ok(());
        };
        let request_id = candidate
            .get("request_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("logical refresh demand has no request ID"))?
            .to_owned();
        if recovered
            .insert(request_id, ManualAllContinuation::from_json(value)?)
            .is_some()
        {
            bail!("logical refresh demand request ID is duplicated");
        }
        Ok(())
    };
    recover(job)?;
    if let Some(successors) = job.get(QUEUED_SUCCESSORS_FIELD) {
        for successor in successors
            .as_array()
            .ok_or_else(|| anyhow!("durable source refresh successors must be an array"))?
        {
            recover(successor)?;
        }
    }
    Ok(recovered)
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
    if !matches!(request_state, Some("admission_pending" | "queued"))
        && !(is_root && request_state == Some("running"))
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
    // Pre-overlay periodic roots can carry obsolete catalog-shaped data. It
    // was never request authority for refresh operations, so retain that
    // compatibility without weakening import or successor validation.
    let requested_catalog = if is_root && operation == SourceBackedRefreshOperation::Refresh {
        None
    } else {
        job.get("requested_explicit_source_catalog")
            .filter(|value| !value.is_null())
            .map(ExplicitSourceCatalogAuthority::from_json)
            .transpose()
            .with_context(|| format!("recover durable source refresh {role} explicit authority"))?
    };
    match (operation, requested_catalog.is_some()) {
        (SourceBackedRefreshOperation::Import, false) => {
            bail!("durable import {role} has no explicit source authority")
        }
        (SourceBackedRefreshOperation::Refresh, true) => {
            bail!("durable refresh {role} carries explicit source authority")
        }
        _ => {}
    }
    let daemon_mode = job
        .get("daemon_mode")
        .and_then(Value::as_str)
        .and_then(canonical_daemon_mode)
        .ok_or_else(|| anyhow!("durable source refresh {role} has invalid daemon mode"))?;
    let trigger =
        recover_static_job_field(job, role, "trigger", &["search", "periodic", "import"])?;
    let trigger_provenance = recover_static_job_field(
        job,
        role,
        "trigger_provenance",
        &[
            "manual",
            "autostart",
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
    let mut attempt = new_refresh_attempt(
        previous_generation,
        metadata,
        requested_catalog,
        refresh_scope,
    );
    attempt.request_id = request_id.to_owned();
    attempt.physical_attempt_id = optional_pending_string(job, "physical_attempt_id")?;
    attempt.state = if request_state == Some("admission_pending") {
        SourceBackedRefreshState::AdmissionPending
    } else {
        SourceBackedRefreshState::Queued
    };
    attempt.fresh_after_admitted_snapshot = match job.get("fresh_after_admitted_snapshot") {
        None => false,
        Some(value) => value.as_bool().ok_or_else(|| {
            anyhow!("durable source refresh {role} has invalid freshness requirement")
        })?,
    };
    attempt.request_fingerprint = optional_sha256(job, "request_fingerprint")?;
    attempt.admission_durability_indeterminate =
        recover_admission_durability(job, &format!("durable source refresh {role}"))?;
    attempt.coalesced_into_request_id = job
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
    if attempt.physical_attempt_id.is_none() {
        attempt.physical_attempt_id = Some(
            attempt
                .coalesced_into_request_id
                .clone()
                .unwrap_or_else(|| attempt.request_id.clone()),
        );
    }
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
    if let Some(coalesced_logical_demands) = job.get("coalesced_logical_demands") {
        attempt.coalesced_logical_demands =
            coalesced_logical_demands.as_u64().ok_or_else(|| {
                anyhow!("durable source refresh {role} has invalid logical demand count")
            })?;
    }
    if attempt.state == SourceBackedRefreshState::AdmissionPending
        && !attempt.fresh_after_admitted_snapshot
    {
        bail!("durable admission-pending source refresh has no freshness requirement");
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
