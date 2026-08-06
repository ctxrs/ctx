use super::*;
mod recovery;
mod resolution;

impl CoreRefreshEngine {
    pub fn enqueue_periodic(&self, data_root: &Path) -> Result<Value> {
        let observed_generation = self.observed_published_generation(data_root)?;
        self.enqueue_with_catalog_metadata(
            observed_generation,
            SourceRefreshRuntimeMetadata::periodic(),
            None,
            SourceBackedRefreshScope::All,
            SourceRefreshLogicalDemand {
                admission: SourceRefreshAdmissionRequirement::AttachEquivalent,
                route_observations: BTreeMap::new(),
                request_id: None,
                request_fingerprint: None,
                admission_pending: false,
            },
        )
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
    pub fn enqueue_fresh_demand_for_test(
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
            SourceRefreshLogicalDemand {
                admission: SourceRefreshAdmissionRequirement::FreshAfterAdmittedSnapshot,
                route_observations: admission_route_observations,
                request_id: Some(request_id),
                request_fingerprint: None,
                admission_pending: false,
            },
        )
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn enqueue_fresh_catalog_demand_for_test(
        &self,
        data_root: &Path,
        observed_generation: Option<String>,
        request_id: String,
        requested_catalog: ExplicitSourceCatalogAuthority,
    ) -> Result<Value> {
        let response = match self.enqueue_with_catalog_metadata(
            observed_generation,
            SourceRefreshRuntimeMetadata {
                operation: SourceBackedRefreshOperation::Import,
                daemon_mode: "full".to_owned(),
                trigger: "import",
                trigger_provenance: "explicit_source_catalog",
            },
            Some(requested_catalog),
            SourceBackedRefreshScope::All,
            SourceRefreshLogicalDemand {
                admission: SourceRefreshAdmissionRequirement::FreshAfterAdmittedSnapshot,
                route_observations: BTreeMap::new(),
                request_id: Some(request_id),
                request_fingerprint: None,
                admission_pending: false,
            },
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
        self.enqueue_with_catalog_metadata(
            observed_generation,
            metadata,
            None,
            SourceBackedRefreshScope::All,
            SourceRefreshLogicalDemand {
                admission: SourceRefreshAdmissionRequirement::AttachEquivalent,
                route_observations: BTreeMap::new(),
                request_id: None,
                request_fingerprint: None,
                admission_pending: false,
            },
        )
        .expect("requests without catalog authority always coalesce")
    }

    pub(super) fn enqueue_with_catalog_metadata(
        &self,
        observed_generation: Option<String>,
        metadata: SourceRefreshRuntimeMetadata,
        requested_catalog: Option<ExplicitSourceCatalogAuthority>,
        refresh_scope: SourceBackedRefreshScope,
        logical_demand: SourceRefreshLogicalDemand,
    ) -> Result<Value> {
        let mut state = self.lock_state();
        let response = Self::enqueue_with_catalog_metadata_locked(
            &mut state,
            observed_generation,
            metadata,
            requested_catalog,
            refresh_scope,
            logical_demand,
        )?;
        trim_terminal_attempt_history(&mut state);
        Ok(response)
    }

    pub(super) fn enqueue_with_catalog_metadata_locked(
        state: &mut CoreRefreshEngineState,
        observed_generation: Option<String>,
        metadata: SourceRefreshRuntimeMetadata,
        requested_catalog: Option<ExplicitSourceCatalogAuthority>,
        refresh_scope: SourceBackedRefreshScope,
        logical_demand: SourceRefreshLogicalDemand,
    ) -> Result<Value> {
        let SourceRefreshLogicalDemand {
            admission,
            route_observations: mut admission_route_observations,
            request_id: logical_request_id,
            request_fingerprint,
            admission_pending,
        } = logical_demand;
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
        let is_manual_all = admission
            == SourceRefreshAdmissionRequirement::FreshAfterAdmittedSnapshot
            && refresh_scope == SourceBackedRefreshScope::All;
        let mut continuation_predecessor = None;
        if let Some(active_request_id) = state.active_request_id.clone() {
            if let Some(active) = find_attempt_mut(state, &active_request_id) {
                if active.state.is_active() {
                    if is_manual_all {
                        continuation_predecessor = Some(active.request_id.clone());
                        if active.state == SourceBackedRefreshState::Queued {
                            if let Some(requested_catalog) = requested_catalog.as_ref() {
                                if active.requested_explicit_source_catalog.is_none() {
                                    active.requested_explicit_source_catalog =
                                        Some(requested_catalog.clone());
                                }
                            }
                            active.refresh_scope = SourceBackedRefreshScope::All;
                            let _ = coalesce_attempt(active, metadata.clone());
                            active.coalesced_logical_demands =
                                active.coalesced_logical_demands.saturating_add(1);
                        } else {
                            active.coalesced_logical_demands =
                                active.coalesced_logical_demands.saturating_add(1);
                        }
                    } else if admission.requires_successor(active.state) {
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
                find_attempt(state, request_id)
                    .filter(|attempt| {
                        attempt.state.is_active()
                            && attempt.requested_explicit_source_catalog.as_ref()
                                == requested_catalog.as_ref()
                            && attempt.refresh_scope == refresh_scope
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

        let mut attempt = new_refresh_attempt(
            observed_generation,
            metadata,
            requested_catalog,
            refresh_scope,
        );
        if let Some(logical_request_id) = logical_request_id {
            attempt.request_id = logical_request_id;
            attempt.physical_attempt_id = Some(attempt.request_id.clone());
        }
        attempt.request_fingerprint = request_fingerprint;
        attempt.fresh_after_admitted_snapshot =
            admission == SourceRefreshAdmissionRequirement::FreshAfterAdmittedSnapshot;
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
        if let Some(predecessor_request_id) = continuation_predecessor {
            attempt.coalesced_into_request_id = Some(predecessor_request_id.clone());
            attempt.physical_attempt_id = Some(predecessor_request_id.clone());
            let continuation = if admission_pending {
                ManualAllContinuation::pending(predecessor_request_id)
            } else {
                let ledger_eligible_routes = state
                    .known_route_ids
                    .iter()
                    .filter(|route| {
                        admission_route_observations
                            .get(*route)
                            .is_none_or(Option::is_none)
                    })
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
                ManualAllContinuation::new(
                    predecessor_request_id,
                    admission_route_observations,
                    ledger_eligible_routes,
                    admission_event_watermarks,
                    predecessor_event_watermarks,
                )
            };
            state
                .manual_all_continuations
                .insert(request_id.clone(), continuation);
        }
        if admission_pending {
            attempt.state = SourceBackedRefreshState::AdmissionPending;
            attempt.progress.phase = "admission_pending".to_owned();
        }
        state.attempts.push_back(attempt);
        projected_status_json(state, &request_id)
            .ok_or_else(|| anyhow!("new source refresh request disappeared"))
    }

    pub fn status(&self, request_id: &str) -> Option<RefreshStatus> {
        let state = self.lock_state();
        projected_status_json(&state, request_id).map(RefreshStatus::from_schema_v1_fields)
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

    #[cfg(any(test, feature = "test-support"))]
    pub fn request_catalog_authority_for_test(
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

    #[cfg(any(test, feature = "test-support"))]
    pub fn admit_refresh_scope_for_test(
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
                        pending.scheduler_retry(),
                        attempt.refresh_scope.clone(),
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
        )) = pending_retry
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
                PendingTerminalOutcome::Failed { .. } => {
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
            attempt.physical_attempt_id = Some(request_id.clone());
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
            .map(|error| source_backed_refresh_failure_outcome(error, &attempted_routes));
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
                let request_source_count = terminal.request_source_count(&receipt);
                let did_work = {
                    let attempt = find_attempt_mut(&mut state, &request_id)?;
                    attempt.finished_at_ms = Some(utc_now().timestamp_millis());
                    attempt.progress.current_source = None;
                    attempt.progress.completed_records = None;
                    attempt.progress.completed_bytes = None;
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
                    attempt.publication_receipt = publication_receipt;
                    attempt.timings = Some(publication.timings);
                    attempt.failure_type = None;
                    attempt.failure_outcome = None;
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
                    attempt.failure_outcome =
                        Some(execution_failure_outcome.unwrap_or_else(|| {
                            source_backed_refresh_failure_outcome(
                                &anyhow!("terminal source refresh verification failed"),
                                &attempted_routes,
                            )
                        }));
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
                        outcome: PendingTerminalOutcome::Failed {
                            scheduler_retry: true,
                        },
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

#[cfg(not(any(test, feature = "test-support")))]
fn missing_publication_metadata_receipt(
    _request_receipt: SourceBackedRefreshReceipt,
) -> Result<SourceBackedRefreshReceipt> {
    bail!("verified Core generation has no durable source-refresh publication authority")
}

#[cfg(any(test, feature = "test-support"))]
fn missing_publication_metadata_receipt(
    request_receipt: SourceBackedRefreshReceipt,
) -> Result<SourceBackedRefreshReceipt> {
    // State-machine unit tests use synthetic verified indexes. Production and
    // integration-test publications must always bind Core metadata above.
    Ok(request_receipt)
}
