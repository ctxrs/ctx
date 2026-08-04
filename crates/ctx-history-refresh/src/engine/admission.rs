use super::*;
use sha2::{Digest as _, Sha256};

fn request_fingerprint(
    operation: SourceBackedRefreshOperation,
    requested_catalog: Option<&ExplicitSourceCatalogAuthority>,
    refresh_scope: &SourceBackedRefreshScope,
    admission: SourceRefreshAdmissionRequirement,
) -> Result<String> {
    let authority = compact_json(json!({
        "operation": operation.as_str(),
        "explicit_source_catalog": requested_catalog
            .map(ExplicitSourceCatalogAuthority::to_json),
        "fresh_after_admitted_snapshot": admission
            == SourceRefreshAdmissionRequirement::FreshAfterAdmittedSnapshot,
        "refresh_scope": refresh_scope_json(refresh_scope),
    }));
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&authority)?)
    ))
}

struct SourceRefreshAdmissionReservation<'a> {
    data_root: &'a Path,
    previous_generation: Option<String>,
    metadata: SourceRefreshRuntimeMetadata,
    requested_catalog: Option<ExplicitSourceCatalogAuthority>,
    refresh_scope: SourceBackedRefreshScope,
    logical_demand: SourceRefreshLogicalDemand,
    defer_admission_until_response: bool,
}

