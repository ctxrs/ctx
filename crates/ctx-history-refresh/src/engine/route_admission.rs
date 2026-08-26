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
        let request_id = {
            let mut state = self.lock_state();
            if durable_queue_entry_count(&state) != 0 {
                return Ok(false);
            }
            let routes = state
                .dirty_routes
                .due_routes(now_ms, SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT);
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
            let refresh_scope = if automatic_split_pending {
                // A released collapsed identity is still active. Exact watch
                // work cannot establish the complete role cohort required to
                // bridge or retire it, so retain the event as the trigger but
                // admit a single all-route exhaustive migration attempt.
                SourceBackedRefreshScope::All
            } else if retry_intent.is_none() && cold_all && observed_generation.is_none() {
                // A cold generation has no retained routes to carry. Publish
                // the complete startup inventory atomically instead of one
                // transient partial generation per initially dirty route.
                SourceBackedRefreshScope::All
            } else {
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
        for admission in admissions {
            let terminal_failed = !publication_ready
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
                    state.dirty_routes.permanent_failure(&admission);
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
}
