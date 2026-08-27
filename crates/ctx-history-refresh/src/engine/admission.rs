use super::*;
use sha2::{Digest as _, Sha256};

#[derive(Clone)]
struct PendingAdmissionClaim {
    request_id: String,
    intent: RefreshIntent,
    persisted_scope: SourceBackedRefreshScope,
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
                RefreshIntent::SelectedImport(RefreshSelection::All),
                RefreshRequestTrigger::Import,
            ) => metadata.trigger_provenance = "import_command",
            (
                RefreshIntent::SelectedImport(RefreshSelection::Provider(_)),
                RefreshRequestTrigger::Import,
            ) => metadata.trigger_provenance = "automatic_provider",
            (
                RefreshIntent::SelectedImport(RefreshSelection::ExactSource(_)),
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
            == Some("admission_pending"))
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
        self.complete_claimed_pending_admission(data_root, &claim.request_id, resolution)?;
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
        self.complete_claimed_pending_admission(data_root, &claim.request_id, resolution)
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

    fn resolve_pending_admission_claim(
        &self,
        data_root: &Path,
        claim: &PendingAdmissionClaim,
    ) -> Result<ctx_history_refresh_execution::AdmittedRefresh> {
        let discovery = self
            .runtime
            .discovery_context(data_root)?
            .with_data_root(data_root);
        if let (RefreshIntent::AutomaticMaintenance, SourceBackedRefreshScope::Exact(routes)) =
            (&claim.intent, &claim.persisted_scope)
        {
            return self.resolve_exact_maintenance_admission(data_root, claim, routes);
        }
        match &claim.intent {
            RefreshIntent::AutomaticMaintenance
            | RefreshIntent::SelectedImport(RefreshSelection::All) => {
                let resolved =
                    (self.admission_fence)(&discovery, self.journal.as_ref(), data_root, None)?;
                self.bound_resolved_admission(claim, resolved)
            }
            RefreshIntent::SelectedImport(RefreshSelection::Provider(provider)) => {
                let started = StdInstant::now();
                let report =
                    ctx_history_capture::discover_provider_sources_for_provider_with_context(
                        &discovery, *provider,
                    );
                let duration = started.elapsed();
                self.resolve_scoped_admission_report(data_root, claim, &discovery, report, duration)
                    .with_context(|| {
                        format!(
                            "resolve automatic provider `{}` source refresh admission",
                            provider.as_str()
                        )
                    })
            }
            RefreshIntent::SelectedImport(RefreshSelection::ExactSource(authority)) => {
                authority
                    .validate_source_roots(data_root)
                    .context("validate explicit source roots before scoped admission")?;
                let started = StdInstant::now();
                let installed_catalog = self.lock_state().watch_catalog.clone();
                let report = match installed_catalog.as_ref() {
                    Some(catalog) => authority
                        .admission_discovery_report_with_automatic_catalog(data_root, catalog)?,
                    None => authority.admission_discovery_report(data_root)?,
                };
                let duration = started.elapsed();
                self.resolve_scoped_admission_report(data_root, claim, &discovery, report, duration)
                    .context("resolve explicit-catalog source refresh admission")
            }
        }
    }

    fn resolve_exact_maintenance_admission(
        &self,
        data_root: &Path,
        claim: &PendingAdmissionClaim,
        routes: &BTreeSet<SourceRouteIdentity>,
    ) -> Result<ctx_history_refresh_execution::AdmittedRefresh> {
        let catalog = self.lock_state().watch_catalog.clone();
        #[cfg(any(test, feature = "test-support"))]
        if catalog.is_none() {
            let admitted = admitted_refresh_for_test(
                routes.iter().cloned().map(|route| (route, None)).collect(),
            );
            return self.bound_resolved_admission(claim, admitted);
        }
        let catalog = catalog
            .ok_or_else(|| anyhow!("exact maintenance admission has no installed route catalog"))?;
        let installed_routes = catalog.route_ids().cloned().collect::<BTreeSet<_>>();
        let missing = routes
            .difference(&installed_routes)
            .cloned()
            .collect::<BTreeSet<_>>();
        if !missing.is_empty() {
            bail!(
                "exact maintenance admission routes are missing from the installed catalog: {missing:?}"
            );
        }
        let started = StdInstant::now();
        let admitted =
            ctx_history_refresh_execution::AdmittedRefresh::from_exact_catalog_authority(
                routes.clone(),
                started.elapsed(),
                catalog,
            )?;
        validate_provider_source_roots_outside_data_root(
            data_root,
            admitted.discovery().report().sources.iter(),
        )
        .context("validate exact maintenance roots before admission")?;
        self.bound_resolved_admission(claim, admitted)
    }

    fn bound_resolved_admission(
        &self,
        claim: &PendingAdmissionClaim,
        resolved: ctx_history_refresh_execution::AdmittedRefresh,
    ) -> Result<ctx_history_refresh_execution::AdmittedRefresh> {
        let selected_routes = match &claim.persisted_scope {
            SourceBackedRefreshScope::All => resolved.exact_routes().clone(),
            SourceBackedRefreshScope::Exact(persisted) => {
                let missing = persisted
                    .difference(resolved.exact_routes())
                    .cloned()
                    .collect::<BTreeSet<_>>();
                if !missing.is_empty() {
                    bail!(
                        "recovered source refresh admission is missing persisted exact routes: {missing:?}"
                    );
                }
                persisted.clone()
            }
        };
        let route_observations = source_backed_requested_route_observations(
            resolved.discovery().watch_catalog(),
            &selected_routes,
        );
        validate_admission_observations(route_observations.clone())?;
        let admitted = match &claim.persisted_scope {
            SourceBackedRefreshScope::All => resolved,
            SourceBackedRefreshScope::Exact(_) => resolved.narrow_to(selected_routes)?,
        };
        Ok(admitted)
    }

    fn resolve_scoped_admission_report(
        &self,
        data_root: &Path,
        claim: &PendingAdmissionClaim,
        discovery: &DiscoveryContext,
        report: ctx_history_capture::DiscoveryReport,
        discovery_duration: StdDuration,
    ) -> Result<ctx_history_refresh_execution::AdmittedRefresh> {
        if report.sources.is_empty() && report.issues.is_empty() {
            match &claim.intent {
                RefreshIntent::SelectedImport(RefreshSelection::Provider(provider)) => bail!(
                    "automatic provider `{}` discovery produced no executable source routes",
                    provider.as_str()
                ),
                RefreshIntent::SelectedImport(RefreshSelection::ExactSource(_)) => {
                    bail!("explicit source catalog produced no executable source routes")
                }
                RefreshIntent::AutomaticMaintenance
                | RefreshIntent::SelectedImport(RefreshSelection::All) => {
                    bail!("all-automatic admission unexpectedly resolved as scoped")
                }
            }
        }
        validate_provider_source_roots_outside_data_root(data_root, report.sources.iter())
            .context("validate provider roots before scoped source refresh admission")?;
        prepare_generation_control_state(data_root)?;
        let published_state = crate::orchestration::RetainedPublishedState {
            journal: self.journal.as_ref(),
        };
        let mut admitted_refresh =
            ctx_history_refresh_execution::source_backed_admitted_discovery_from_report(
                discovery,
                report,
                discovery_duration,
                data_root,
                ctx_history_refresh_execution::AdmittedRefreshCoverage::SelectedRoutes,
                claim.intent.explicit_source_authority(),
                &published_state,
            )?;
        if let RefreshIntent::SelectedImport(RefreshSelection::Provider(provider)) = &claim.intent {
            let catalog = admitted_refresh.discovery().watch_catalog().clone();
            let catalog_provider_routes = catalog.route_ids_for_provider(*provider);
            let freshly_executable = admitted_refresh
                .exact_routes()
                .intersection(&catalog_provider_routes)
                .cloned()
                .collect::<BTreeSet<_>>();
            let mut provider_routes = freshly_executable.clone();
            if provider_routes.is_empty() {
                bail!(
                    "automatic provider `{}` discovery produced no executable source routes",
                    provider.as_str()
                );
            }
            if let SourceBackedRefreshScope::Exact(persisted) = &claim.persisted_scope {
                let missing = persisted
                    .difference(&freshly_executable)
                    .cloned()
                    .collect::<BTreeSet<_>>();
                if !missing.is_empty() {
                    bail!(
                        "recovered scoped source refresh discovery is missing persisted exact routes: {missing:?}"
                    );
                }
                provider_routes = persisted.clone();
            }
            admitted_refresh =
                ctx_history_refresh_execution::AdmittedRefresh::from_exact_catalog_authority(
                    provider_routes,
                    discovery_duration,
                    catalog,
                )?;
        }
        let routes = admitted_refresh.exact_routes().clone();
        if routes.is_empty() {
            match &claim.intent {
                RefreshIntent::SelectedImport(RefreshSelection::Provider(provider)) => bail!(
                    "automatic provider `{}` discovery produced no executable source routes",
                    provider.as_str()
                ),
                RefreshIntent::SelectedImport(RefreshSelection::ExactSource(_)) => {
                    bail!("explicit source catalog produced no executable source routes")
                }
                RefreshIntent::AutomaticMaintenance
                | RefreshIntent::SelectedImport(RefreshSelection::All) => {
                    bail!("all-automatic admission unexpectedly resolved as scoped")
                }
            }
        }
        let selected_routes = match &claim.persisted_scope {
            SourceBackedRefreshScope::All => routes,
            SourceBackedRefreshScope::Exact(persisted) => {
                let missing = persisted
                    .difference(&routes)
                    .cloned()
                    .collect::<BTreeSet<_>>();
                if !missing.is_empty() {
                    bail!(
                        "recovered scoped source refresh discovery is missing persisted exact routes: {missing:?}"
                    );
                }
                // Provider discovery may legitimately grow while an
                // acknowledged request is interrupted. Resume only the
                // persisted exact selection; newly discovered routes belong
                // to later maintenance.
                persisted.clone()
            }
        };
        if selected_routes.len() > SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT {
            bail!("scoped source refresh admission exceeds its bounded route capacity");
        }
        let route_observations = source_backed_requested_route_observations(
            admitted_refresh.discovery().watch_catalog(),
            &selected_routes,
        );
        validate_admission_observations(route_observations.clone())?;
        let admitted = admitted_refresh.narrow_to(selected_routes)?;
        Ok(admitted)
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

    fn claim_active_pending_admission(&self) -> Option<PendingAdmissionClaim> {
        let mut state = self.lock_state();
        if state.watch_uncertain_through.is_some() {
            return None;
        }
        let request_id = state.active_request_id.clone()?;
        let attempt = find_attempt(&state, &request_id)?;
        if attempt.state != SourceBackedRefreshState::AdmissionPending
            || state.unacknowledged_admissions.contains_key(&request_id)
            || state.admission_resolutions_in_flight.contains(&request_id)
        {
            return None;
        }
        let claim = PendingAdmissionClaim {
            request_id: request_id.clone(),
            intent: attempt.intent.clone(),
            persisted_scope: attempt.refresh_scope.clone(),
        };
        state
            .admission_resolutions_in_flight
            .insert(request_id.clone());
        Some(claim)
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
        };
        state
            .admission_resolutions_in_flight
            .insert(request_id.clone());
        Some(claim)
    }

    fn complete_claimed_pending_admission(
        &self,
        data_root: &Path,
        request_id: &str,
        resolution: Result<ctx_history_refresh_execution::AdmittedRefresh>,
    ) -> Result<Option<SourceBackedRefreshRun>> {
        let mut state = self.lock_state();
        state.admission_resolutions_in_flight.remove(request_id);
        if state.watch_uncertain_through.is_some() {
            return Ok(None);
        }
        if find_attempt(&state, request_id)
            .is_none_or(|attempt| attempt.state != SourceBackedRefreshState::AdmissionPending)
        {
            return Ok(None);
        }
        match resolution {
            Ok(resolution) => {
                self.persist_resolved_admission(data_root, &mut state, request_id, resolution)?;
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
        authority: ctx_history_refresh_execution::AdmittedRefresh,
    ) -> Result<()> {
        let snapshot = AdmissionResolutionSnapshot::capture(state);
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
        let failure_type = source_backed_refresh_failure_type(&error);
        let classified_outcome = source_backed_refresh_failure_outcome(&error, &attempted_routes);
        let failure_outcome = if classified_outcome.code == RefreshOutcomeCode::SourceRefreshFailed
            && classified_outcome.class == RefreshOutcomeClass::Internal
        {
            SourceBackedRefreshFailureOutcome::new(
                RefreshOutcomeCode::SourceRefreshAdmissionFailed,
                RefreshOutcomeClass::ControlPlane,
                true,
                BTreeSet::new(),
                Some(RefreshRetryAdvice::RetryAdmission),
            )
        } else {
            classified_outcome
        };
        let retry_admission = failure_outcome.code
            == RefreshOutcomeCode::SourceRefreshAdmissionFailed
            && failure_outcome.retry_advice == Some(RefreshRetryAdvice::RetryAdmission);
        let retryable_routes = failure_outcome.retryable_routes.clone();
        let blocked_routes = failure_outcome.blocked_routes.clone();
        let (scope, last_error) = {
            let attempt = find_attempt_mut(state, request_id)
                .ok_or_else(|| anyhow!("source refresh request `{request_id}` is unknown"))?;
            let last_error = format!("source refresh admission fence failed: {error:#}");
            attempt.state = SourceBackedRefreshState::Failed;
            attempt.finished_at_ms = Some(utc_now().timestamp_millis());
            attempt.progress.phase = "failed".to_owned();
            attempt.failure_type = failure_type;
            attempt.failure_outcome = Some(failure_outcome);
            attempt.last_error = Some(last_error.clone());
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
            observations.extend(routes.iter().cloned().map(|route| (route, None)));
        }
        let admitted = admitted_refresh_for_test(observations);
        let admitted = match exact_routes {
            Some(routes) => admitted.narrow_to(routes)?,
            None => admitted,
        };
        self.persist_resolved_admission(data_root, &mut state, request_id, admitted)
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

struct AdmissionReservationSnapshot {
    active_request_id: Option<String>,
    pending_request_ids: VecDeque<String>,
    attempts: VecDeque<SourceBackedRefreshAttempt>,
    response_barriers: BTreeMap<String, usize>,
}

impl AdmissionReservationSnapshot {
    fn capture(state: &CoreRefreshEngineState) -> Self {
        Self {
            active_request_id: state.active_request_id.clone(),
            pending_request_ids: state.pending_request_ids.clone(),
            attempts: state.attempts.clone(),
            response_barriers: state.unacknowledged_admissions.clone(),
        }
    }

    fn restore(self, state: &mut CoreRefreshEngineState) {
        state.active_request_id = self.active_request_id;
        state.pending_request_ids = self.pending_request_ids;
        state.attempts = self.attempts;
        state.unacknowledged_admissions = self.response_barriers;
    }
}

struct AdmissionResolutionSnapshot {
    attempts: VecDeque<SourceBackedRefreshAttempt>,
}

impl AdmissionResolutionSnapshot {
    fn capture(state: &CoreRefreshEngineState) -> Self {
        Self {
            attempts: state.attempts.clone(),
        }
    }

    fn restore(self, state: &mut CoreRefreshEngineState) {
        state.attempts = self.attempts;
    }
}