impl CoreRefreshEngine {
    pub fn submit(
        &self,
        data_root: &Path,
        submission: RefreshSubmission,
    ) -> Result<RefreshAdmission> {
        let admission = if submission.fresh_after_admitted_snapshot {
            SourceRefreshAdmissionRequirement::FreshAfterAdmittedSnapshot
        } else {
            SourceRefreshAdmissionRequirement::AttachEquivalent
        };
        if submission.maintenance_wake {
            let response =
                self.background_maintenance_wake_response(data_root, submission.request_id)?;
            return Ok(RefreshAdmission::new(
                RefreshStatus::from_schema_v1_fields(response),
                None,
            ));
        }
        let fingerprint = request_fingerprint(
            submission.operation,
            submission.explicit_source_catalog.as_ref(),
            &submission.refresh_scope,
            admission,
        )?;
        let previous_generation = self.observed_published_generation(data_root)?;
        let metadata = self.runtime.metadata(data_root, submission.operation);
        let response = self.reserve_ipc_request(SourceRefreshAdmissionReservation {
            data_root,
            previous_generation,
            metadata,
            requested_catalog: submission.explicit_source_catalog,
            refresh_scope: submission.refresh_scope,
            logical_demand: SourceRefreshLogicalDemand {
                admission,
                route_observations: BTreeMap::new(),
                request_id: Some(submission.request_id),
                request_fingerprint: Some(fingerprint),
                admission_pending: admission
                    == SourceRefreshAdmissionRequirement::FreshAfterAdmittedSnapshot,
            },
            defer_admission_until_response: true,
        });
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                if let Some(queue_full) = error.downcast_ref::<SourceBackedRefreshQueueFull>() {
                    queue_full.to_json()
                } else if let Some(conflict) =
                    error.downcast_ref::<SourceBackedRefreshIdempotencyConflict>()
                {
                    conflict.to_json()
                } else {
                    return Err(error);
                }
            }
        };
        let response_barrier = (response.get("request_state").and_then(Value::as_str)
            == Some("admission_pending"))
        .then(|| {
            response
                .get("request_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .map(AdmissionResponseBarrier::new)
        })
        .flatten();
        Ok(RefreshAdmission::new(
            RefreshStatus::from_schema_v1_fields(response),
            response_barrier,
        ))
    }

    fn reserve_ipc_request(
        &self,
        reservation: SourceRefreshAdmissionReservation<'_>,
    ) -> Result<Value> {
        let SourceRefreshAdmissionReservation {
            data_root,
            previous_generation,
            metadata,
            requested_catalog,
            refresh_scope,
            logical_demand,
            defer_admission_until_response,
        } = reservation;
        let logical_request_id = logical_demand
            .request_id
            .as_deref()
            .ok_or_else(|| anyhow!("daemon source refresh logical request ID is missing"))?;
        let request_fingerprint = logical_demand.request_fingerprint.as_ref();
        let mut state = self.lock_state();
        if let Some(existing) = find_attempt(&state, logical_request_id) {
            if existing.request_fingerprint.as_ref() != request_fingerprint {
                return Err(SourceBackedRefreshIdempotencyConflict {
                    request_id: logical_request_id.to_owned(),
                }
                .into());
            }
            if existing.admission_durability_indeterminate {
                self.reconfirm_retained_admission_locked(data_root, &mut state, logical_request_id);
            }
            let existing = find_attempt(&state, logical_request_id).ok_or_else(|| {
                anyhow!("source refresh request `{logical_request_id}` disappeared during replay")
            })?;
            let response = existing.to_json();
            if defer_admission_until_response
                && existing.state == SourceBackedRefreshState::AdmissionPending
            {
                increment_response_barrier(&mut state, logical_request_id);
            }
            return Ok(response);
        }

        let snapshot = AdmissionReservationSnapshot::capture(&state);
        let response = match Self::enqueue_with_catalog_metadata_locked(
            &mut state,
            previous_generation,
            metadata,
            requested_catalog,
            refresh_scope,
            logical_demand,
        ) {
            Ok(response) => response,
            Err(error) => {
                snapshot.restore(&mut state);
                return Err(error);
            }
        };
        let request_id = response
            .get("request_id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("queued source refresh has no request ID"))?
            .to_owned();
        if defer_admission_until_response
            && find_attempt(&state, &request_id)
                .is_some_and(|attempt| attempt.state == SourceBackedRefreshState::AdmissionPending)
        {
            increment_response_barrier(&mut state, &request_id);
        }
        let attempt = find_attempt_mut(&mut state, &request_id)
            .ok_or_else(|| anyhow!("source refresh request `{request_id}` is unknown"))?;
        // Persist the conservative marker in the first replace. If durability
        // confirmation fails after replacement, a crash must recover both the
        // admitted request and the fact that its acknowledgement was uncertain.
        attempt.admission_durability_indeterminate = true;
        let durable_root_id = durable_request_id(&state, &request_id);
        let job = durable_job_json(&state, durable_root_id)
            .ok_or_else(|| anyhow!("source refresh request `{durable_root_id}` is unknown"))?;
        let retained_error = match self.write_durable_admission_status(data_root, &job) {
            DurableAdmissionPersistence::Confirmed => None,
            DurableAdmissionPersistence::Retained(error) => Some(error),
            DurableAdmissionPersistence::Failed(error) => {
                snapshot.restore(&mut state);
                return Err(error
                    .context("persist durable source refresh admission before acknowledgement"));
            }
        };
        if retained_error.is_some() {
            let attempt = find_attempt(&state, &request_id).ok_or_else(|| {
                anyhow!("retained source refresh request `{request_id}` is unknown")
            })?;
            let response = attempt.to_json();
            trim_terminal_attempt_history(&mut state);
            return Ok(response);
        }
        let attempt = find_attempt_mut(&mut state, &request_id)
            .ok_or_else(|| anyhow!("confirmed source refresh request `{request_id}` is unknown"))?;
        attempt.admission_durability_indeterminate = false;
        let durable_request_id = durable_request_id(&state, &request_id);
        if let Some(confirmed_job) = durable_job_json(&state, durable_request_id) {
            // The marker-bearing image already proved the request durable. A
            // failed cleanup leaves that conservative image recoverable.
            let _ = self.write_durable_admission_status(data_root, &confirmed_job);
        }
        trim_terminal_attempt_history(&mut state);
        Ok(find_attempt(&state, &request_id)
            .map(SourceBackedRefreshAttempt::to_json)
            .unwrap_or(response))
    }

    fn reconfirm_retained_admission_locked(
        &self,
        data_root: &Path,
        state: &mut CoreRefreshEngineState,
        request_id: &str,
    ) {
        let durable_id = durable_request_id(state, request_id);
        let Some(retained_job) = durable_job_json(state, durable_id) else {
            return;
        };
        if !matches!(
            self.write_durable_admission_status(data_root, &retained_job),
            DurableAdmissionPersistence::Confirmed
        ) {
            return;
        }
        let Some(attempt) = find_attempt_mut(state, request_id) else {
            return;
        };
        attempt.admission_durability_indeterminate = false;
        let durable_request_id = durable_request_id(state, request_id);
        let Some(confirmed_job) = durable_job_json(state, durable_request_id) else {
            return;
        };
        // The marker-bearing image was confirmed above, so a cleanup failure
        // may leave the conservative marker on disk but cannot lose admission.
        let _ = self.write_durable_admission_status(data_root, &confirmed_job);
    }

    pub(crate) fn release_admission_response(&self, request_id: &str) {
        let mut state = self.lock_state();
        let Some(remaining) = state.unacknowledged_admissions.get_mut(request_id) else {
            return;
        };
        *remaining = remaining.saturating_sub(1);
        if *remaining == 0 {
            state.unacknowledged_admissions.remove(request_id);
        }
    }

    pub fn resolve_pending_admission(
        &self,
        data_root: &Path,
    ) -> Result<Option<SourceBackedRefreshRun>> {
        self.resolve_active_pending_admission(data_root)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn has_pending_admission(&self) -> bool {
        self.lock_state()
            .attempts
            .iter()
            .any(|attempt| attempt.state == SourceBackedRefreshState::AdmissionPending)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn prepare_next_pending_admission(&self, data_root: &Path) -> Result<bool> {
        let Some((request_id, requested_catalog)) = self.claim_next_pending_admission() else {
            return Ok(false);
        };
        // Corpus-scale discovery must run without a coordinator or admission
        // lock so status, ping, and additional bounded admissions stay live.
        let discovery = self.runtime.discovery_context(data_root)?;
        let observations = (self.admission_fence)(
            &discovery,
            self.journal.as_ref(),
            data_root,
            requested_catalog.as_ref(),
        );
        self.complete_claimed_pending_admission(data_root, &request_id, observations)?;
        Ok(true)
    }

    pub(super) fn resolve_active_pending_admission(
        &self,
        data_root: &Path,
    ) -> Result<Option<SourceBackedRefreshRun>> {
        let Some((request_id, requested_catalog)) = self.claim_active_pending_admission() else {
            return Ok(None);
        };
        // Corpus-scale discovery must run without a coordinator or admission
        // lock so status, ping, and additional bounded admissions stay live.
        let discovery = self.runtime.discovery_context(data_root)?;
        let observations = (self.admission_fence)(
            &discovery,
            self.journal.as_ref(),
            data_root,
            requested_catalog.as_ref(),
        );
        self.complete_claimed_pending_admission(data_root, &request_id, observations)
    }

    pub(super) fn active_request_admission_pending(&self) -> bool {
        let state = self.lock_state();
        state
            .active_request_id
            .as_deref()
            .and_then(|request_id| find_attempt(&state, request_id))
            .is_some_and(|attempt| attempt.state == SourceBackedRefreshState::AdmissionPending)
    }

    pub(super) fn admission_persistence_retry_run(
        &self,
        error: anyhow::Error,
    ) -> Option<SourceBackedRefreshRun> {
        let mut state = self.lock_state();
        let request_id = state.active_request_id.clone()?;
        let attempt = find_attempt_mut(&mut state, &request_id)?;
        if attempt.state != SourceBackedRefreshState::AdmissionPending {
            return None;
        }
        attempt.last_error = Some(format!("persist source refresh admission: {error:#}"));
        let scope = attempt.refresh_scope.clone();
        let durable_request_id = durable_request_id(&state, &request_id);
        let mut job = durable_job_json(&state, durable_request_id)?;
        job["retryable"] = Value::Bool(true);
        Some(SourceBackedRefreshRun {
            job,
            did_work: false,
            failed: false,
            terminal_persistence_pending: true,
            scope,
            coverage_certificate: None,
        })
    }

    fn claim_active_pending_admission(
        &self,
    ) -> Option<(String, Option<ExplicitSourceCatalogAuthority>)> {
        let mut state = self.lock_state();
        let request_id = state.active_request_id.clone()?;
        let attempt = find_attempt(&state, &request_id)?;
        if attempt.state != SourceBackedRefreshState::AdmissionPending
            || state.unacknowledged_admissions.contains_key(&request_id)
            || state.admission_resolutions_in_flight.contains(&request_id)
        {
            return None;
        }
        let requested_catalog = attempt.requested_explicit_source_catalog.clone();
        state
            .admission_resolutions_in_flight
            .insert(request_id.clone());
        Some((request_id, requested_catalog))
    }

    #[cfg(any(test, feature = "test-support"))]
    fn claim_next_pending_admission(
        &self,
    ) -> Option<(String, Option<ExplicitSourceCatalogAuthority>)> {
        let mut state = self.lock_state();
        let request_id = state
            .attempts
            .iter()
            .find(|attempt| {
                attempt.state == SourceBackedRefreshState::AdmissionPending
                    && !state
                        .unacknowledged_admissions
                        .contains_key(&attempt.request_id)
                    && !state
                        .admission_resolutions_in_flight
                        .contains(&attempt.request_id)
            })?
            .request_id
            .clone();
        let requested_catalog = find_attempt(&state, &request_id)
            .and_then(|attempt| attempt.requested_explicit_source_catalog.clone());
        state
            .admission_resolutions_in_flight
            .insert(request_id.clone());
        Some((request_id, requested_catalog))
    }

    fn complete_claimed_pending_admission(
        &self,
        data_root: &Path,
        request_id: &str,
        observations: Result<BTreeMap<SourceRouteIdentity, Option<String>>>,
    ) -> Result<Option<SourceBackedRefreshRun>> {
        let mut state = self.lock_state();
        state.admission_resolutions_in_flight.remove(request_id);
        if find_attempt(&state, request_id)
            .is_none_or(|attempt| attempt.state != SourceBackedRefreshState::AdmissionPending)
        {
            return Ok(None);
        }
        match observations.and_then(validate_admission_observations) {
            Ok(observations) => {
                self.persist_resolved_admission(data_root, &mut state, request_id, observations)?;
                Ok(None)
            }
            Err(error) => self.persist_failed_admission(data_root, &mut state, request_id, error),
        }
    }

    fn persist_resolved_admission(
        &self,
        data_root: &Path,
        state: &mut CoreRefreshEngineState,
        request_id: &str,
        mut observations: BTreeMap<SourceRouteIdentity, Option<String>>,
    ) -> Result<()> {
        let snapshot = AdmissionResolutionSnapshot::capture(state);
        if state.manual_all_continuations.contains_key(request_id) {
            let known_route_ids = state.known_route_ids.clone();
            if observations.len().saturating_add(known_route_ids.len())
                > SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT.saturating_mul(2)
            {
                bail!("source refresh admission fence exceeds its bounded route capacity");
            }
            let ledger_eligible_routes = known_route_ids
                .iter()
                .filter(|route| observations.get(*route).is_none_or(Option::is_none))
                .cloned()
                .collect::<BTreeSet<_>>();
            for route in &known_route_ids {
                observations.entry(route.clone()).or_insert(None);
            }
            if observations.len() > SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT {
                bail!("source refresh admission fence exceeds its bounded route capacity");
            }
            let admission_event_watermarks = observations
                .keys()
                .filter_map(|route| {
                    state
                        .route_event_watermarks
                        .get(route)
                        .copied()
                        .map(|watermark| (route.clone(), watermark))
                })
                .collect::<BTreeMap<_, _>>();
            let predecessor_request_id = state
                .manual_all_continuations
                .get(request_id)
                .map(|continuation| continuation.predecessor_request_id.clone())
                .ok_or_else(|| {
                    anyhow!("source refresh request `{request_id}` lost its predecessor")
                })?;
            let predecessor_event_watermarks = state
                .route_admission_watermarks
                .get(&predecessor_request_id)
                .cloned()
                .or_else(|| {
                    state
                        .manual_all_continuations
                        .get(request_id)
                        .map(|continuation| continuation.predecessor_event_watermarks.clone())
                })
                .unwrap_or_default();
            let continuation = state
                .manual_all_continuations
                .get_mut(request_id)
                .ok_or_else(|| {
                    anyhow!("source refresh request `{request_id}` lost its continuation")
                })?;
            continuation.admission_pending = false;
            continuation.admission_route_observations = observations;
            continuation.ledger_eligible_routes = ledger_eligible_routes;
            continuation.admission_event_watermarks = admission_event_watermarks;
            continuation.predecessor_event_watermarks = predecessor_event_watermarks;
        }
        let attempt = find_attempt_mut(state, request_id)
            .ok_or_else(|| anyhow!("source refresh request `{request_id}` is unknown"))?;
        attempt.state = SourceBackedRefreshState::Queued;
        attempt.progress.phase = "queued".to_owned();
        attempt.last_error = None;
        apply_finished_predecessor_coverage(state, request_id);
        let durable_request_id = durable_request_id(state, request_id);
        let job = durable_job_json(state, durable_request_id)
            .ok_or_else(|| anyhow!("source refresh request `{durable_request_id}` is unknown"))?;
        if let Err(error) = self.write_status(data_root, &job) {
            snapshot.restore(state);
            return Err(error.context("persist resolved source refresh admission before execution"));
        }
        Ok(())
    }

    fn persist_failed_admission(
        &self,
        data_root: &Path,
        state: &mut CoreRefreshEngineState,
        request_id: &str,
        error: anyhow::Error,
    ) -> Result<Option<SourceBackedRefreshRun>> {
        let snapshot = AdmissionResolutionSnapshot::capture(state);
        state.manual_all_continuations.remove(request_id);
        let (scope, last_error) = {
            let attempt = find_attempt_mut(state, request_id)
                .ok_or_else(|| anyhow!("source refresh request `{request_id}` is unknown"))?;
            let last_error = format!("source refresh admission fence failed: {error:#}");
            attempt.state = SourceBackedRefreshState::Failed;
            attempt.finished_at_ms = Some(utc_now().timestamp_millis());
            attempt.progress.phase = "failed".to_owned();
            attempt.last_error = Some(last_error.clone());
            (attempt.refresh_scope.clone(), last_error)
        };
        let job = durable_job_json(state, request_id)
            .ok_or_else(|| anyhow!("source refresh request `{request_id}` is unknown"))?;
        if let Err(persist_error) = self.write_status(data_root, &job) {
            snapshot.restore(state);
            return Err(persist_error.context("persist terminal source refresh admission failure"));
        }
        state.pending_scheduler_retry_root_id = Some(request_id.to_owned());
        let observed_generation = state.current_published_generation.clone();
        advance_after_terminal_attempt(state, request_id, observed_generation);
        trim_terminal_attempt_history(state);
        debug_assert_eq!(job["last_error"], last_error);
        Ok(Some(SourceBackedRefreshRun {
            job,
            did_work: false,
            failed: true,
            terminal_persistence_pending: false,
            scope,
            coverage_certificate: None,
        }))
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn complete_pending_admission_for_test(
        &self,
        data_root: &Path,
        request_id: &str,
        observations: BTreeMap<SourceRouteIdentity, Option<String>>,
    ) -> Result<()> {
        let mut state = self.lock_state();
        if find_attempt(&state, request_id)
            .is_none_or(|attempt| attempt.state != SourceBackedRefreshState::AdmissionPending)
        {
            return Ok(());
        }
        self.persist_resolved_admission(
            data_root,
            &mut state,
            request_id,
            validate_admission_observations(observations)?,
        )
    }
}

fn validate_admission_observations(
    observations: BTreeMap<SourceRouteIdentity, Option<String>>,
) -> Result<BTreeMap<SourceRouteIdentity, Option<String>>> {
    if observations.len() > SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT {
        bail!("source refresh admission fence exceeds its bounded route capacity");
    }
    if observations
        .values()
        .flatten()
        .any(|observation| !is_sha256_identity(observation))
    {
        bail!("source refresh admission fence returned an invalid route observation");
    }
    Ok(observations)
}

fn increment_response_barrier(state: &mut CoreRefreshEngineState, request_id: &str) {
    state
        .unacknowledged_admissions
        .entry(request_id.to_owned())
        .and_modify(|count| *count = count.saturating_add(1))
        .or_insert(1);
}

fn durable_request_id<'a>(state: &'a CoreRefreshEngineState, request_id: &'a str) -> &'a str {
    if find_attempt(state, request_id).is_some_and(|attempt| !attempt.state.is_active()) {
        return request_id;
    }
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
}

fn apply_finished_predecessor_coverage(state: &mut CoreRefreshEngineState, request_id: &str) {
    let Some(continuation) = state.manual_all_continuations.get(request_id).cloned() else {
        return;
    };
    if !continuation.predecessor_finished || continuation.admission_pending {
        return;
    }
    let Some(predecessor) = find_attempt(state, &continuation.predecessor_request_id).cloned()
    else {
        return;
    };
    let Some(receipt) = predecessor.receipt.as_ref() else {
        return;
    };
    let continuation = state
        .manual_all_continuations
        .get_mut(request_id)
        .expect("logical demand continuation");
    for (route, admission_observation) in &continuation.admission_route_observations {
        let covered = !continuation.invalidated_routes.contains(route)
            && continuation.admission_event_watermarks.get(route)
                == continuation.predecessor_event_watermarks.get(route)
            && admission_observation.as_ref().is_some_and(|admitted| {
                predecessor.route_observations.get(route) == Some(admitted)
                    && receipt.route_results.iter().any(|result| {
                        result.route_identity == route.as_str() && result.outcome.is_success()
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
    continuation.covered_removed_source_count = receipt.current.removed_source_count;
    continuation.covered_timings = predecessor.timings.unwrap_or_default();
}

struct AdmissionReservationSnapshot {
    active_request_id: Option<String>,
    pending_request_ids: VecDeque<String>,
    attempts: VecDeque<SourceBackedRefreshAttempt>,
    continuations: BTreeMap<String, ManualAllContinuation>,
    response_barriers: BTreeMap<String, usize>,
}

impl AdmissionReservationSnapshot {
    fn capture(state: &CoreRefreshEngineState) -> Self {
        Self {
            active_request_id: state.active_request_id.clone(),
            pending_request_ids: state.pending_request_ids.clone(),
            attempts: state.attempts.clone(),
            continuations: state.manual_all_continuations.clone(),
            response_barriers: state.unacknowledged_admissions.clone(),
        }
    }

    fn restore(self, state: &mut CoreRefreshEngineState) {
        state.active_request_id = self.active_request_id;
        state.pending_request_ids = self.pending_request_ids;
        state.attempts = self.attempts;
        state.manual_all_continuations = self.continuations;
        state.unacknowledged_admissions = self.response_barriers;
    }
}

struct AdmissionResolutionSnapshot {
    attempts: VecDeque<SourceBackedRefreshAttempt>,
    continuations: BTreeMap<String, ManualAllContinuation>,
}

impl AdmissionResolutionSnapshot {
    fn capture(state: &CoreRefreshEngineState) -> Self {
        Self {
            attempts: state.attempts.clone(),
            continuations: state.manual_all_continuations.clone(),
        }
    }

    fn restore(self, state: &mut CoreRefreshEngineState) {
        state.attempts = self.attempts;
        state.manual_all_continuations = self.continuations;
    }
}
