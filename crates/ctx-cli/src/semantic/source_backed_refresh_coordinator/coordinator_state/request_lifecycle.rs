use super::*;

impl CoreRefreshEngine {
    pub(in crate::semantic) fn run_next(&self, data_root: &Path) -> Option<SourceBackedRefreshRun> {
        if let Some(run) = self.resolve_fully_covered_continuation_with(data_root, |catalog| {
            source_backed_route_admission_fence(data_root, catalog)
        }) {
            return Some(run);
        }
        self.run_next_with_verified_index_opener(data_root, |index_root| {
            Ok(Arc::new(open_verified_index(index_root)?))
        })
    }

    #[cfg(test)]
    pub(in super::super) fn run_next_with_post_publication_sampler_for_test<Sample>(
        &self,
        data_root: &Path,
        sample: Sample,
    ) -> Option<SourceBackedRefreshRun>
    where
        Sample: FnOnce(
            Option<&ExplicitSourceCatalogAuthority>,
        ) -> Result<BTreeMap<SourceRouteIdentity, Option<String>>>,
    {
        if let Some(run) = self.resolve_fully_covered_continuation_with(data_root, sample) {
            return Some(run);
        }
        self.run_next_with_verified_index_opener(data_root, |index_root| {
            Ok(Arc::new(open_verified_index(index_root)?))
        })
    }

    fn resolve_fully_covered_continuation_with<Sample>(
        &self,
        data_root: &Path,
        sample: Sample,
    ) -> Option<SourceBackedRefreshRun>
    where
        Sample: FnOnce(
            Option<&ExplicitSourceCatalogAuthority>,
        ) -> Result<BTreeMap<SourceRouteIdentity, Option<String>>>,
    {
        let (sample_request_id, sample_routes) = {
            let state = self.lock_state();
            let request_id = state.active_request_id.clone()?;
            let continuation = state.manual_all_continuations.get(&request_id)?;
            if !continuation.predecessor_finished || !continuation.is_fully_covered() {
                return None;
            }
            let predecessor = find_attempt(&state, &continuation.predecessor_request_id)?;
            let publication_receipt = predecessor
                .publication_receipt
                .as_ref()
                .or(predecessor.receipt.as_ref())?;
            if publication_receipt.published_generation.is_empty() {
                return None;
            }
            let routes = continuation.covered_route_results.keys().cloned().collect();
            (request_id, routes)
        };
        let post_publication_fence = self.post_publication_route_coverage_fence_with(
            &sample_request_id,
            sample_routes,
            sample,
        );

        let mut state = self.lock_state();
        let request_id = state.active_request_id.clone()?;
        if request_id != sample_request_id {
            return None;
        }
        let continuation = state.manual_all_continuations.get(&request_id)?.clone();
        if !continuation.predecessor_finished || !continuation.is_fully_covered() {
            return None;
        }
        let predecessor = find_attempt(&state, &continuation.predecessor_request_id)?.clone();
        let publication_receipt = predecessor
            .publication_receipt
            .clone()
            .or_else(|| predecessor.receipt.clone())?;
        let published_generation = publication_receipt.published_generation.clone();
        let coverage_certificate = state
            .pinned_core_publication
            .as_ref()
            .filter(|authority| authority.generation_id() == published_generation)
            .and_then(|authority| {
                SourceBackedPublicationMetadata::decode(authority.verified_index_ref()).ok()
            })
            .filter(|metadata| metadata.request_id == continuation.predecessor_request_id)
            .map(|metadata| {
                let routes = continuation
                    .covered_route_results
                    .keys()
                    .filter_map(|route| {
                        let observation = continuation
                            .admission_route_observations
                            .get(route)
                            .and_then(Option::as_ref)?;
                        if metadata.route_observations.get(route) != Some(observation) {
                            return None;
                        }
                        let admitted_watermark = continuation
                            .admission_event_watermarks
                            .get(route)
                            .copied()?;
                        let admitted_watermark = post_publication_fence.certified_boundary(
                            route,
                            admitted_watermark,
                            observation,
                        );
                        Some((
                            route.clone(),
                            SourceBackedRefreshRouteCoverageCertificate {
                                observation: observation.clone(),
                                admitted_watermark,
                            },
                        ))
                    })
                    .collect();
                SourceBackedRefreshCoverageCertificate {
                    request_id: request_id.clone(),
                    published_generation: published_generation.clone(),
                    routes,
                }
            });
        let now = utc_now().timestamp_millis();
        let request_receipt = {
            let attempt = find_attempt_mut(&mut state, &request_id)?;
            let mut receipt = publication_receipt.clone();
            receipt.previous_generation = attempt.previous_generation.clone();
            receipt.generation_changed =
                receipt.previous_generation.as_deref() != Some(published_generation.as_str());
            attempt.state = SourceBackedRefreshState::Published;
            attempt.started_at_ms = Some(now);
            attempt.finished_at_ms = Some(now);
            attempt.published_generation = Some(published_generation.clone());
            attempt.progress.phase = "published".to_owned();
            attempt.progress.completed_sources = receipt.route_results.len();
            attempt.progress.total_sources = receipt.route_results.len();
            attempt.scanned_routes = Some(0);
            attempt.unsupported_routes = Some(
                receipt
                    .route_results
                    .iter()
                    .filter(|result| result.outcome.failure_class() == Some("incompatible"))
                    .count(),
            );
            attempt.certified_source_count = Some(receipt.current.source_count);
            attempt.certified_source_bytes = Some(receipt.current.certified_source_bytes);
            attempt.receipt = Some(receipt.clone());
            attempt.publication_receipt = Some(publication_receipt);
            attempt.timings = Some(continuation.covered_timings);
            attempt.failure_type = None;
            attempt.last_error = None;
            receipt
        };
        let terminal_job = durable_job_json(&state, &request_id)?;
        if let Err(error) = self.write_status(data_root, &terminal_job) {
            let attempt = find_attempt_mut(&mut state, &request_id)?;
            attempt.state = SourceBackedRefreshState::Queued;
            attempt.progress.phase = "persisting_terminal".to_owned();
            attempt.receipt = None;
            attempt.publication_receipt = None;
            attempt.last_error = Some(format!(
                "persist exact logical demand resolution before acknowledgement: {error:#}"
            ));
            return Some(SourceBackedRefreshRun {
                job: attempt.job_json(),
                did_work: false,
                failed: false,
                terminal_persistence_pending: true,
                scope: attempt.refresh_scope.clone(),
                coverage_certificate: None,
            });
        }
        let scope = find_attempt(&state, &request_id)?.refresh_scope.clone();
        state.manual_all_continuations.remove(&request_id);
        state.current_published_generation = Some(published_generation.clone());
        advance_after_terminal_attempt(&mut state, &request_id, Some(published_generation));
        trim_terminal_attempt_history(&mut state);
        let job = find_attempt(&state, &request_id)?.job_json();
        debug_assert_eq!(
            request_receipt.published_generation,
            job.get("published_generation")
                .and_then(Value::as_str)
                .unwrap_or_default()
        );
        Some(SourceBackedRefreshRun {
            job,
            did_work: false,
            failed: false,
            terminal_persistence_pending: false,
            scope,
            coverage_certificate,
        })
    }

