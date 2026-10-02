use super::*;
use sha2::{Digest as _, Sha256};
mod queued_admission;
mod scope_resolution;
mod state_helpers;
use state_helpers::*;
#[derive(Clone)]
struct PendingAdmissionClaim {
    request_id: String,
    intent: RefreshIntent,
    persisted_scope: SourceBackedRefreshScope,
    watch_catalog_revision: u64,
    route_event_watermarks: BTreeMap<SourceRouteIdentity, EventWatermark>,
}
pub(super) struct AdmissionObservationFence {
    watch_catalog_revision: u64,
    route_event_watermarks: BTreeMap<SourceRouteIdentity, Option<EventWatermark>>,
    route_observations: BTreeMap<SourceRouteIdentity, String>,
}
impl AdmissionObservationFence {
    pub(super) fn still_matches(&self, state: &CoreRefreshEngineState) -> bool {
        self.watch_catalog_revision == state.watch_catalog_revision
            && self.route_event_watermarks.iter().all(|(route, expected)| {
                state.route_event_watermarks.get(route).copied() == *expected
            })
    }
}

pub(super) fn admission_failure_fence_matches(
    state: &CoreRefreshEngineState,
    scope: &SourceBackedRefreshScope,
    claimed_route_event_watermarks: &BTreeMap<SourceRouteIdentity, EventWatermark>,
) -> bool {
    match scope {
        SourceBackedRefreshScope::Exact(routes) => routes.iter().all(|route| {
            state.route_event_watermarks.get(route).copied()
                == claimed_route_event_watermarks.get(route).copied()
        }),
        SourceBackedRefreshScope::All => {
            state.route_event_watermarks == *claimed_route_event_watermarks
        }
    }
}

fn request_fingerprint(request: &RefreshRequest) -> Result<String> {
    let authority = json!({
        "intent": request.intent.to_json(),
        "trigger": request.trigger.as_str(),
    });
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&authority)?)
    ))
}

struct SourceRefreshAdmissionReservation<'a> {
    data_root: &'a Path,
    previous_generation: Option<String>,
    metadata: SourceRefreshRuntimeMetadata,
    request: RefreshRequest,
    request_fingerprint: String,
    defer_admission_until_response: bool,
}

impl CoreRefreshEngine {
    pub fn maintenance_wake(&self, data_root: &Path, request_id: String) -> Result<RefreshStatus> {
        self.background_maintenance_wake_response(data_root, request_id)
            .map(RefreshStatus::from_schema_v1_fields)
    }

