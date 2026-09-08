use super::*;
mod admission_scope;
mod queued_batch;
mod recovery;
mod resolution;
use queued_batch::QueuedRefreshBatch;
pub(in crate::engine) use recovery::recover_automatic_retry_checkpoints;

impl CoreRefreshEngine {
    /// Fences admission after unbounded callback loss. Callback pressure only
    /// advances this constant-size authority boundary; the watcher worker owns
    /// catalog reconstruction and exhaustive route seeding.
    pub fn fence_watch_uncertainty(&self, watermark: EventWatermark) {
        let mut state = self.lock_state();
        state.watch_uncertain_through = Some(
            state
                .watch_uncertain_through
                .map_or(watermark, |current| current.max(watermark)),
        );
    }

    pub fn watch_uncertainty_pending(&self) -> bool {
        self.watch_uncertainty_watermark().is_some()
    }

    pub fn watch_uncertainty_watermark(&self) -> Option<EventWatermark> {
        self.lock_state().watch_uncertain_through
    }

    /// Restores the one watch catalog only after fresh construction and a
    /// successful physical rearm. Watch uncertainty always becomes fresh,
    /// separately-owned exhaustive maintenance; it never reopens a request
    /// whose publication has already been verified.
    pub fn complete_watch_uncertainty_recovery(
        &self,
        _data_root: &Path,
        catalog: SourceBackedWatchCatalog,
        covered_through: EventWatermark,
        observed_at_ms: u64,
    ) -> Result<bool> {
        let routes = catalog.route_ids().cloned().collect::<BTreeSet<_>>();
        let mut state = self.lock_state();
        let Some(current_uncertainty) = state.watch_uncertain_through else {
            return Ok(false);
        };
        state.dirty_routes.retain_exact_routes(&routes);
        state
            .hermes_routes_requiring_exhaustive_recovery
            .retain(|route| routes.contains(route));
        state
            .routes_requiring_exhaustive_reconciliation
            .retain(|route| routes.contains(route));
        state
            .route_event_watermarks
            .retain(|route, _| routes.contains(route));
        state.route_worksets.clear();
        state
            .automatic_retry_checkpoints
            .retain(|route, _| routes.contains(route));
        state.known_route_ids = routes.clone();
        state.watch_catalog = Some(catalog);
        state.watch_catalog_revision = state.watch_catalog_revision.saturating_add(1);
        state.watch_routes_initialized = true;
        if current_uncertainty > covered_through {
            state
                .routes_requiring_exhaustive_reconciliation
                .extend(routes.iter().cloned());
            state
                .dirty_routes
                .seed_exact_routes(routes, current_uncertainty, observed_at_ms);
            return Ok(false);
        }
        state
            .routes_requiring_exhaustive_reconciliation
            .extend(routes.iter().cloned());
        for route in &routes {
            state
                .route_event_watermarks
                .entry(route.clone())
                .and_modify(|current| *current = (*current).max(covered_through))
                .or_insert(covered_through);
        }
        state
            .dirty_routes
            .seed_exact_routes(routes, covered_through, observed_at_ms);
        state.watch_uncertain_through = None;
        Ok(true)
    }