    pub(in super::super) fn run_next_with_verified_index_opener<Open>(
        &self,
        data_root: &Path,
        open_verified: Open,
    ) -> Option<SourceBackedRefreshRun>
    where
        Open: FnOnce(&Path) -> Result<Arc<VerifiedIndex>>,
    {
        let executor = Arc::clone(&self.executor);
        let verified_index = RefCell::new(None::<Arc<VerifiedIndex>>);
        let publication_probe_attempted = Cell::new(false);
        let mut run = self.run_next_with_terminal_success(
            |request_id, coordinator| {
                let requested_catalog = coordinator.requested_explicit_source_catalog(request_id);
                let refresh_scope = coordinator
                    .refresh_scope(request_id)
                    .ok_or_else(|| anyhow!("source refresh request `{request_id}` is unknown"))?;
                let operation = coordinator
                    .operation(request_id)
                    .ok_or_else(|| anyhow!("source refresh request `{request_id}` is unknown"))?;
                let (covered_route_ids, covered_publication) =
                    coordinator.admit_refresh_scope(request_id, &refresh_scope)?;
                coordinator.persist_job_status(data_root, request_id)?;
                let mut publication = execute_source_backed_refresh(
                    executor.as_ref(),
                    data_root,
                    request_id,
                    coordinator,
                    SourceBackedRefreshPlan {
                        explicit_source_catalog: requested_catalog.as_ref(),
                        operation,
                        scope: refresh_scope.clone(),
                        covered_route_ids,
                        covered_publication,
                    },
                )?;
                let probe_started = StdInstant::now();
                let pin = if let Some(pin) = publication.verified_index.take() {
                    pin
                } else {
                    publication_probe_attempted.set(true);
                    open_verified(&source_backed_index_root(data_root))
                        .context("verify Core generation after publication")?
                };
                let verification = verify_source_backed_publication(&publication, &pin);
                coordinator.set_publication_probe_timing(
                    request_id,
                    nonzero_duration_micros(probe_started.elapsed()),
                );
                verification?;
                if let Ok(metadata) = SourceBackedPublicationMetadata::decode(&pin) {
                    if metadata.request_id == request_id
                        && metadata.operation == operation
                        && metadata.refresh_scope == refresh_scope
                    {
                        coordinator.set_route_observations(request_id, metadata.route_observations);
                    }
                }
                verified_index.replace(Some(pin));
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
            |receipt| {
                let pin = verified_index.borrow_mut().take().ok_or_else(|| {
                    anyhow!("verified Core publication has no exact retained generation pin")
                })?;
                let authority_receipt = publication_authority_receipt(&pin, receipt)?;
                CoreRefreshTerminalSuccess::bind(authority_receipt, pin)
                    .context("bind exact Core publication receipt and generation authority")
            },
            |job| self.write_status(data_root, job),
            |_| Ok(()),
        )?;
        let publication_ready = !run.failed && !run.terminal_persistence_pending;
        if let Some(request_id) = run.job.get("request_id").and_then(Value::as_str) {
            if !run.terminal_persistence_pending {
                let post_publication_fence = publication_ready
                    .then(|| self.post_publication_route_coverage_fence(data_root, request_id));
                let coverage_certificate = self.finish_route_admissions(
                    request_id,
                    publication_ready,
                    post_publication_fence.as_ref(),
                );
                if let Err(error) = self.persist_job_status(data_root, request_id) {
                    run.terminal_persistence_pending = true;
                    let mut state = self.lock_state();
                    if let Some(active_request_id) = state.active_request_id.clone() {
                        if let Some(active) = find_attempt_mut(&mut state, &active_request_id) {
                            active.last_error = Some(format!(
                                "persist logical demand coverage after publication: {error:#}"
                            ));
                        }
                    }
                } else {
                    run.coverage_certificate = coverage_certificate;
                }
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

    fn set_route_observations(
        &self,
        request_id: &str,
        observations: BTreeMap<SourceRouteIdentity, String>,
    ) {
        let mut state = self.lock_state();
        if let Some(attempt) = find_attempt_mut(&mut state, request_id) {
            attempt.route_observations = observations;
        }
    }

    fn post_publication_route_coverage_fence(
        &self,
        data_root: &Path,
        request_id: &str,
    ) -> PostPublicationRouteCoverageFence {
        let routes = {
            let state = self.lock_state();
            find_attempt(&state, request_id)
                .into_iter()
                .flat_map(|attempt| attempt.route_observations.keys().cloned())
                .collect()
        };
        self.post_publication_route_coverage_fence_with(request_id, routes, |catalog| {
            source_backed_route_admission_fence(data_root, catalog)
        })
    }

    fn post_publication_route_coverage_fence_with<Sample>(
        &self,
        request_id: &str,
        routes: BTreeSet<SourceRouteIdentity>,
        sample: Sample,
    ) -> PostPublicationRouteCoverageFence
    where
        Sample: FnOnce(
            Option<&ExplicitSourceCatalogAuthority>,
        ) -> Result<BTreeMap<SourceRouteIdentity, Option<String>>>,
    {
        // Snapshot the exact seen-event boundary before touching provider
        // targets. Events delivered after this lock is released are outside
        // the certificate even if their content-free observation is equal.
        let (seen_watermarks, requested_catalog) = {
            let state = self.lock_state();
            let attempt = find_attempt(&state, request_id);
            let seen_watermarks = routes
                .iter()
                .filter_map(|route| {
                    state
                        .route_event_watermarks
                        .get(route)
                        .copied()
                        .map(|watermark| (route.clone(), watermark))
                })
                .collect();
            let requested_catalog =
                attempt.and_then(|attempt| attempt.requested_explicit_source_catalog.clone());
            (seen_watermarks, requested_catalog)
        };
        let mut sampled = sample(requested_catalog.as_ref()).unwrap_or_default();
        let sampled_observations = routes
            .into_iter()
            .map(|route| {
                let observation = sampled.remove(&route).flatten();
                (route, observation)
            })
            .collect();
        PostPublicationRouteCoverageFence {
            seen_watermarks,
            sampled_observations,
        }
    }

    #[cfg(test)]
    pub(in super::super) fn set_route_observations_for_test(
        &self,
        request_id: &str,
        observations: BTreeMap<SourceRouteIdentity, String>,
    ) {
        self.set_route_observations(request_id, observations);
    }

    pub(in crate::semantic) fn enqueue_periodic(&self, data_root: &Path) -> Result<Value> {
        let observed_generation = self.observed_published_generation(data_root)?;
        self.enqueue_with_catalog_metadata(
            observed_generation,
            SourceRefreshRuntimeMetadata::periodic(),
            None,
            SourceBackedRefreshScope::All,
            SourceRefreshAdmissionRequirement::AttachEquivalent,
            BTreeMap::new(),
            None,
        )
    }

    /// Restores an exact published authority when its durable receipt is
    /// complete, or durably enqueues one replay before daemon readiness when
    /// Core may have committed past the last terminal job snapshot.
    pub(in crate::semantic) fn recover_interrupted_publication(
        &self,
        data_root: &Path,
    ) -> Result<bool> {
        let Some(job) = read_daemon_job_status(&daemon_source_backed_refresh_job_path(data_root))
        else {
            return Ok(false);
        };
        let verified = open_published_generation(data_root)?.map(Arc::new);
        let active_generation = verified
            .as_ref()
            .map(|verified| verified.generation_id().to_owned());
        let queued_successors = recover_queued_successors(&job, active_generation.clone())?;
        let recovered_continuations = recover_logical_demand_continuations(&job)?;
        if job.get("request_state").and_then(Value::as_str) == Some("published") {
            if let Some(verified) = verified.as_ref() {
                if let Ok(status_receipt) = published_refresh_receipt_for_index(&job, verified) {
                    if let Ok(metadata) = SourceBackedPublicationMetadata::decode(verified) {
                        if let Ok(durable_receipt) = published_refresh_receipt_for_index(
                            &metadata.response_value(),
                            verified,
                        ) {
                            if status_receipt.published_generation == verified.generation_id()
                                && durable_receipt == status_receipt
                            {
                                let attempt = SourceBackedRefreshAttempt::recovered_published(
                                    &job,
                                    &metadata,
                                    durable_receipt.clone(),
                                );
                                let terminal = CoreRefreshTerminalSuccess::bind(
                                    durable_receipt,
                                    Arc::clone(verified),
                                )?;
                                let has_successors = !queued_successors.is_empty();
                                {
                                    let mut state = self.lock_state();
                                    terminal.install(&mut state);
                                    state.attempts.push_back(attempt);
                                    install_recovered_successors(
                                        &mut state,
                                        queued_successors.clone(),
                                    )?;
                                    state
                                        .manual_all_continuations
                                        .extend(recovered_continuations.clone());
                                    state.current_published_generation = active_generation.clone();
                                    trim_terminal_attempt_history(&mut state);
                                }
                                let _ =
                                    self.finish_route_admissions(&metadata.request_id, true, None);
                                self.persist_job_status(data_root, &metadata.request_id)?;
                                return Ok(has_successors);
                            }
                        }
                    }
                }
            }
        }

        let request_state = job
            .get("request_state")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let previous_generation = job.get("previous_generation").and_then(Value::as_str);
        let pointer_advanced = active_generation.as_deref() != previous_generation;
        // A terminal job must always recover or reject its exact publication,
        // even when the persisted previous-generation pointer already equals
        // the active generation. Otherwise malformed terminal authority can
        // be ignored and queued successors can be lost on restart.
        if pointer_advanced || request_state == "published" {
            let active_generation = active_generation.ok_or_else(|| {
                anyhow!("interrupted source refresh advanced Core without an active generation")
            })?;
            let verified = verified.ok_or_else(|| {
                anyhow!("interrupted source refresh advanced Core without a verified generation")
            })?;
            if verified.publication_metadata().is_none() && request_state == "published" {
                // Publications written before the refresh control plane carry
                // no source-refresh receipt, so there is nothing exact to
                // recover. Accept the verified generation as terminal-complete
                // only when the legacy job names that exact publication, then
                // install any queued successors; the next scheduled refresh
                // publishes with metadata again. Non-terminal jobs, mismatched
                // generations, and present-but-malformed metadata keep failing
                // closed below.
                let job_generation = required_generation(
                    job.get("published_generation"),
                    "legacy published refresh generation",
                )?;
                if job_generation != active_generation {
                    bail!("legacy Core refresh job names a different published generation");
                }
                if !queued_successors.is_empty() {
                    let durable_request_id = {
                        let mut state = self.lock_state();
                        install_recovered_successors(&mut state, queued_successors)?;
                        state
                            .manual_all_continuations
                            .extend(recovered_continuations.clone());
                        state.current_published_generation = Some(active_generation);
                        state
                            .active_request_id
                            .as_deref()
                            .ok_or_else(|| {
                                anyhow!("recovered source refresh successor is unavailable")
                            })?
                            .to_owned()
                    };
                    self.persist_job_status(data_root, &durable_request_id)?;
                    return Ok(true);
                }
                self.lock_state().current_published_generation = Some(active_generation);
                return Ok(false);
            }
            let metadata = SourceBackedPublicationMetadata::decode(&verified)
                .context("recover exact terminal refresh receipt from Core publication metadata")?;
            let job_request_id = job
                .get("request_id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("interrupted source refresh job has no request ID"))?;
            if metadata.request_id != job_request_id {
                bail!("active Core refresh metadata belongs to a different request");
            }
            let job_operation = SourceBackedRefreshOperation::from_request_json(&job)?;
            let job_scope = refresh_scope_from_json(job.get("refresh_scope"))?;
            if metadata.operation != job_operation || metadata.refresh_scope != job_scope {
                bail!("active Core refresh metadata does not match the interrupted request");
            }
            let receipt =
                published_refresh_receipt_for_index(&metadata.response_value(), verified.as_ref())?;
            if receipt.published_generation != active_generation {
                bail!("active Core refresh metadata names a different generation");
            }
            let attempt =
                SourceBackedRefreshAttempt::recovered_published(&job, &metadata, receipt.clone());
            let terminal = CoreRefreshTerminalSuccess::bind(receipt, Arc::clone(&verified))?;
            let has_successors = !queued_successors.is_empty();
            {
                let mut state = self.lock_state();
                terminal.install(&mut state);
                state.attempts.push_back(attempt);
                install_recovered_successors(&mut state, queued_successors.clone())?;
                state
                    .manual_all_continuations
                    .extend(recovered_continuations.clone());
                state.current_published_generation = Some(active_generation);
                trim_terminal_attempt_history(&mut state);
            }
            let _ = self.finish_route_admissions(&metadata.request_id, true, None);
            self.persist_job_status(data_root, &metadata.request_id)?;
            return Ok(has_successors);
        }
        if request_state == "failed" && !queued_successors.is_empty() {
            let durable_request_id = {
                let mut state = self.lock_state();
                install_recovered_successors(&mut state, queued_successors)?;
                state
                    .manual_all_continuations
                    .extend(recovered_continuations.clone());
                state.current_published_generation = active_generation;
                state
                    .active_request_id
                    .as_deref()
                    .ok_or_else(|| anyhow!("recovered source refresh successor is unavailable"))?
                    .to_owned()
            };
            self.persist_job_status(data_root, &durable_request_id)?;
            return Ok(true);
        }
        let needs_recovery = matches!(request_state, "queued" | "running");
        if !needs_recovery {
            if let Some(verified) = verified {
                self.lock_state().current_published_generation =
                    Some(verified.generation_id().to_owned());
            }
            return Ok(false);
        }

        let root = recover_queued_root(&job, active_generation.clone())?;
        let request_id = root.request_id.clone();
        {
            let mut state = self.lock_state();
            if state.active_request_id.is_some() || !state.pending_request_ids.is_empty() {
                bail!("interrupted source refresh recovery conflicts with an active queue");
            }
            state.active_request_id = Some(request_id.clone());
            state.attempts.push_back(root);
            install_recovered_successors(&mut state, queued_successors)?;
            state
                .manual_all_continuations
                .extend(recovered_continuations);
            state.current_published_generation = active_generation;
        }
        self.persist_job_status(data_root, &request_id)?;
        Ok(true)
    }

    #[cfg(test)]
    pub(in super::super) fn enqueue(&self, observed_generation: Option<String>) -> Value {
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
    pub(in super::super) fn enqueue_fresh_demand_for_test(
        &self,
        observed_generation: Option<String>,
        request_id: String,
        admission_route_observations: BTreeMap<SourceRouteIdentity, Option<String>>,
    ) -> Result<Value> {
        self.enqueue_with_catalog_metadata(
            observed_generation,
            SourceRefreshRuntimeMetadata::default(),
            None,
            SourceBackedRefreshScope::All,
            SourceRefreshAdmissionRequirement::FreshAfterAdmittedSnapshot,
            admission_route_observations,
            Some(request_id),
        )
    }

    #[cfg(test)]
    fn enqueue_with_metadata(
        &self,
        observed_generation: Option<String>,
        metadata: SourceRefreshRuntimeMetadata,
    ) -> Value {
        self.enqueue_with_catalog_metadata(
            observed_generation,
            metadata,
            None,
            SourceBackedRefreshScope::All,
            SourceRefreshAdmissionRequirement::AttachEquivalent,
            BTreeMap::new(),
            None,
        )
        .expect("requests without catalog authority always coalesce")
    }

    pub(super) fn enqueue_with_catalog_metadata(
        &self,
        observed_generation: Option<String>,
        metadata: SourceRefreshRuntimeMetadata,
        requested_catalog: Option<ExplicitSourceCatalogAuthority>,
        refresh_scope: SourceBackedRefreshScope,
        admission: SourceRefreshAdmissionRequirement,
        mut admission_route_observations: BTreeMap<SourceRouteIdentity, Option<String>>,
        logical_request_id: Option<String>,
    ) -> Result<Value> {
        let mut state = self.lock_state();
        if let Some(existing) = logical_request_id
            .as_deref()
            .and_then(|request_id| find_attempt(&state, request_id))
        {
            return Ok(existing.to_json());
        }
        let is_manual_all = admission
            == SourceRefreshAdmissionRequirement::FreshAfterAdmittedSnapshot
            && refresh_scope == SourceBackedRefreshScope::All;
        let mut continuation_predecessor = None;
        if let Some(active_request_id) = state.active_request_id.clone() {
            if let Some(active) = find_attempt_mut(&mut state, &active_request_id) {
                if active.state.is_active() {
                    if is_manual_all && active.state == SourceBackedRefreshState::Running {
                        continuation_predecessor = Some(active.request_id.clone());
                    }
                    if admission.requires_successor(active.state) {
                        // A logical freshness demand attaches to the immutable
                        // physical attempt, then proves coverage after its
                        // publication instead of eagerly repeating the pass.
                    } else {
                        if let Some(requested_catalog) = requested_catalog.as_ref() {
                            let upgrades_queued_automatic =
                                active.requested_explicit_source_catalog.is_none()
                                    && active.state == SourceBackedRefreshState::Queued;
                            if upgrades_queued_automatic {
                                active.requested_explicit_source_catalog =
                                    Some(requested_catalog.clone());
                            }
                        }
                        let automatic_exact = active.trigger == "periodic"
                            && active.trigger_provenance == "daemon_scheduler"
                            && matches!(&active.refresh_scope, SourceBackedRefreshScope::Exact(_));
                        if is_manual_all
                            && automatic_exact
                            && active.state == SourceBackedRefreshState::Queued
                        {
                            active.refresh_scope = SourceBackedRefreshScope::All;
                            return Ok(coalesce_attempt(active, metadata));
                        }
                        if active.requested_explicit_source_catalog.as_ref()
                            == requested_catalog.as_ref()
                            && active.refresh_scope == refresh_scope
                        {
                            return Ok(coalesce_attempt(active, metadata));
                        }
                        // A running refresh is immutable. Preserve both catalog
                        // authorities by serializing the newer one as a successor.
                    }
                }
            }
        }

        if admission != SourceRefreshAdmissionRequirement::FreshAfterAdmittedSnapshot
            && requested_catalog.is_some()
        {
            let coalesced_request_id = state.pending_request_ids.iter().find_map(|request_id| {
                find_attempt(&state, request_id)
                    .filter(|attempt| {
                        attempt.state.is_active()
                            && attempt.requested_explicit_source_catalog.as_ref()
                                == requested_catalog.as_ref()
                            && attempt.refresh_scope == refresh_scope
                    })
                    .map(|attempt| attempt.request_id.clone())
            });
            if let Some(coalesced_request_id) = coalesced_request_id {
                let attempt = find_attempt_mut(&mut state, &coalesced_request_id)
                    .expect("pending source refresh attempt");
                return Ok(coalesce_attempt(attempt, metadata));
            }
        }

        let active_pending_requests = durable_queue_entry_count(&state);
        if active_pending_requests >= SOURCE_REFRESH_ACTIVE_PENDING_LIMIT {
            return Err(SourceBackedRefreshQueueFull {
                active_pending_requests,
            }
            .into());
        }

        let mut attempt = new_refresh_attempt(
            observed_generation,
            metadata,
            requested_catalog,
            refresh_scope,
        );
        if let Some(logical_request_id) = logical_request_id {
            attempt.request_id = logical_request_id;
        }
        let response = attempt.to_json();
        let request_id = attempt.request_id.clone();
        let terminal_persistence_owns_root = state.pending_terminal_persistence.is_some();
        let active_attempt_owns_root = state
            .active_request_id
            .as_deref()
            .and_then(|request_id| find_attempt(&state, request_id))
            .is_some_and(|attempt| attempt.state.is_active());
        if terminal_persistence_owns_root || active_attempt_owns_root {
            state.pending_request_ids.push_back(request_id.clone());
        } else {
            state.active_request_id = Some(request_id.clone());
        }
        if let Some(predecessor_request_id) = continuation_predecessor {
            let ledger_eligible_routes = state
                .known_route_ids
                .iter()
                .filter(|route| !admission_route_observations.contains_key(*route))
                .cloned()
                .collect();
            for route in &state.known_route_ids {
                admission_route_observations
                    .entry(route.clone())
                    .or_insert(None);
            }
            let admission_event_watermarks = admission_route_observations
                .keys()
                .filter_map(|route| {
                    state
                        .route_event_watermarks
                        .get(route)
                        .copied()
                        .map(|watermark| (route.clone(), watermark))
                })
                .collect();
            let predecessor_event_watermarks = state
                .route_admission_watermarks
                .get(&predecessor_request_id)
                .cloned()
                .unwrap_or_default();
            state.manual_all_continuations.insert(
                request_id,
                ManualAllContinuation::new(
                    predecessor_request_id,
                    admission_route_observations,
                    ledger_eligible_routes,
                    admission_event_watermarks,
                    predecessor_event_watermarks,
                ),
            );
        }
        state.attempts.push_back(attempt);
        trim_terminal_attempt_history(&mut state);
        Ok(response)
    }

    pub(in super::super) fn status(&self, request_id: &str) -> Option<Value> {
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

    fn refresh_scope(&self, request_id: &str) -> Option<SourceBackedRefreshScope> {
        let state = self.lock_state();
        find_attempt(&state, request_id).map(|attempt| attempt.refresh_scope.clone())
    }

    fn operation(&self, request_id: &str) -> Option<SourceBackedRefreshOperation> {
        let state = self.lock_state();
        find_attempt(&state, request_id).map(|attempt| attempt.operation)
    }

    #[cfg(test)]
    pub(in crate::semantic) fn request_catalog_authority_for_test(
        &self,
        request_id: &str,
    ) -> Option<ExplicitSourceCatalogAuthority> {
        let state = self.lock_state();
        find_attempt(&state, request_id)
            .and_then(|attempt| attempt.requested_explicit_source_catalog.clone())
    }

    fn admit_refresh_scope(
        &self,
        request_id: &str,
        scope: &SourceBackedRefreshScope,
    ) -> Result<(
        BTreeSet<SourceRouteIdentity>,
        SourceBackedRefreshCoveredPublication,
    )> {
        let now_ms = source_route_ledger_now_ms();
        let mut state = self.lock_state();
        if state.route_admissions.contains_key(request_id) {
            bail!("source refresh request `{request_id}` already has retained route admissions");
        }
        let known_route_ids = state.known_route_ids.clone();
        let mut covered_route_ids = if let Some(continuation) =
            state.manual_all_continuations.get_mut(request_id)
        {
            if !continuation.predecessor_finished {
                bail!(
                    "manual all-route continuation `{request_id}` started before its exact predecessor finished"
                );
            }
            let retained = continuation
                .covered_route_results
                .keys()
                .filter(|route| known_route_ids.contains(*route))
                .cloned()
                .collect::<BTreeSet<_>>();
            if retained.len() != continuation.covered_route_results.len() {
                continuation
                    .covered_route_results
                    .retain(|route, _| retained.contains(route));
                if continuation.covered_route_results.is_empty() {
                    continuation.covered_removed_source_count = 0;
                    continuation.covered_timings = SourceBackedRefreshTimings::default();
                }
            }
            retained
        } else {
            BTreeSet::new()
        };
        let admissions = match scope {
            SourceBackedRefreshScope::All => {
                let watermark = state.dirty_routes.seed_watermark();
                let routes = known_route_ids
                    .difference(&covered_route_ids)
                    .cloned()
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
                    .seed_exact_routes(routes, watermark, now_ms);
                let admissions = state.dirty_routes.admit_all();
                if admissions
                    .iter()
                    .any(|admission| covered_route_ids.contains(admission.route()))
                {
                    covered_route_ids.clear();
                    if let Some(continuation) = state.manual_all_continuations.get_mut(request_id) {
                        continuation.covered_route_results.clear();
                        continuation.covered_removed_source_count = 0;
                        continuation.covered_timings = SourceBackedRefreshTimings::default();
                    }
                }
                admissions
            }
            SourceBackedRefreshScope::Exact(routes) => {
                if routes.is_empty() || routes.len() > SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT {
                    bail!(
                        "daemon exact source refresh must admit between one and {SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT} routes"
                    );
                }
                state
                    .dirty_routes
                    .admit_exact_routes(routes, now_ms)
                    .ok_or_else(|| {
                        anyhow!("one or more exact source routes are no longer due for admission")
                    })?
            }
        };
        state
            .route_admissions
            .insert(request_id.to_owned(), admissions);
        let admitted_watermarks = state
            .route_admissions
            .get(request_id)
            .into_iter()
            .flatten()
            .filter_map(|admission| {
                state
                    .route_event_watermarks
                    .get(admission.route())
                    .copied()
                    .map(|watermark| (admission.route().clone(), watermark))
            })
            .collect::<BTreeMap<_, _>>();
        for continuation in state.manual_all_continuations.values_mut() {
            if continuation.predecessor_request_id == request_id {
                continuation.predecessor_event_watermarks = admitted_watermarks.clone();
            }
        }
        state
            .route_admission_watermarks
            .insert(request_id.to_owned(), admitted_watermarks);
        let covered_publication = state
            .manual_all_continuations
            .get(request_id)
            .map(ManualAllContinuation::covered_publication)
            .unwrap_or_default();
        Ok((covered_route_ids, covered_publication))
    }

    #[cfg(test)]
    pub(in super::super) fn admit_refresh_scope_for_test(
        &self,
        request_id: &str,
        scope: &SourceBackedRefreshScope,
    ) -> Result<BTreeSet<SourceRouteIdentity>> {
        self.admit_refresh_scope(request_id, scope)
            .map(|(routes, _)| routes)
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
        Published: FnOnce(&Value) -> Result<()>,
        Failed: FnOnce(&str) -> Result<()>,
    {
        let mut state = self.lock_state();
        let pending_retry = state
            .pending_terminal_persistence
            .as_ref()
            .and_then(|pending| {
                find_attempt(&state, &pending.request_id).map(|attempt| {
                    (
                        pending.request_id.clone(),
                        job_with_queued_successors(&state, pending.terminal_job.clone()),
                        pending.did_work(),
                        pending.failed(),
                        attempt.refresh_scope.clone(),
                    )
                })
            });
        if let Some((request_id, terminal_job, did_work, failed_run, refresh_scope)) = pending_retry
        {
            // Keep terminal retry publication under the admission lock. An
            // acknowledged successor must never be followed by an older
            // root snapshot reaching the same durable status path.
            let persistence = published(&terminal_job);
            if let Err(error) = persistence {
                let terminal_error = terminal_job
                    .get("last_error")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                {
                    let attempt = find_attempt_mut(&mut state, &request_id)?;
                    if failed_run {
                        attempt.state = SourceBackedRefreshState::Failed;
                        attempt.progress.phase = "failed".to_owned();
                        attempt.last_error = Some(format!(
                            "{terminal_error}; persist exact terminal refresh failure before acknowledgement: {error:#}"
                        ));
                    } else {
                        attempt.state = SourceBackedRefreshState::Running;
                        attempt.progress.phase = "persisting_terminal".to_owned();
                        attempt.failure_type = None;
                        attempt.last_error = Some(format!(
                            "persist exact terminal Core publication before acknowledgement: {error:#}"
                        ));
                    }
                }
                let job = durable_job_json(&state, &request_id)?;
                return Some(SourceBackedRefreshRun {
                    job,
                    did_work: false,
                    failed: failed_run,
                    terminal_persistence_pending: true,
                    scope: refresh_scope,
                    coverage_certificate: None,
                });
            }

            let pending = state.pending_terminal_persistence.take()?;
            let published_generation = match pending.outcome {
                PendingTerminalOutcome::Published { terminal, .. } => {
                    let receipt = terminal.install(&mut state);
                    let published_generation = receipt.published_generation.clone();
                    let attempt = find_attempt_mut(&mut state, &request_id)?;
                    attempt.state = SourceBackedRefreshState::Published;
                    attempt.progress.phase = "published".to_owned();
                    attempt.failure_type = None;
                    attempt.last_error = None;
                    state.current_published_generation = Some(published_generation.clone());
                    Some(published_generation)
                }
                PendingTerminalOutcome::Failed => {
                    let attempt = find_attempt_mut(&mut state, &request_id)?;
                    attempt.state = SourceBackedRefreshState::Failed;
                    attempt.progress.phase = "failed".to_owned();
                    attempt.last_error = pending
                        .terminal_job
                        .get("last_error")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    attempt.published_generation.clone()
                }
            };
            if failed_run {
                // The daemon still has to add its durable retry deadline to
                // this terminal root. Reserve the root's queue slot until
                // that lock-serialized write completes.
                state.pending_scheduler_retry_root_id = Some(request_id.clone());
            }
            advance_after_terminal_attempt(&mut state, &request_id, published_generation);
            trim_terminal_attempt_history(&mut state);
            drop(state);
            return Some(SourceBackedRefreshRun {
                job: terminal_job,
                did_work,
                failed: failed_run,
                terminal_persistence_pending: false,
                scope: refresh_scope,
                coverage_certificate: None,
            });
        }
        drop(state);

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
                    explicit_catalog_request_is_accounted_for(
                        requested,
                        publication.published_explicit_source_catalog.as_ref(),
                        &publication.catalog_route_bindings,
                        &publication.route_results,
                    )
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
        let verified = match verified {
            Ok((observed, publication)) => {
                let exact_scope_matches = match &refresh_scope {
                    SourceBackedRefreshScope::All => true,
                    SourceBackedRefreshScope::Exact(routes) => {
                        publication
                            .route_results
                            .iter()
                            .filter_map(|result| {
                                SourceRouteIdentity::from_sha256(result.route_identity.clone()).ok()
                            })
                            .collect::<BTreeSet<_>>()
                            == *routes
                            && publication.route_results.len() == routes.len()
                    }
                };
                if !exact_scope_matches {
                    Err("validate terminal Core publication: exact refresh omitted or added a selected route outcome".to_owned())
                } else {
                    SourceBackedRefreshReceipt::from_verified_publication(
                        previous_generation.clone(),
                        observed.clone(),
                        &publication,
                    )
                    .map_err(|error| format!("validate terminal Core publication: {error:#}"))
                    .and_then(|request_receipt| {
                        terminal(request_receipt.clone())
                            .map(|terminal| (observed, publication, request_receipt, terminal))
                            .map_err(|error| {
                                format!("finalize verified Core publication: {error:#}")
                            })
                    })
                }
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
        let mut terminal_persistence_pending = false;
        let (failed_run, did_work) = match verified {
            Ok((observed, publication, receipt, terminal)) => {
                let publication_receipt = terminal.publication_receipt().cloned();
                let did_work = {
                    let attempt = find_attempt_mut(&mut state, &request_id)?;
                    attempt.finished_at_ms = Some(utc_now().timestamp_millis());
                    attempt.progress.current_source = None;
                    attempt.progress.completed_records = None;
                    attempt.progress.completed_bytes = None;
                    attempt.state = SourceBackedRefreshState::Published;
                    attempt.published_generation = Some(observed.clone());
                    attempt.progress.phase = "published".to_owned();
                    attempt.progress.completed_sources = attempt.progress.total_sources;
                    attempt.scanned_routes = Some(publication.route_results.len());
                    attempt.unsupported_routes = Some(publication.unsupported_routes);
                    attempt.certified_source_count = Some(publication.certified_source_count);
                    attempt.certified_source_bytes = Some(publication.certified_source_bytes);
                    attempt.receipt = Some(receipt.clone());
                    attempt.publication_receipt = publication_receipt;
                    attempt.timings = Some(publication.timings);
                    attempt.failure_type = None;
                    attempt.last_error = None;
                    attempt.published_generation != previous_generation
                };
                let terminal_job = durable_job_json(&state, &request_id)?;
                if let Err(error) = published(&terminal_job) {
                    let attempt = find_attempt_mut(&mut state, &request_id)?;
                    attempt.state = SourceBackedRefreshState::Running;
                    attempt.progress.phase = "persisting_terminal".to_owned();
                    attempt.failure_type = None;
                    attempt.last_error = Some(format!(
                        "persist exact terminal Core publication before acknowledgement: {error:#}"
                    ));
                    state.pending_terminal_persistence = Some(PendingTerminalPersistence {
                        request_id: request_id.clone(),
                        terminal_job,
                        outcome: PendingTerminalOutcome::Published { terminal, did_work },
                    });
                    terminal_persistence_pending = true;
                } else {
                    terminal.install(&mut state);
                    newly_published_generation = Some(observed);
                }
                (false, did_work)
            }
            Err(error) => {
                {
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
                }
                let failure_job = durable_job_json(&state, &request_id)?;
                if let Err(persist_error) = published(&failure_job) {
                    let attempt = find_attempt_mut(&mut state, &request_id)?;
                    let original = attempt.last_error.take().unwrap_or_default();
                    attempt.last_error = Some(format!(
                        "{original}; persist exact terminal refresh failure before acknowledgement: {persist_error:#}"
                    ));
                    state.pending_terminal_persistence = Some(PendingTerminalPersistence {
                        request_id: request_id.clone(),
                        terminal_job: failure_job,
                        outcome: PendingTerminalOutcome::Failed,
                    });
                    terminal_persistence_pending = true;
                } else {
                    // The scheduler adds retry timing in a second durable
                    // write. Keep this failed root inside the shared queue
                    // bound until that write has completed.
                    state.pending_scheduler_retry_root_id = Some(request_id.clone());
                }
                (true, false)
            }
        };
        if newly_published_generation.is_some() {
            state.current_published_generation = newly_published_generation.clone();
        }
        if !terminal_persistence_pending {
            advance_after_terminal_attempt(
                &mut state,
                &request_id,
                newly_published_generation.or(observed_for_status),
            );
        }
        trim_terminal_attempt_history(&mut state);
        let job = find_attempt(&state, &request_id)?.job_json();
        drop(state);
        Some(SourceBackedRefreshRun {
            job,
            did_work: did_work && !terminal_persistence_pending,
            failed: failed_run,
            terminal_persistence_pending,
            scope: refresh_scope,
            coverage_certificate: None,
        })
    }

    #[cfg(test)]
    pub(in super::super) fn run_next_with<Execute, Probe, Published, Failed>(
        &self,
        execute: Execute,
        probe: Probe,
        published: Published,
        failed: Failed,
    ) -> Option<SourceBackedRefreshRun>
    where
        Execute: FnOnce(&str, &Self) -> Result<SourceBackedRefreshPublication>,
        Probe: FnOnce() -> Result<Option<String>>,
        Published: FnOnce(&Value) -> Result<()>,
        Failed: FnOnce(&str) -> Result<()>,
    {
        let run = self.run_next_with_terminal_success(
            execute,
            probe,
            |receipt| Ok(CoreRefreshTerminalSuccess::state_only(receipt)),
            published,
            failed,
        )?;
        let publication_ready = !run.failed && !run.terminal_persistence_pending;
        if let Some(request_id) = run.job.get("request_id").and_then(Value::as_str) {
            if !run.terminal_persistence_pending {
                let _ = self.finish_route_admissions(request_id, publication_ready, None);
            }
        }
        Some(run)
    }
}

fn publication_authority_receipt(
    pin: &VerifiedIndex,
    request_receipt: SourceBackedRefreshReceipt,
) -> Result<SourceBackedRefreshReceipt> {
    if pin.publication_metadata().is_none() {
        return missing_publication_metadata_receipt(request_receipt);
    }
    let metadata = SourceBackedPublicationMetadata::decode(pin)
        .context("decode durable Core refresh publication authority")?;
    published_refresh_receipt_for_index(&metadata.response_value(), pin)
        .context("validate durable Core refresh publication authority")
}

#[cfg(not(test))]
fn missing_publication_metadata_receipt(
    _request_receipt: SourceBackedRefreshReceipt,
) -> Result<SourceBackedRefreshReceipt> {
    bail!("verified Core generation has no durable source-refresh publication authority")
}

#[cfg(test)]
fn missing_publication_metadata_receipt(
    request_receipt: SourceBackedRefreshReceipt,
) -> Result<SourceBackedRefreshReceipt> {
    // State-machine unit tests use synthetic verified indexes. Production and
    // integration-test publications must always bind Core metadata above.
    Ok(request_receipt)
}