    pub fn submit(&self, data_root: &Path, request: RefreshRequest) -> Result<RefreshAdmission> {
        let trigger = request.trigger;
        let operation = request.intent.operation();
        let fingerprint = request_fingerprint(&request)?;
        let previous_generation = self.observed_published_generation(data_root)?;
        let mut metadata = self.runtime.metadata(data_root, operation);
        metadata.trigger = trigger.as_str();
        match (&request.intent, trigger) {
            (_, RefreshRequestTrigger::Setup) => metadata.trigger_provenance = "setup_command",
            (
                RefreshIntent::SelectedImport {
                    selection: RefreshSelection::All,
                    ..
                },
                RefreshRequestTrigger::Import,
            ) => metadata.trigger_provenance = "import_command",
            (
                RefreshIntent::SelectedImport {
                    selection: RefreshSelection::Provider(_),
                    ..
                },
                RefreshRequestTrigger::Import,
            ) => metadata.trigger_provenance = "automatic_provider",
            (
                RefreshIntent::SelectedImport {
                    selection: RefreshSelection::ExactSource(_),
                    ..
                },
                RefreshRequestTrigger::Import,
            ) => metadata.trigger_provenance = "explicit_source_catalog",
            _ => {}
        }
        let response = self.reserve_ipc_request(SourceRefreshAdmissionReservation {
            data_root,
            previous_generation,
            metadata,
            request,
            request_fingerprint: fingerprint,
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
            == Some(SourceBackedRefreshState::AdmissionPending.as_str()))
        .then(|| {
            response
                .get("request_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .map(AdmissionResponseBarrier::new)
        })
        .flatten();
        self.request_activity_generation
            .fetch_add(1, Ordering::Release);
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
            request,
            request_fingerprint,
            defer_admission_until_response,
        } = reservation;
        let logical_request_id = request.request_id;
        let intent = request.intent;
        let request_fingerprint = Some(&request_fingerprint);
        let mut state = self.lock_state();
        if let Some(existing) = find_attempt(&state, &logical_request_id) {
            if existing.request_fingerprint.as_ref() != request_fingerprint {
                return Err(SourceBackedRefreshIdempotencyConflict {
                    request_id: logical_request_id.clone(),
                }
                .into());
            }
            if existing.admission_durability_indeterminate {
                self.reconfirm_retained_admission_locked(
                    data_root,
                    &mut state,
                    &logical_request_id,
                );
            }
            let existing = find_attempt(&state, &logical_request_id).ok_or_else(|| {
                anyhow!("source refresh request `{logical_request_id}` disappeared during replay")
            })?;
            let response = projected_status_json(&state, &logical_request_id)
                .ok_or_else(|| anyhow!("replayed source refresh request disappeared"))?;
            if defer_admission_until_response
                && existing.state == SourceBackedRefreshState::AdmissionPending
            {
                increment_response_barrier(&mut state, &logical_request_id);
            }
            return Ok(response);
        }

        let snapshot = AdmissionReservationSnapshot::capture(&state);
        let response = match Self::enqueue_intent_locked(
            &mut state,
            previous_generation,
            metadata,
            intent,
            SourceBackedRefreshScope::All,
            Some(logical_request_id),
            request_fingerprint.cloned(),
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
            find_attempt(&state, &request_id).ok_or_else(|| {
                anyhow!("retained source refresh request `{request_id}` is unknown")
            })?;
            let response = projected_status_json(&state, &request_id)
                .ok_or_else(|| anyhow!("retained source refresh request disappeared"))?;
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
        Ok(projected_status_json(&state, &request_id).unwrap_or(response))
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
        let Some(claim) = self.claim_next_pending_admission() else {
            return Ok(false);
        };
        // Corpus-scale discovery must run without a coordinator or admission
        // lock so status, ping, and additional bounded admissions stay live.
        let resolution = self.resolve_pending_admission_claim(data_root, &claim);
        self.complete_claimed_pending_admission(data_root, &claim, resolution)?;
        Ok(true)
    }

    pub(super) fn resolve_active_pending_admission(
        &self,
        data_root: &Path,
    ) -> Result<Option<SourceBackedRefreshRun>> {
        let Some(claim) = self.claim_active_pending_admission() else {
            return Ok(None);
        };
        // Corpus-scale discovery must run without a coordinator or admission
        // lock so status, ping, and additional bounded admissions stay live.
        let resolution = self.resolve_pending_admission_claim(data_root, &claim);
        self.complete_claimed_pending_admission(data_root, &claim, resolution)
    }

    pub(super) fn requeue_stale_provider_root_admission(&self, data_root: &Path) -> Result<bool> {
        let admitted_config = {
            let state = self.lock_state();
            state
                .active_request_id
                .as_deref()
                .and_then(|request_id| find_attempt(&state, request_id))
                .filter(|attempt| attempt.state == SourceBackedRefreshState::Queued)
                .and_then(|attempt| attempt.admitted_authority.as_ref())
                .filter(|authority| {
                    authority.coverage()
                        == ctx_history_refresh_execution::AdmittedRefreshCoverage::CompleteCatalog
                })
                .map(|authority| {
                    (
                        authority
                            .discovery()
                            .configured_provider_roots()
                            .map(<[_]>::to_vec),
                        authority.discovery().automatic_provider_discovery(),
                    )
                })
        };
        let Some((admitted_roots, admitted_automatic)) = admitted_config else {
            return Ok(false);
        };
        let current_discovery = self.runtime.discovery_context(data_root)?;
        let current_roots = current_discovery.configured_provider_roots().to_vec();
        let current_automatic = current_discovery.automatic_provider_discovery_enabled();
        let stale_snapshot = admitted_roots
            .as_deref()
            .is_some_and(|admitted| admitted != current_roots.as_slice())
            || admitted_roots.is_none() && !current_roots.is_empty()
            || admitted_automatic.unwrap_or(true) != current_automatic;
        if !stale_snapshot {
            return Ok(false);
        }
        let mut state = self.lock_state();
        let Some(request_id) = state.active_request_id.clone() else {
            return Ok(false);
        };
        let stale = find_attempt(&state, &request_id).is_some_and(|attempt| {
            attempt.state == SourceBackedRefreshState::Queued
                && attempt
                    .admitted_authority
                    .as_ref()
                    .filter(|authority| {
                        authority.coverage()
                            == ctx_history_refresh_execution::AdmittedRefreshCoverage::CompleteCatalog
                    })
                    .map(|authority| authority.discovery())
                    .is_some_and(|admitted| {
                        let roots_changed = match admitted.configured_provider_roots() {
                            Some(admitted) => admitted != current_roots.as_slice(),
                            None => !current_roots.is_empty(),
                        };
                        roots_changed
                            || admitted.automatic_provider_discovery().unwrap_or(true)
                                != current_automatic
                    })
        });
        if !stale {
            return Ok(false);
        }

        let snapshot = AdmissionResolutionSnapshot::capture(&state);
        let attempt = find_attempt_mut(&mut state, &request_id)
            .ok_or_else(|| anyhow!("source refresh request `{request_id}` is unknown"))?;
        attempt.admitted_authority = None;
        attempt.state = SourceBackedRefreshState::AdmissionPending;
        attempt.progress.phase = "admission_pending".to_owned();
        attempt.last_error = None;
        let durable_request_id = durable_request_id(&state, &request_id);
        let job = durable_job_json(&state, durable_request_id)
            .ok_or_else(|| anyhow!("source refresh request `{durable_request_id}` is unknown"))?;
        if let Err(error) = self.write_status(data_root, &job) {
            snapshot.restore(&mut state);
            return Err(error.context(
                "persist source refresh re-admission after provider-root config changed",
            ));
        }
        Ok(true)
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

    #[cfg(any(test, feature = "test-support"))]
    fn claim_next_pending_admission(&self) -> Option<PendingAdmissionClaim> {
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
        let attempt = find_attempt(&state, &request_id)?;
        let claim = PendingAdmissionClaim {
            request_id: request_id.clone(),
            intent: attempt.intent.clone(),
            persisted_scope: attempt.refresh_scope.clone(),
            watch_catalog_revision: state.watch_catalog_revision,
            route_event_watermarks: state.route_event_watermarks.clone(),
        };
        state
            .admission_resolutions_in_flight
            .insert(request_id.clone());
        Some(claim)
    }

    fn complete_claimed_pending_admission(
        &self,
        data_root: &Path,
        claim: &PendingAdmissionClaim,
        resolution: Result<ctx_history_refresh_execution::AdmittedRefresh>,
    ) -> Result<Option<SourceBackedRefreshRun>> {
        let resolution = resolution.and_then(|authority| {
            let observation_fence = self.sample_admission_observations(
                &authority,
                claim.watch_catalog_revision,
                &claim.route_event_watermarks,
            )?;
            Ok((authority, observation_fence))
        });
        let mut state = self.lock_state();
        state
            .admission_resolutions_in_flight
            .remove(&claim.request_id);
        if state.watch_uncertain_through.is_some() {
            return Ok(None);
        }
        if find_attempt(&state, &claim.request_id)
            .is_none_or(|attempt| attempt.state != SourceBackedRefreshState::AdmissionPending)
        {
            return Ok(None);
        }
        if state.watch_catalog_revision != claim.watch_catalog_revision {
            return Ok(None);
        }
        if resolution.is_err()
            && !admission_failure_fence_matches(
                &state,
                &claim.persisted_scope,
                &claim.route_event_watermarks,
            )
        {
            return Ok(None);
        }
        match resolution {
            Ok((resolution, observation_fence)) => {
                if !observation_fence.still_matches(&state) {
                    return Ok(None);
                }
                self.persist_resolved_admission(
                    data_root,
                    &mut state,
                    &claim.request_id,
                    resolution,
                    observation_fence.route_observations,
                )?;
                Ok(None)
            }
            Err(error) => {
                if state.active_request_id.as_deref() != Some(claim.request_id.as_str()) {
                    // A speculative batch peer still owns a queued turn. Its
                    // normal active resolution owns failure and retry handoff.
                    return Ok(None);
                }
                self.persist_failed_admission(data_root, &mut state, &claim.request_id, error)
            }
        }
    }

    pub(super) fn sample_admission_observations(
        &self,
        authority: &ctx_history_refresh_execution::AdmittedRefresh,
        watch_catalog_revision: u64,
        claimed_route_event_watermarks: &BTreeMap<SourceRouteIdentity, EventWatermark>,
    ) -> Result<AdmissionObservationFence> {
        let routes = authority.exact_routes();
        let route_event_watermarks = routes
            .iter()
            .map(|route| {
                (
                    route.clone(),
                    claimed_route_event_watermarks.get(route).copied(),
                )
            })
            .collect();
        let route_observations =
            validate_admission_observations(source_backed_requested_route_observations(
                authority.discovery().watch_catalog(),
                routes,
            ))?
            .into_iter()
            .filter_map(|(route, observation)| observation.map(|value| (route, value)))
            .collect();
        Ok(AdmissionObservationFence {
            watch_catalog_revision,
            route_event_watermarks,
            route_observations,
        })
    }

    fn persist_resolved_admission(
        &self,
        data_root: &Path,
        state: &mut CoreRefreshEngineState,
        request_id: &str,
        authority: ctx_history_refresh_execution::AdmittedRefresh,
        route_observations: BTreeMap<SourceRouteIdentity, String>,
    ) -> Result<()> {
        let snapshot = AdmissionResolutionSnapshot::capture(state);
        let automatic_retry_checkpoints = state.automatic_retry_checkpoints.clone();
        let attempt = find_attempt_mut(state, request_id)
            .ok_or_else(|| anyhow!("source refresh request `{request_id}` is unknown"))?;
        let authority_scope = match authority.coverage() {
            ctx_history_refresh_execution::AdmittedRefreshCoverage::CompleteCatalog => {
                SourceBackedRefreshScope::All
            }
            ctx_history_refresh_execution::AdmittedRefreshCoverage::SelectedRoutes => {
                SourceBackedRefreshScope::Exact(authority.exact_routes().clone())
            }
        };
        if matches!(&attempt.refresh_scope, SourceBackedRefreshScope::Exact(_))
            && attempt.refresh_scope != authority_scope
        {
            bail!("scoped source refresh admission would widen its persisted exact scope");
        }
        attempt.refresh_scope = authority_scope;
        attempt.admitted_authority = Some(authority);
        attempt.route_observations = route_observations;
        attempt.automatic_retry_checkpoints = automatic_retry_checkpoints;
        let attempt = find_attempt_mut(state, request_id)
            .ok_or_else(|| anyhow!("source refresh request `{request_id}` is unknown"))?;
        attempt.state = SourceBackedRefreshState::Queued;
        attempt.progress.phase = "queued".to_owned();
        attempt.last_error = None;
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
        let attempted_routes = find_attempt(state, request_id)
            .and_then(|attempt| match &attempt.refresh_scope {
                SourceBackedRefreshScope::All => None,
                SourceBackedRefreshScope::Exact(routes) => Some(routes.clone()),
            })
            .unwrap_or_default();
        let classified_outcome =
            source_backed_refresh_failure_outcome(&error, &attempted_routes, request_id)?;
        let failure_outcome =
            if classified_outcome.code() == RefreshOutcomeCode::SourceRefreshFailed {
                RefreshTerminalOutcome::with_uniform_route_disposition(
                    RefreshOutcomeCode::SourceRefreshAdmissionFailed,
                    true,
                    BTreeSet::new(),
                    request_id.to_owned(),
                    None,
                    None,
                    Some(RefreshRetryAdvice::RetryAdmission),
                    None,
                )?
            } else {
                classified_outcome
            };
        let retained_generation = find_attempt(state, request_id).and_then(|attempt| {
            attempt
                .published_generation
                .clone()
                .or_else(|| attempt.previous_generation.clone())
        });
        let failure_outcome = failure_outcome.with_failure_context(
            retained_generation,
            Some(format!("source refresh admission fence failed: {error:#}")),
        )?;
        let retry_admission = failure_outcome.code()
            == RefreshOutcomeCode::SourceRefreshAdmissionFailed
            && failure_outcome.retry_advice() == Some(RefreshRetryAdvice::RetryAdmission);
        let retryable_routes = failure_outcome.retryable_routes().clone();
        let blocked_routes = failure_outcome.blocked_routes().clone();
        let (scope, last_error) = {
            let attempt = find_attempt_mut(state, request_id)
                .ok_or_else(|| anyhow!("source refresh request `{request_id}` is unknown"))?;
            let last_error = format!("source refresh admission fence failed: {error:#}");
            attempt.state = SourceBackedRefreshState::Failed;
            attempt.finished_at_ms = Some(utc_now().timestamp_millis());
            attempt.progress.phase = "failed".to_owned();
            attempt.failure_type = source_backed_refresh_failure_type(&error);
            attempt.terminal_outcome = Some(failure_outcome);
            attempt.last_error = Some(last_error.clone());
            attempt.failure_diagnostic = Some(RefreshFailureDiagnostic::new(
                FailureStage::Admission,
                Some(&error),
            ));
            (attempt.refresh_scope.clone(), last_error)
        };
        let job = durable_job_json(state, request_id)
            .ok_or_else(|| anyhow!("source refresh request `{request_id}` is unknown"))?;
        if let Err(persist_error) = self.write_status(data_root, &job) {
            snapshot.restore(state);
            return Err(persist_error.context("persist terminal source refresh admission failure"));
        }
        let retry_intent = find_attempt(state, request_id).map(|attempt| attempt.intent.clone());
        Self::restore_route_dispositions_locked(
            state,
            &retryable_routes,
            &blocked_routes,
            retry_intent.as_ref(),
        );
        if retry_admission {
            state.pending_scheduler_retry_root_id = Some(request_id.to_owned());
        }
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
        mut observations: BTreeMap<SourceRouteIdentity, Option<String>>,
    ) -> Result<()> {
        let mut state = self.lock_state();
        let Some(attempt) = find_attempt(&state, request_id) else {
            return Ok(());
        };
        if attempt.state != SourceBackedRefreshState::AdmissionPending {
            return Ok(());
        }
        let exact_routes = match &attempt.refresh_scope {
            SourceBackedRefreshScope::All => None,
            SourceBackedRefreshScope::Exact(routes) => Some(routes.clone()),
        };
        if let Some(routes) = exact_routes.as_ref() {
            for route in routes {
                observations.entry(route.clone()).or_insert(None);
            }
        }
        let route_observations = validate_admission_observations(observations.clone())?
            .into_iter()
            .filter_map(|(route, observation)| observation.map(|value| (route, value)))
            .collect();
        let admitted = admitted_refresh_for_test(observations);
        let admitted = match exact_routes {
            Some(routes) => admitted.narrow_to(routes)?,
            None => admitted,
        };
        self.persist_resolved_admission(
            data_root,
            &mut state,
            request_id,
            admitted,
            route_observations,
        )
    }
}