    pub fn enqueue_periodic(&self, data_root: &Path) -> Result<Value> {
        let observed_generation = self.observed_published_generation(data_root)?;
        let mut state = self.lock_state();
        let paused_routes = state
            .automatic_retry_checkpoints
            .iter()
            .filter(|(_, checkpoint)| checkpoint.is_paused())
            .map(|(route, _)| route.clone())
            .collect::<BTreeSet<_>>();
        let refresh_scope = if paused_routes.is_empty() {
            SourceBackedRefreshScope::All
        } else {
            let healthy_routes = state
                .known_route_ids
                .difference(&paused_routes)
                .cloned()
                .collect::<BTreeSet<_>>();
            if healthy_routes.is_empty() {
                let request_id = state
                    .attempts
                    .back()
                    .map(|attempt| attempt.request_id.as_str())
                    .ok_or_else(|| anyhow!("paused automatic refresh has no durable status"))?;
                return projected_status_json(&state, request_id)
                    .ok_or_else(|| anyhow!("paused automatic refresh status disappeared"));
            }
            SourceBackedRefreshScope::Exact(healthy_routes)
        };
        let response = Self::enqueue_intent_locked(
            &mut state,
            observed_generation,
            SourceRefreshRuntimeMetadata::periodic(),
            RefreshIntent::AutomaticMaintenance,
            refresh_scope.clone(),
            None,
            None,
        )?;
        if let SourceBackedRefreshScope::Exact(routes) = refresh_scope {
            let watermark = state.dirty_routes.seed_watermark();
            for route in &routes {
                state
                    .route_event_watermarks
                    .entry(route.clone())
                    .and_modify(|current| *current = (*current).max(watermark))
                    .or_insert(watermark);
            }
            state.dirty_routes.seed_exact_routes(
                routes,
                watermark,
                source_route_ledger_now_ms().saturating_sub(1_000),
            );
        }
        trim_terminal_attempt_history(&mut state);
        Ok(response)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn enqueue(&self, observed_generation: Option<String>) -> Value {
        self.enqueue_with_metadata(observed_generation, SourceRefreshRuntimeMetadata::default())
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn enqueue_for_test(&self, observed_generation: Option<String>) -> Value {
        self.enqueue(observed_generation)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn enqueue_fresh_catalog_demand_for_test(
        &self,
        data_root: &Path,
        observed_generation: Option<String>,
        request_id: String,
        requested_catalog: ExplicitSourceCatalogAuthority,
    ) -> Result<Value> {
        let response = match self.enqueue_intent(
            observed_generation,
            SourceRefreshRuntimeMetadata {
                operation: SourceBackedRefreshOperation::Import,
                daemon_mode: "full".to_owned(),
                trigger: "import",
                trigger_provenance: "explicit_source_catalog",
            },
            RefreshIntent::SelectedImport(RefreshSelection::ExactSource(requested_catalog)),
            SourceBackedRefreshScope::All,
            Some(request_id),
            None,
        ) {
            Ok(response) => response,
            Err(error) => {
                if let Some(queue_full) = error.downcast_ref::<SourceBackedRefreshQueueFull>() {
                    return Ok(queue_full.to_json());
                }
                return Err(error);
            }
        };
        let request_id = response
            .get("request_id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("queued test source refresh has no request ID"))?;
        self.persist_job_status(data_root, request_id)?;
        Ok(response)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn enqueue_manual_all_demand_for_test(
        &self,
        data_root: &Path,
        observed_generation: Option<String>,
        request_id: String,
    ) -> Result<Value> {
        let response = match self.enqueue_intent(
            observed_generation,
            SourceRefreshRuntimeMetadata {
                operation: SourceBackedRefreshOperation::Import,
                daemon_mode: "full".to_owned(),
                trigger: "import",
                trigger_provenance: "import_command",
            },
            RefreshIntent::SelectedImport(RefreshSelection::All),
            SourceBackedRefreshScope::All,
            Some(request_id),
            None,
        ) {
            Ok(response) => response,
            Err(error) => {
                if let Some(queue_full) = error.downcast_ref::<SourceBackedRefreshQueueFull>() {
                    return Ok(queue_full.to_json());
                }
                return Err(error);
            }
        };
        let request_id = response
            .get("request_id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("queued test source refresh has no request ID"))?;
        self.persist_job_status(data_root, request_id)?;
        Ok(response)
    }

    #[cfg(any(test, feature = "test-support"))]
    fn enqueue_with_metadata(
        &self,
        observed_generation: Option<String>,
        metadata: SourceRefreshRuntimeMetadata,
    ) -> Value {
        let response = self
            .enqueue_intent(
                observed_generation,
                metadata,
                RefreshIntent::AutomaticMaintenance,
                SourceBackedRefreshScope::All,
                None,
                None,
            )
            .expect("requests without catalog authority always coalesce");
        let request_id = response["request_id"]
            .as_str()
            .expect("test refresh response has a request ID")
            .to_owned();
        let mut state = self.lock_state();
        if let Some(attempt) = find_attempt_mut(&mut state, &request_id) {
            if attempt.state == SourceBackedRefreshState::AdmissionPending {
                attempt.admitted_authority = Some(admitted_refresh_for_test(BTreeMap::new()));
                attempt.state = SourceBackedRefreshState::Queued;
            }
        }
        projected_status_json(&state, &request_id).unwrap_or(response)
    }

    pub(super) fn enqueue_intent(
        &self,
        observed_generation: Option<String>,
        metadata: SourceRefreshRuntimeMetadata,
        intent: RefreshIntent,
        refresh_scope: SourceBackedRefreshScope,
        request_id: Option<String>,
        request_fingerprint: Option<String>,
    ) -> Result<Value> {
        let mut state = self.lock_state();
        let response = Self::enqueue_intent_locked(
            &mut state,
            observed_generation,
            metadata,
            intent,
            refresh_scope,
            request_id,
            request_fingerprint,
        )?;
        trim_terminal_attempt_history(&mut state);
        Ok(response)
    }

    pub(super) fn enqueue_intent_locked(
        state: &mut CoreRefreshEngineState,
        observed_generation: Option<String>,
        metadata: SourceRefreshRuntimeMetadata,
        intent: RefreshIntent,
        refresh_scope: SourceBackedRefreshScope,
        logical_request_id: Option<String>,
        request_fingerprint: Option<String>,
    ) -> Result<Value> {
        let reconciliation_demand = match (&intent, &refresh_scope) {
            (RefreshIntent::AutomaticMaintenance, SourceBackedRefreshScope::Exact(_)) => {
                SourceBackedReconciliationDemand::Exhaustive
            }
            _ => intent.reconciliation_demand(),
        };
        if let Some(existing) = logical_request_id
            .as_deref()
            .and_then(|request_id| find_attempt(state, request_id))
        {
            if existing.request_fingerprint.as_ref() != request_fingerprint.as_ref() {
                return Err(SourceBackedRefreshIdempotencyConflict {
                    request_id: existing.request_id.clone(),
                }
                .into());
            }
            return projected_status_json(state, &existing.request_id)
                .ok_or_else(|| anyhow!("existing source refresh request disappeared"));
        }
        if logical_request_id.is_none() && intent == RefreshIntent::AutomaticMaintenance {
            let coalesced_request_id = state
                .active_request_id
                .iter()
                .chain(state.pending_request_ids.iter())
                .find_map(|request_id| {
                    find_attempt(state, request_id)
                        .filter(|attempt| {
                            attempt.state.is_active()
                                && attempt.intent == RefreshIntent::AutomaticMaintenance
                                && attempt.refresh_scope == refresh_scope
                                && attempt.reconciliation_demand >= reconciliation_demand
                        })
                        .map(|attempt| attempt.request_id.clone())
                });
            if let Some(coalesced_request_id) = coalesced_request_id {
                let attempt = find_attempt_mut(state, &coalesced_request_id)
                    .expect("pending source refresh attempt");
                return Ok(coalesce_attempt(attempt, metadata));
            }
        }

        let active_pending_requests = durable_queue_entry_count(state);
        if active_pending_requests >= SOURCE_REFRESH_ACTIVE_PENDING_LIMIT {
            return Err(SourceBackedRefreshQueueFull {
                active_pending_requests,
            }
            .into());
        }

        let mut attempt = new_refresh_attempt(observed_generation, metadata, intent, refresh_scope);
        if let Some(logical_request_id) = logical_request_id {
            attempt.request_id = logical_request_id;
        }
        attempt.request_fingerprint = request_fingerprint;
        attempt.reconciliation_demand = reconciliation_demand;
        attempt.automatic_retry_checkpoints = state.automatic_retry_checkpoints.clone();
        let request_id = attempt.request_id.clone();
        let terminal_persistence_owns_root = state.pending_terminal_persistence.is_some();
        let active_attempt_owns_root = state
            .active_request_id
            .as_deref()
            .and_then(|request_id| find_attempt(state, request_id))
            .is_some_and(|attempt| attempt.state.is_active());
        if terminal_persistence_owns_root || active_attempt_owns_root {
            state.pending_request_ids.push_back(request_id.clone());
        } else {
            state.active_request_id = Some(request_id.clone());
        }
        attempt.state = SourceBackedRefreshState::AdmissionPending;
        attempt.progress.phase = "admission_pending".to_owned();
        state.attempts.push_back(attempt);
        projected_status_json(state, &request_id)
            .ok_or_else(|| anyhow!("new source refresh request disappeared"))
    }

    pub(super) fn run_next_with_terminal_success<Execute, Probe, Terminal, Published, Failed>(
        &self,
        execute: Execute,
        probe: Probe,
        terminal: Terminal,
        mut published: Published,
        failed: Failed,
    ) -> Option<SourceBackedRefreshRun>
    where
        Execute: FnOnce(&str, &Self) -> Result<SourceBackedRefreshPublication>,
        Probe: FnOnce() -> Result<Option<String>>,
        Terminal: FnOnce(
            &str,
            SourceBackedRefreshReceipt,
        ) -> Result<(
            CoreRefreshTerminalSuccess,
            PostPublicationRouteCoverageFence,
        )>,
        Published: FnMut(&Value) -> Result<()>,
        Failed: FnOnce(&str) -> Result<()>,
    {
        let mut state = self.lock_state();
        let pending_retry = state
            .pending_terminal_persistence
            .as_ref()
            .and_then(|pending| {
                find_attempt(&state, &pending.request_id).map(|attempt| {
                    let coverage_certificate = match &pending.outcome {
                        PendingTerminalOutcome::Published {
                            coverage_certificate,
                            ..
                        } => coverage_certificate.clone(),
                        PendingTerminalOutcome::Failed { .. } => None,
                    };
                    (
                        pending.request_id.clone(),
                        job_with_queued_successors(&state, pending.terminal_job.clone()),
                        pending.did_work(),
                        pending.failed(),
                        pending.scheduler_retry(),
                        attempt.refresh_scope.clone(),
                        coverage_certificate,
                    )
                })
            });
        if let Some((
            request_id,
            terminal_job,
            did_work,
            failed_run,
            scheduler_retry,
            refresh_scope,
            coverage_certificate,
        )) = pending_retry
        {
            // Keep terminal retry publication under the admission lock. An
            // acknowledged successor must never be followed by an older
            // root snapshot reaching the same durable status path.
            if published(&terminal_job).is_err() {
                return Some(SourceBackedRefreshRun {
                    job: terminal_job,
                    did_work: false,
                    failed: failed_run,
                    terminal_persistence_pending: true,
                    scope: refresh_scope,
                    coverage_certificate: None,
                });
            }

            state.pending_terminal_persistence.take()?;
            let published_generation = find_attempt(&state, &request_id)
                .and_then(|attempt| attempt.published_generation.clone());
            if failed_run && scheduler_retry {
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
                coverage_certificate,
            });
        }
        drop(state);

        let (request_id, previous_generation, requested_catalog, refresh_scope, queued_batch) = {
            let mut state = self.lock_state();
            let request_id = state.active_request_id.clone()?;
            let queued_batch = QueuedRefreshBatch::snapshot(&state, &request_id);
            let attempt = find_attempt_mut(&mut state, &request_id)?;
            if attempt.state != SourceBackedRefreshState::Queued {
                return None;
            }
            attempt.state = SourceBackedRefreshState::Running;
            attempt.attempt_history_progress = Some(Default::default());
            attempt.started_at_ms = Some(utc_now().timestamp_millis());
            // The executor owns provider discovery. Persist the truthful live
            // phase before entering it so a long discovery cannot leave an
            // attached request apparently idle at `starting`.
            attempt.progress.phase = "discovering".to_owned();
            (
                request_id,
                attempt.previous_generation.clone(),
                attempt.requested_explicit_source_catalog().cloned(),
                attempt.refresh_scope.clone(),
                queued_batch,
            )
        };

        let execution = execute(&request_id, self);
        self.runtime.refresh_execution_finished();
        let attempted_routes = {
            let state = self.lock_state();
            state
                .route_admissions
                .get(&request_id)
                .map(|admissions| {
                    admissions
                        .iter()
                        .map(|admission| admission.route().clone())
                        .collect::<BTreeSet<_>>()
                })
                .filter(|routes| !routes.is_empty())
                .unwrap_or_else(|| match &refresh_scope {
                    SourceBackedRefreshScope::All => BTreeSet::new(),
                    SourceBackedRefreshScope::Exact(routes) => routes.clone(),
                })
        };
        let execution_failure_type = execution
            .as_ref()
            .err()
            .and_then(source_backed_refresh_failure_type);
        let execution_failure_outcome = execution
            .as_ref()
            .err()
            .map(|error| {
                source_backed_refresh_failure_outcome(error, &attempted_routes, &request_id)
            })
            .transpose()
            .ok()?;
        let verification_failure_outcome = source_backed_refresh_failure_outcome(
            &anyhow!("terminal source refresh verification failed"),
            &attempted_routes,
            &request_id,
        )
        .ok()?;
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
            (Ok(publication), Ok(observed)) => (
                Err(format!(
                    "source-backed refresh returned generation {}, but the verified published generation is {observed:?}",
                    publication.generation_id
                )),
                observed,
            ),
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
            (Err(error), Err(probe_error)) => (
                Err(format!(
                    "{}; verifying the retained generation also failed: {probe_error:#}",
                    source_backed_refresh_error_summary(&error)
                )),
                None,
            ),
        };
        let verified = match verified {
            Ok((observed, publication)) => {
                let exact_scope_mismatch = match &refresh_scope {
                    SourceBackedRefreshScope::All => None,
                    SourceBackedRefreshScope::Exact(routes) => {
                        let actual = publication
                            .route_results
                            .iter()
                            .filter_map(|result| {
                                SourceRouteIdentity::from_sha256(result.route_identity.clone()).ok()
                            })
                            .collect::<BTreeSet<_>>();
                        (actual != *routes || publication.route_results.len() != routes.len())
                            .then_some((routes, actual))
                    }
                };
                if let Some((expected, actual)) = exact_scope_mismatch {
                    Err(format!(
                        "validate terminal Core publication: exact refresh omitted or added a selected route outcome (expected={expected:?}, actual={actual:?}, result_count={})",
                        publication.route_results.len()
                    ))
                } else {
                    SourceBackedRefreshReceipt::from_verified_publication(
                        previous_generation.clone(),
                        observed.clone(),
                        &publication,
                    )
                    .map_err(|error| format!("validate terminal Core publication: {error:#}"))
                    .map(|request_receipt| (observed, publication, request_receipt))
                }
            }
            Err(error) => Err(error),
        };
        let verified = verified.and_then(|(observed, publication, request_receipt)| {
            terminal(&request_id, request_receipt.clone())
                .map(|(terminal, coverage_fence)| {
                    (
                        observed,
                        publication,
                        request_receipt,
                        terminal,
                        coverage_fence,
                    )
                })
                .map_err(|error| format!("finalize verified Core publication: {error:#}"))
        });
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
        // Publication receipts and terminal progress are authoritative. Do
        // not project transient producer facts once this attempt exits.
        if let Some(attempt) = find_attempt_mut(&mut state, &request_id) {
            attempt.snapshot_attempt_history_progress();
            attempt.attempt_history_progress = None;
        }
        let mut terminal_persistence_pending = false;
        let mut covered_batch = None;
        let (failed_run, did_work, mut coverage_certificate, mut terminal_job) = match verified {
            Ok((observed, publication, receipt, terminal, coverage_fence)) => {
                covered_batch = queued_batch
                    .and_then(|batch| batch.bind_capture(&state, &request_id, &terminal));
                let request_source_count = terminal.request_source_count(&receipt);
                let did_work = {
                    let attempt = find_attempt_mut(&mut state, &request_id)?;
                    attempt.finished_at_ms = Some(utc_now().timestamp_millis());
                    attempt.progress.current_source = None;
                    attempt.progress.completed_records = None;
                    attempt.progress.completed_bytes = None;
                    attempt.progress.current_source_progress = None;
                    attempt.whole_run_eta.clear();
                    attempt.state = SourceBackedRefreshState::Published;
                    attempt.published_generation = Some(observed.clone());
                    attempt.progress.phase = "published".to_owned();
                    attempt.progress.completed_sources = publication.route_results.len();
                    attempt.progress.total_sources = publication.route_results.len();
                    attempt.progress_total_sources_known = true;
                    attempt.scanned_routes = Some(publication.route_results.len());
                    attempt.unsupported_routes = Some(publication.unsupported_routes);
                    attempt.request_source_count = Some(request_source_count);
                    attempt.certified_source_count = Some(publication.certified_source_count);
                    attempt.certified_source_bytes = Some(publication.certified_source_bytes);
                    attempt.receipt = Some(receipt.clone());
                    attempt.timings = Some(publication.timings);
                    attempt.failure_type = None;
                    attempt.terminal_outcome = None;
                    attempt.last_error = None;
                    attempt.published_generation != previous_generation
                };
                terminal.install(&mut state);
                state.current_published_generation = Some(observed.clone());
                update_automatic_retry_after_publication(&mut state, &request_id);
                let finish = Self::finish_route_admissions_locked(
                    &mut state,
                    &request_id,
                    true,
                    Some(&coverage_fence),
                );
                let terminal_job = durable_job_json(&state, &request_id)?;
                if published(&terminal_job).is_err() {
                    state.pending_terminal_persistence = Some(PendingTerminalPersistence {
                        request_id: request_id.clone(),
                        terminal_job: terminal_job.clone(),
                        outcome: PendingTerminalOutcome::Published {
                            did_work,
                            coverage_certificate: finish.coverage_certificate.clone(),
                        },
                    });
                    terminal_persistence_pending = true;
                }
                (false, did_work, finish.coverage_certificate, terminal_job)
            }
            Err(error) => {
                let terminal_outcome = execution_failure_outcome
                    .unwrap_or(verification_failure_outcome)
                    .with_failure_context(observed_for_status.clone(), Some(error.clone()))
                    .ok()?;
                {
                    let attempt = find_attempt_mut(&mut state, &request_id)?;
                    attempt.finished_at_ms = Some(utc_now().timestamp_millis());
                    attempt.progress.current_source = None;
                    attempt.progress.completed_records = None;
                    attempt.progress.completed_bytes = None;
                    attempt.progress.current_source_progress = None;
                    attempt.whole_run_eta.clear();
                    if observed_for_status.is_some() {
                        attempt.published_generation = observed_for_status.clone();
                    }
                    attempt.state = SourceBackedRefreshState::Failed;
                    attempt.progress.phase = "failed".to_owned();
                    attempt.failure_type = execution_failure_type;
                    attempt.terminal_outcome = Some(terminal_outcome);
                    attempt.last_error = Some(error);
                }
                update_automatic_retry_after_failure(&mut state, &request_id);
                // Reserve a scheduler handoff only for failures that are not
                // already represented by exact route retry/block state. Route
                // finalization clears this provisional root when it restores
                // affected-route ownership.
                state.pending_scheduler_retry_root_id = Some(request_id.clone());
                Self::finish_route_admissions_locked(&mut state, &request_id, false, None);
                let scheduler_retry =
                    state.pending_scheduler_retry_root_id.as_deref() == Some(request_id.as_str());
                let failure_job = durable_job_json(&state, &request_id)?;
                if published(&failure_job).is_err() {
                    state.pending_terminal_persistence = Some(PendingTerminalPersistence {
                        request_id: request_id.clone(),
                        terminal_job: failure_job.clone(),
                        outcome: PendingTerminalOutcome::Failed { scheduler_retry },
                    });
                    terminal_persistence_pending = true;
                }
                (true, false, None, failure_job)
            }
        };
        if !terminal_persistence_pending {
            let published_generation = find_attempt(&state, &request_id)
                .and_then(|attempt| attempt.published_generation.clone())
                .or(observed_for_status);
            advance_after_terminal_attempt(&mut state, &request_id, published_generation);
            if let Some(batch) = covered_batch {
                if let Some(run) = batch.publish_covered_members(
                    &mut state,
                    &request_id,
                    coverage_certificate.as_ref(),
                    did_work,
                    &mut published,
                ) {
                    terminal_job = run.job;
                    coverage_certificate = run.coverage_certificate;
                    terminal_persistence_pending = run.terminal_persistence_pending;
                }
            }
        }
        trim_terminal_attempt_history(&mut state);
        drop(state);
        Some(SourceBackedRefreshRun {
            job: terminal_job,
            did_work: did_work && !terminal_persistence_pending,
            failed: failed_run,
            terminal_persistence_pending,
            scope: refresh_scope,
            coverage_certificate: (!terminal_persistence_pending)
                .then_some(coverage_certificate)
                .flatten(),
        })
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn run_next_with<Execute, Probe, Published, Failed>(
        &self,
        execute: Execute,
        probe: Probe,
        published: Published,
        failed: Failed,
    ) -> Option<SourceBackedRefreshRun>
    where
        Execute: FnOnce(&str, &Self) -> Result<SourceBackedRefreshPublication>,
        Probe: FnOnce() -> Result<Option<String>>,
        Published: FnMut(&Value) -> Result<()>,
        Failed: FnOnce(&str) -> Result<()>,
    {
        let run = self.run_next_with_terminal_success(
            execute,
            probe,
            |_, receipt| {
                Ok((
                    CoreRefreshTerminalSuccess::state_only(receipt),
                    PostPublicationRouteCoverageFence::fail_closed(),
                ))
            },
            published,
            failed,
        )?;
        Some(run)
    }
}

fn update_automatic_retry_after_publication(state: &mut CoreRefreshEngineState, request_id: &str) {
    let completed_routes = find_attempt(state, request_id)
        .and_then(|attempt| attempt.receipt.as_ref())
        .map(|receipt| {
            receipt
                .route_results
                .iter()
                .filter(|result| {
                    result.outcome.is_success()
                        && source_backed_route_retry_disposition(result).is_none()
                })
                .filter_map(|result| {
                    SourceRouteIdentity::from_sha256(result.route_identity.clone()).ok()
                })
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    for route in completed_routes {
        state.automatic_retry_checkpoints.remove(&route);
    }
    let checkpoints = state.automatic_retry_checkpoints.clone();
    if let Some(attempt) = find_attempt_mut(state, request_id) {
        attempt.automatic_retry_checkpoints = checkpoints;
    }
}

fn update_automatic_retry_after_failure(state: &mut CoreRefreshEngineState, request_id: &str) {
    let Some((outcome, observations, terminal_error)) =
        find_attempt(state, request_id).and_then(|attempt| {
            let outcome = attempt.terminal_outcome.as_ref()?;
            Some((
                outcome.clone(),
                attempt.route_observations.clone(),
                attempt.last_error.clone().unwrap_or_default(),
            ))
        })
    else {
        return;
    };

    let mut newly_paused = BTreeSet::new();
    if outcome.is_automatic_retry_eligible() {
        for route in outcome.retryable_routes() {
            let Some(observation) = observations.get(route) else {
                continue;
            };
            let candidate = SourceBackedAutomaticRetryCheckpoint::confirming(
                &outcome,
                route,
                observation,
                &terminal_error,
            );
            let can_insert = state.automatic_retry_checkpoints.contains_key(route)
                || state.automatic_retry_checkpoints.len() < SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT;
            match state.automatic_retry_checkpoints.get_mut(route) {
                Some(checkpoint) if checkpoint.matches(&candidate) => {
                    checkpoint.pause();
                    newly_paused.insert(route.clone());
                }
                Some(checkpoint) => *checkpoint = candidate,
                None if can_insert => {
                    state
                        .automatic_retry_checkpoints
                        .insert(route.clone(), candidate);
                }
                None => {}
            }
        }
    }

    let checkpoints = state.automatic_retry_checkpoints.clone();
    if let Some(attempt) = find_attempt_mut(state, request_id) {
        if let Some(outcome) = attempt.terminal_outcome.as_mut() {
            outcome.pause_automatic_retry_routes(&newly_paused);
        }
        attempt.automatic_retry_checkpoints = checkpoints;
    }
}

pub(in crate::engine) fn rearm_build_changed_automatic_retry_checkpoints(
    attempt: &mut SourceBackedRefreshAttempt,
) -> BTreeSet<SourceRouteIdentity> {
    let rearmed = attempt
        .automatic_retry_checkpoints
        .iter()
        .filter(|(_, checkpoint)| checkpoint.build_version != SOURCE_REFRESH_BUILD_VERSION)
        .map(|(route, _)| route.clone())
        .collect::<BTreeSet<_>>();
    for route in &rearmed {
        attempt.automatic_retry_checkpoints.remove(route);
    }
    if let Some(outcome) = attempt.terminal_outcome.as_mut() {
        outcome.rearm_automatic_retry_routes(&rearmed);
    }
    rearmed
}
