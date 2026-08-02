use super::*;

impl CoreRefreshEngine {
    pub(in crate::semantic) fn run_next(&self, data_root: &Path) -> Option<SourceBackedRefreshRun> {
        self.run_next_with_verified_index_opener(data_root, |index_root| {
            Ok(Arc::new(open_verified_index(index_root)?))
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
        let request_id_cell = RefCell::new(None::<String>);
        let run = self.run_next_with_terminal_success(
            |request_id, coordinator| {
                request_id_cell.replace(Some(request_id.to_owned()));
                let requested_catalog =
                    coordinator.freeze_requested_explicit_source_catalog(data_root, request_id)?;
                let refresh_scope = coordinator
                    .refresh_scope(request_id)
                    .ok_or_else(|| anyhow!("source refresh request `{request_id}` is unknown"))?;
                let covered_route_ids =
                    coordinator.admit_refresh_scope(request_id, &refresh_scope)?;
                coordinator.persist_job_status(data_root, request_id)?;
                let publication = execute_source_backed_refresh(
                    executor.as_ref(),
                    data_root,
                    request_id,
                    coordinator,
                    SourceBackedRefreshPlan {
                        explicit_source_catalog: Some(&requested_catalog),
                        scope: refresh_scope,
                        covered_route_ids,
                    },
                )?;
                let probe_started = StdInstant::now();
                publication_probe_attempted.set(true);
                let pin = open_verified(&source_backed_index_root(data_root))
                    .context("verify Core generation after publication")?;
                let verification = verify_source_backed_publication(&publication, &pin);
                coordinator.set_publication_probe_timing(
                    request_id,
                    nonzero_duration_micros(probe_started.elapsed()),
                );
                verification?;
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
                CoreRefreshTerminalSuccess::bind(receipt, pin)
                    .context("bind exact Core publication receipt and generation authority")
            },
            |_| {
                request_id_cell
                    .borrow()
                    .as_deref()
                    .ok_or_else(|| anyhow!("published source refresh has no request ID"))
                    .and_then(|request_id| self.persist_job_status(data_root, request_id))
            },
            |_| Ok(()),
        )?;
        let publication_ready = !run.failed;
        if let Some(request_id) = run.job.get("request_id").and_then(Value::as_str) {
            self.finish_route_admissions(request_id, publication_ready);
        }
        Some(run)
    }

    fn set_publication_probe_timing(&self, request_id: &str, duration_us: u64) {
        let mut state = self.lock_state();
        if let Some(attempt) = find_attempt_mut(&mut state, request_id) {
            attempt.publication_probe_us = duration_us;
        }
    }

    pub(in crate::semantic) fn enqueue_periodic(&self, data_root: &Path) -> Result<Value> {
        let observed_generation = self.observed_published_generation(data_root)?;
        let catalog = load_explicit_source_catalog_authority(data_root)?;
        self.enqueue_with_catalog_metadata(
            observed_generation,
            SourceRefreshRuntimeMetadata::periodic(),
            Some(catalog),
            SourceBackedRefreshScope::All,
            SourceRefreshAdmissionRequirement::AttachEquivalent,
        )
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
    ) -> Result<Value> {
        let mut state = self.lock_state();
        let is_manual_all = metadata.operation == SourceBackedRefreshOperation::Import
            && requested_catalog.is_some()
            && refresh_scope == SourceBackedRefreshScope::All;
        let mut continuation_predecessor = None;
        if let Some(active_request_id) = state.active_request_id.clone() {
            if let Some(active) = find_attempt_mut(&mut state, &active_request_id) {
                if active.state.is_active() && !admission.requires_successor(active.state) {
                    if let Some(requested_catalog) = requested_catalog.as_ref() {
                        let upgrades_queued_automatic =
                            active.requested_explicit_source_catalog.is_none()
                                && active.state == SourceBackedRefreshState::Queued;
                        if upgrades_queued_automatic {
                            active.requested_explicit_source_catalog =
                                Some(requested_catalog.clone());
                        }
                    }
                    let same_catalog = active.requested_explicit_source_catalog.as_ref()
                        == requested_catalog.as_ref();
                    let automatic_exact = active.trigger == "periodic"
                        && active.trigger_provenance == "daemon_scheduler"
                        && matches!(
                            &active.refresh_scope,
                            SourceBackedRefreshScope::Exact(routes) if routes.len() == 1
                        );
                    if is_manual_all && same_catalog && automatic_exact {
                        if active.state == SourceBackedRefreshState::Queued {
                            active.refresh_scope = SourceBackedRefreshScope::All;
                            return Ok(coalesce_attempt(active, metadata));
                        }
                        continuation_predecessor = Some(active.request_id.clone());
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

        if admission == SourceRefreshAdmissionRequirement::FreshAfterAdmittedSnapshot
            || requested_catalog.is_some()
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

        let active_pending_requests = active_attempt_count(&state);
        if active_pending_requests >= SOURCE_REFRESH_ACTIVE_PENDING_LIMIT {
            return Err(SourceBackedRefreshQueueFull {
                active_pending_requests,
            }
            .into());
        }

        let attempt = new_refresh_attempt(
            observed_generation,
            metadata,
            requested_catalog,
            refresh_scope,
        );
        let response = attempt.to_json();
        let request_id = attempt.request_id.clone();
        if state
            .active_request_id
            .as_deref()
            .and_then(|request_id| find_attempt(&state, request_id))
            .is_some_and(|attempt| attempt.state.is_active())
        {
            state.pending_request_ids.push_back(request_id.clone());
        } else {
            state.active_request_id = Some(request_id.clone());
        }
        if let Some(predecessor_request_id) = continuation_predecessor {
            state.manual_all_continuations.insert(
                request_id,
                ManualAllContinuation::new(predecessor_request_id),
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
    ) -> Result<BTreeSet<SourceRouteIdentity>> {
        let now_ms = source_route_ledger_now_ms();
        let mut state = self.lock_state();
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
                .covered_route_ids
                .intersection(&known_route_ids)
                .cloned()
                .collect::<BTreeSet<_>>();
            if retained != continuation.covered_route_ids {
                continuation.covered_route_ids = retained;
                if continuation.covered_route_ids.is_empty() {
                    continuation.covered_scanned_routes = 0;
                    continuation.covered_removed_source_count = 0;
                    continuation.covered_timings = SourceBackedRefreshTimings::default();
                }
            }
            continuation.covered_route_ids.clone()
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
                        continuation.covered_route_ids.clear();
                        continuation.covered_scanned_routes = 0;
                        continuation.covered_removed_source_count = 0;
                        continuation.covered_timings = SourceBackedRefreshTimings::default();
                    }
                }
                admissions
            }
            SourceBackedRefreshScope::Exact(routes) => {
                if routes.len() != 1 {
                    bail!("daemon exact source refresh must admit exactly one route");
                }
                let route = routes
                    .iter()
                    .next()
                    .ok_or_else(|| anyhow!("daemon exact source refresh has no route"))?;
                vec![state
                    .dirty_routes
                    .admit_exact(route, now_ms)
                    .ok_or_else(|| {
                        anyhow!(
                            "exact source route {} is no longer due for admission",
                            route.as_str()
                        )
                    })?]
            }
        };
        state
            .route_admissions
            .insert(request_id.to_owned(), admissions);
        Ok(covered_route_ids)
    }

    fn freeze_requested_explicit_source_catalog(
        &self,
        data_root: &Path,
        request_id: &str,
    ) -> Result<ExplicitSourceCatalogAuthority> {
        if let Some(catalog) = self.requested_explicit_source_catalog(request_id) {
            return Ok(catalog);
        }
        let catalog = load_explicit_source_catalog_authority(data_root)?;
        let mut state = self.lock_state();
        let attempt = find_attempt_mut(&mut state, request_id)
            .ok_or_else(|| anyhow!("source refresh request `{request_id}` is unknown"))?;
        if let Some(existing) = attempt.requested_explicit_source_catalog.as_ref() {
            return Ok(existing.clone());
        }
        attempt.requested_explicit_source_catalog = Some(catalog.clone());
        Ok(catalog)
    }

    pub(super) fn job_status(&self, request_id: &str) -> Option<Value> {
        let state = self.lock_state();
        find_attempt(&state, request_id).map(SourceBackedRefreshAttempt::job_json)
    }

    pub(super) fn persist_job_status(&self, data_root: &Path, request_id: &str) -> Result<()> {
        let state = self.lock_state();
        let job = find_attempt(&state, request_id)
            .ok_or_else(|| anyhow!("source refresh request `{request_id}` is unknown"))?
            .job_json();
        // Keep the state lock through publication so an admission snapshot
        // cannot overwrite a later terminal snapshot during waiter races.
        write_daemon_job_status(&daemon_source_backed_refresh_job_path(data_root), &job)
    }

    pub(in super::super) fn set_progress(
        &self,
        request_id: &str,
        update: SourceBackedRefreshProgressUpdate,
    ) -> Option<Value> {
        let mut state = self.lock_state();
        let attempt = find_attempt_mut(&mut state, request_id)?;
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
        };
        Some(attempt.job_json())
    }
}
