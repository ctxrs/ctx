use super::*;

impl CoreRefreshEngine {
    pub fn enqueue_next_scheduled_refresh(&self, data_root: &Path, now_ms: u64) -> Result<bool> {
        self.enqueue_next_dirty_route_with_cold_all(data_root, now_ms, true)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn enqueue_next_dirty_route(&self, data_root: &Path, now_ms: u64) -> Result<bool> {
        self.enqueue_next_dirty_route_with_cold_all(data_root, now_ms, false)
    }

    fn enqueue_next_dirty_route_with_cold_all(
        &self,
        data_root: &Path,
        now_ms: u64,
        cold_all: bool,
    ) -> Result<bool> {
        let observed_generation = self.observed_published_generation(data_root)?;
        let automatic_split_pending = self.automatic_split_pending_for_watch(data_root)?;
        let all_scope_widening_possible =
            automatic_split_pending || (cold_all && observed_generation.is_none());
        let (sampled_routes, observation_routes, catalog, catalog_revision) = {
            let state = self.lock_state();
            if durable_queue_entry_count(&state) != 0 {
                return Ok(false);
            }
            let sampled_routes = state
                .dirty_routes
                .due_routes(now_ms, SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT);
            let mut observation_routes = sampled_routes.clone();
            if all_scope_widening_possible {
                observation_routes.extend(state.automatic_retry_checkpoints.keys().cloned());
            }
            (
                sampled_routes,
                observation_routes,
                state.watch_catalog.clone(),
                state.watch_catalog_revision,
            )
        };
        if sampled_routes.is_empty() {
            return Ok(false);
        }
        // Route certification may touch provider files. Keep it outside the
        // engine-state mutex, then reject the sample if catalog authority moved.
        let sampled_observations = catalog.as_ref().map(|catalog| {
            source_backed_requested_route_observations(catalog, &observation_routes)
        });
        let request_id = {
            let mut state = self.lock_state();
            if durable_queue_entry_count(&state) != 0
                || state.watch_catalog_revision != catalog_revision
            {
                return Ok(false);
            }
            let currently_due = state
                .dirty_routes
                .due_routes(now_ms, SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT);
            let mut routes = sampled_routes
                .intersection(&currently_due)
                .cloned()
                .collect::<BTreeSet<_>>();
            if routes.is_empty() {
                return Ok(false);
            }
            let paused_routes_present = reconcile_due_automatic_retry_routes(
                &mut state,
                &mut routes,
                sampled_observations.as_ref(),
            );
            if routes.is_empty() {
                return Ok(false);
            }
            let retry_intent = routes
                .iter()
                .find_map(|route| state.route_retry_intents.get(route).cloned());
            let routes = retry_intent.as_ref().map_or(routes.clone(), |intent| {
                routes
                    .iter()
                    .filter(|route| {
                        state
                            .route_retry_intents
                            .get(*route)
                            .is_some_and(|stored| stored.as_ref() == intent.as_ref())
                    })
                    .cloned()
                    .collect()
            });
            let requires_exhaustive_recovery = routes.iter().any(|route| {
                state
                    .hermes_routes_requiring_exhaustive_recovery
                    .contains(route)
                    || state
                        .routes_requiring_exhaustive_reconciliation
                        .contains(route)
            });
            let refresh_scope = if automatic_split_pending && !paused_routes_present {
                // A released collapsed identity is still active. Exact watch
                // work cannot establish the complete role cohort required to
                // bridge or retire it, so retain the event as the trigger but
                // admit a single all-route exhaustive migration attempt.
                SourceBackedRefreshScope::All
            } else if retry_intent.is_none()
                && cold_all
                && observed_generation.is_none()
                && !paused_routes_present
            {
                // A cold generation has no retained routes to carry. Publish
                // the complete startup inventory atomically instead of one
                // transient partial generation per initially dirty route.
                SourceBackedRefreshScope::All
            } else {
                // A paused route must never be pulled back in by an all-route
                // migration or cold-start scan. Healthy exact-route work can
                // still proceed while the migration waits for rearming.
                SourceBackedRefreshScope::Exact(routes)
            };
            let mut attempt = new_refresh_attempt(
                observed_generation,
                SourceRefreshRuntimeMetadata::periodic(),
                retry_intent
                    .as_deref()
                    .cloned()
                    .unwrap_or(RefreshIntent::AutomaticMaintenance),
                refresh_scope,
            );
            attempt.state = SourceBackedRefreshState::AdmissionPending;
            attempt.progress.phase = "admission_pending".to_owned();
            if requires_exhaustive_recovery || automatic_split_pending {
                attempt.reconciliation_demand = SourceBackedReconciliationDemand::Exhaustive;
            }
            attempt.automatic_retry_checkpoints = state.automatic_retry_checkpoints.clone();
            let request_id = attempt.request_id.clone();
            state.active_request_id = Some(request_id.clone());
            state.attempts.push_back(attempt);
            trim_terminal_attempt_history(&mut state);
            request_id
        };
        self.persist_job_status(data_root, &request_id)?;
        Ok(true)
    }

    fn automatic_split_pending_for_watch(&self, data_root: &Path) -> Result<bool> {
        let catalog = self.lock_state().watch_catalog.clone();
        let Some(catalog) = catalog else {
            return Ok(false);
        };
        let Some(index) = open_published_generation(data_root, self.journal.as_ref())? else {
            return Ok(false);
        };
        Ok(index
            .manifest()
            .source_routes()
            .iter()
            .any(|route| catalog.has_automatic_split_legacy_route(route.route_identity())))
    }

    pub(super) fn background_maintenance_wake_response(
        &self,
        data_root: &Path,
        request_id: String,
    ) -> Result<Value> {
        let published_generation = self.observed_published_generation(data_root)?;
        let metadata = self
            .runtime
            .metadata(data_root, SourceBackedRefreshOperation::Refresh);
        Ok(compact_json(json!({
            "ok": true,
            "schema_version": 1,
            "owner": "daemon",
            "request_id": request_id,
            "logical_request_id": request_id,
            "request_state": "queued",
            "logical_phase": "waiting",
            "previous_generation": published_generation.clone(),
            "published_generation": published_generation,
            "progress": {
                "phase": "maintenance_wake",
                "completed_sources": 0,
                "total_sources": 0,
                "total_sources_known": false,
            },
            "daemon_mode": metadata.daemon_mode.as_str(),
            "trigger": metadata.trigger,
            "trigger_provenance": metadata.trigger_provenance,
            "maintenance_wake": true,
        })))
    }

    pub(super) fn finish_route_admissions(
        &self,
        request_id: &str,
        publication_ready: bool,
        post_publication_fence: Option<&PostPublicationRouteCoverageFence>,
    ) -> RouteAdmissionFinish {
        let mut state = self.lock_state();
        Self::finish_route_admissions_locked(
            &mut state,
            request_id,
            publication_ready,
            post_publication_fence,
        )
    }

    pub(super) fn finish_route_admissions_and_persist(
        &self,
        data_root: &Path,
        request_id: &str,
        publication_ready: bool,
        post_publication_fence: Option<&PostPublicationRouteCoverageFence>,
    ) -> Result<RouteAdmissionFinish> {
        let mut state = self.lock_state();
        let finish = Self::finish_route_admissions_locked(
            &mut state,
            request_id,
            publication_ready,
            post_publication_fence,
        );
        let job = durable_job_json(&state, &finish.durable_request_id).ok_or_else(|| {
            anyhow!(
                "source refresh request `{}` disappeared during route finalization",
                finish.durable_request_id
            )
        })?;
        if let Err(error) = self.write_status(data_root, &job) {
            if finish.durable_request_id != request_id {
                state.pending_terminal_persistence = Some(PendingTerminalPersistence {
                    request_id: finish.durable_request_id.clone(),
                    terminal_job: job,
                    outcome: PendingTerminalOutcome::Failed {
                        scheduler_retry: false,
                    },
                });
            }
            return Err(error);
        }
        Ok(finish)
    }

    fn finish_route_admissions_locked(
        state: &mut CoreRefreshEngineState,
        request_id: &str,
        publication_ready: bool,
        post_publication_fence: Option<&PostPublicationRouteCoverageFence>,
    ) -> RouteAdmissionFinish {
        let now_ms = source_route_ledger_now_ms();
        let admissions = state
            .route_admissions
            .remove(request_id)
            .unwrap_or_default();
        let predecessor_event_watermarks = state
            .route_admission_watermarks
            .remove(request_id)
            .unwrap_or_default();
        let attempt = find_attempt(state, request_id).cloned();
        let selected_retry_intent = attempt.as_ref().and_then(|attempt| {
            matches!(attempt.intent, RefreshIntent::SelectedImport(_))
                .then(|| Arc::new(attempt.intent.clone()))
        });
        let route_results = attempt
            .as_ref()
            .and_then(|attempt| attempt.receipt.as_ref())
            .map(|receipt| {
                receipt
                    .route_results
                    .iter()
                    .map(|result| (result.route_identity.as_str(), result))
                    .collect::<BTreeMap<_, _>>()
            });
        let mut certified_routes = BTreeMap::new();
        let mut stale_automatic_pauses = BTreeSet::new();
        for admission in admissions {
            let terminal_failed = state.watch_uncertain_through.is_some()
                || !publication_ready
                || attempt
                    .as_ref()
                    .is_none_or(|attempt| attempt.state != SourceBackedRefreshState::Published);
            if terminal_failed {
                let blocked = attempt
                    .as_ref()
                    .and_then(|attempt| attempt.failure_outcome.as_ref())
                    .is_some_and(|outcome| outcome.blocked_routes.contains(admission.route()));
                if blocked {
                    state.route_retry_intents.remove(admission.route());
                    let actually_paused = state.dirty_routes.permanent_failure(&admission);
                    let automatic_pause = attempt.as_ref().is_some_and(|attempt| {
                        attempt.failure_outcome.as_ref().is_some_and(
                            SourceBackedRefreshFailureOutcome::is_automatic_retry_eligible,
                        ) && attempt
                            .automatic_retry_checkpoints
                            .get(admission.route())
                            .is_some_and(SourceBackedAutomaticRetryCheckpoint::is_paused)
                    });
                    if automatic_pause && !actually_paused {
                        stale_automatic_pauses.insert(admission.route().clone());
                    }
                } else {
                    if let Some(intent) = selected_retry_intent.as_ref() {
                        state
                            .route_retry_intents
                            .insert(admission.route().clone(), intent.clone());
                    }
                    state.dirty_routes.retryable_failure(&admission, now_ms);
                    state
                        .routes_requiring_exhaustive_reconciliation
                        .insert(admission.route().clone());
                }
                continue;
            }
            let Some(result) = route_results
                .as_ref()
                .and_then(|results| results.get(admission.route().as_str()))
                .copied()
            else {
                state.dirty_routes.retryable_failure(&admission, now_ms);
                state
                    .routes_requiring_exhaustive_reconciliation
                    .insert(admission.route().clone());
                continue;
            };
            if let Some(retryable) = source_backed_route_retry_disposition(result) {
                if retryable {
                    if let Some(intent) = selected_retry_intent.as_ref() {
                        state
                            .route_retry_intents
                            .insert(admission.route().clone(), intent.clone());
                    }
                    state.dirty_routes.retryable_failure(&admission, now_ms);
                    state
                        .routes_requiring_exhaustive_reconciliation
                        .insert(admission.route().clone());
                } else {
                    state.route_retry_intents.remove(admission.route());
                    state.dirty_routes.permanent_failure(&admission);
                }
                continue;
            }
            if result.outcome.is_success() {
                let verified_boundary = attempt.as_ref().and_then(|attempt| {
                    let observation = attempt.route_observations.get(admission.route())?;
                    let admitted_watermark = predecessor_event_watermarks
                        .get(admission.route())
                        .copied()?;
                    let published_generation = attempt.published_generation.as_deref()?;
                    let covered_through =
                        post_publication_fence.map_or(admitted_watermark, |fence| {
                            fence.certified_boundary(
                                admission.route(),
                                admitted_watermark,
                                observation,
                            )
                        });
                    VerifiedSourceRefreshRouteBoundary::new(
                        request_id,
                        published_generation,
                        admission.route(),
                        covered_through,
                        observation,
                    )
                    .map(|boundary| (boundary, observation.clone()))
                });
                let acknowledged = match verified_boundary.as_ref() {
                    Some((boundary, _)) => state
                        .dirty_routes
                        .acknowledge_generation_coverage(&admission, boundary),
                    None => state.dirty_routes.acknowledge(&admission),
                };
                if acknowledged {
                    state.route_retry_intents.remove(admission.route());
                    if attempt.as_ref().is_some_and(|attempt| {
                        attempt.reconciliation_demand
                            == SourceBackedReconciliationDemand::Exhaustive
                    }) {
                        state
                            .hermes_routes_requiring_exhaustive_recovery
                            .remove(admission.route());
                        state
                            .routes_requiring_exhaustive_reconciliation
                            .remove(admission.route());
                    }
                    if let Some((boundary, observation)) = verified_boundary {
                        certified_routes.insert(
                            admission.route().clone(),
                            SourceBackedRefreshRouteCoverageCertificate {
                                observation,
                                admitted_watermark: boundary.covered_through(),
                            },
                        );
                    }
                }
            } else {
                if let Some(intent) = selected_retry_intent.as_ref() {
                    state
                        .route_retry_intents
                        .insert(admission.route().clone(), intent.clone());
                }
                state.dirty_routes.retryable_failure(&admission, now_ms);
                state
                    .routes_requiring_exhaustive_reconciliation
                    .insert(admission.route().clone());
            }
        }
        if !stale_automatic_pauses.is_empty() {
            for route in &stale_automatic_pauses {
                state.automatic_retry_checkpoints.remove(route);
            }
            let checkpoints = state.automatic_retry_checkpoints.clone();
            if let Some(attempt) = find_attempt_mut(state, request_id) {
                if let Some(outcome) = attempt.failure_outcome.as_mut() {
                    outcome.rearm_automatic_retry_routes(&stale_automatic_pauses);
                }
                attempt.automatic_retry_checkpoints = checkpoints;
            }
        }
        if attempt
            .as_ref()
            .is_some_and(|attempt| attempt.state == SourceBackedRefreshState::Failed)
        {
            if attempt
                .as_ref()
                .and_then(|attempt| attempt.failure_outcome.as_ref())
                .is_some_and(|outcome| !outcome.affected_routes.is_empty())
                && state.pending_scheduler_retry_root_id.as_deref() == Some(request_id)
            {
                state.pending_scheduler_retry_root_id = None;
            }
            return RouteAdmissionFinish {
                coverage_certificate: None,
                durable_request_id: request_id.to_owned(),
            };
        }
        let coverage_certificate = attempt
            .filter(|attempt| {
                publication_ready && attempt.state == SourceBackedRefreshState::Published
            })
            .and_then(|attempt| {
                Some(SourceBackedRefreshCoverageCertificate {
                    request_id: request_id.to_owned(),
                    published_generation: attempt.published_generation.clone()?,
                    routes: certified_routes,
                })
            });
        RouteAdmissionFinish {
            coverage_certificate,
            durable_request_id: request_id.to_owned(),
        }
    }

    pub(super) fn restore_route_dispositions_locked(
        state: &mut CoreRefreshEngineState,
        retryable_routes: &BTreeSet<SourceRouteIdentity>,
        blocked_routes: &BTreeSet<SourceRouteIdentity>,
        retry_intent: Option<&RefreshIntent>,
    ) {
        let routes = retryable_routes
            .union(blocked_routes)
            .cloned()
            .collect::<BTreeSet<_>>();
        if routes.is_empty() {
            return;
        }
        let now_ms = source_route_ledger_now_ms();
        let watermark = state.dirty_routes.seed_watermark();
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
        state.dirty_routes.block_exact_routes(blocked_routes.iter());
        if let Some(intent @ RefreshIntent::SelectedImport(_)) = retry_intent {
            let intent = Arc::new(intent.clone());
            for route in retryable_routes {
                state
                    .route_retry_intents
                    .insert(route.clone(), Arc::clone(&intent));
            }
        }
        for route in blocked_routes {
            state.route_retry_intents.remove(route);
        }
    }

    pub(super) fn seed_rearmed_automatic_retry_routes_locked(
        state: &mut CoreRefreshEngineState,
        routes: &BTreeSet<SourceRouteIdentity>,
    ) {
        if routes.is_empty() {
            return;
        }
        let watermark = state.dirty_routes.seed_watermark();
        for route in routes {
            state
                .route_event_watermarks
                .entry(route.clone())
                .and_modify(|current| *current = (*current).max(watermark))
                .or_insert(watermark);
        }
        state.dirty_routes.seed_exact_routes(
            routes.iter().cloned(),
            watermark,
            source_route_ledger_now_ms().saturating_sub(1_000),
        );
    }
}

fn reconcile_due_automatic_retry_routes(
    state: &mut CoreRefreshEngineState,
    routes: &mut BTreeSet<SourceRouteIdentity>,
    observations: Option<&BTreeMap<SourceRouteIdentity, Option<String>>>,
) -> bool {
    let mut rearmed = Vec::new();
    let mut still_paused = Vec::new();
    let checkpoint_routes = state
        .automatic_retry_checkpoints
        .keys()
        .filter(|route| {
            routes.contains(*route)
                || observations.is_some_and(|observations| observations.contains_key(*route))
        })
        .cloned()
        .collect::<Vec<_>>();
    for route in &checkpoint_routes {
        let checkpoint = state
            .automatic_retry_checkpoints
            .get(route)
            .expect("automatic retry checkpoint route");
        let build_changed = checkpoint.build_version != SOURCE_REFRESH_BUILD_VERSION;
        let current_observation = observations
            .and_then(|observations| observations.get(route))
            .and_then(Option::as_deref);
        let changed_observation =
            current_observation != Some(checkpoint.source_observation.as_str());
        if build_changed || changed_observation {
            rearmed.push(route.clone());
        } else if checkpoint.is_paused() {
            still_paused.push(route.clone());
        }
    }
    for route in rearmed {
        state.automatic_retry_checkpoints.remove(&route);
    }
    let paused_routes_skipped = !still_paused.is_empty();
    state.dirty_routes.block_exact_routes(still_paused.iter());
    for route in still_paused {
        routes.remove(&route);
    }
    paused_routes_skipped
}
