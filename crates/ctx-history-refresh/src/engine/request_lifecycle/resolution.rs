use super::*;

impl CoreRefreshEngine {
    pub fn run_next(&self, data_root: &Path) -> Option<SourceBackedRefreshRun> {
        if self
            .lock_state()
            .pending_terminal_persistence
            .as_ref()
            .is_some_and(PendingTerminalPersistence::finalization_only)
        {
            return self.run_next_with_verified_index_opener(data_root, |index_root| {
                Ok(Arc::new(open_verified_index(index_root)?))
            });
        }
        match self.resolve_active_pending_admission(data_root) {
            Ok(Some(run)) => return Some(run),
            Ok(None) => {}
            Err(error) => return self.admission_persistence_retry_run(error),
        }
        if self.active_request_admission_pending() {
            return None;
        }
        match self.requeue_stale_provider_root_admission(data_root) {
            Ok(true) => match self.resolve_active_pending_admission(data_root) {
                Ok(Some(run)) => return Some(run),
                Ok(None) => {}
                Err(error) => return self.admission_persistence_retry_run(error),
            },
            Ok(false) => {}
            Err(error) => return self.admission_persistence_retry_run(error),
        }
        if self.active_request_admission_pending() {
            return None;
        }
        self.run_next_with_verified_index_opener(data_root, |index_root| {
            Ok(Arc::new(open_verified_index(index_root)?))
        })
    }

    pub fn run_next_with_verified_index_opener<Open>(
        &self,
        data_root: &Path,
        open_verified: Open,
    ) -> Option<SourceBackedRefreshRun>
    where
        Open: FnOnce(&Path) -> Result<Arc<VerifiedIndex>>,
    {
        self.run_next_with_verified_index_opener_and_coverage_fence(
            data_root,
            open_verified,
            |request_id| self.post_publication_route_coverage_fence(request_id),
        )
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn run_next_with_coverage_fence_for_test<Sample>(
        &self,
        data_root: &Path,
        sample: Sample,
    ) -> Option<SourceBackedRefreshRun>
    where
        Sample: FnOnce(
            Option<&ExplicitSourceCatalogAuthority>,
            &BTreeSet<SourceRouteIdentity>,
        ) -> Result<BTreeMap<SourceRouteIdentity, Option<String>>>,
    {
        self.run_next_with_verified_index_opener_and_coverage_fence(
            data_root,
            |index_root| Ok(Arc::new(open_verified_index(index_root)?)),
            |request_id| {
                self.regular_post_publication_route_coverage_fence_with(request_id, sample)
            },
        )
    }

    fn run_next_with_verified_index_opener_and_coverage_fence<Open, Coverage>(
        &self,
        data_root: &Path,
        open_verified: Open,
        coverage_fence: Coverage,
    ) -> Option<SourceBackedRefreshRun>
    where
        Open: FnOnce(&Path) -> Result<Arc<VerifiedIndex>>,
        Coverage: FnOnce(&str) -> PostPublicationRouteCoverageFence,
    {
        let executor = Arc::clone(&self.executor);
        let verified_index = RefCell::new(None::<Arc<VerifiedIndex>>);
        let publication_probe_attempted = Cell::new(false);
        let mut run = self.run_next_with_terminal_success(
            |request_id, coordinator| {
                let intent = coordinator
                    .refresh_intent(request_id)
                    .ok_or_else(|| anyhow!("source refresh request `{request_id}` is unknown"))?;
                let operation = intent.operation();
                let reconciliation_demand = coordinator
                    .reconciliation_demand(request_id)
                    .ok_or_else(|| anyhow!("source refresh request `{request_id}` is unknown"))?;
                let admitted = coordinator.admit_refresh(request_id)?;
                let refresh_scope = admitted.publication_scope();
                coordinator.persist_job_status(data_root, request_id)?;
                let mut publication = execute_source_backed_refresh(
                    executor.as_ref(),
                    data_root,
                    request_id,
                    coordinator,
                    &intent,
                    reconciliation_demand,
                    admitted,
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
                let verified =
                    open_published_generation(data_root, self.journal.as_ref())?.map(Arc::new);
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
        if run.route_finalization_performed {
            return Some(run);
        }
        let publication_ready = !run.failed && !run.terminal_persistence_pending;
        if let Some(request_id) = run.job.get("request_id").and_then(Value::as_str) {
            if !run.terminal_persistence_pending {
                let post_publication_fence = publication_ready.then(|| coverage_fence(request_id));
                match self.finish_route_admissions_and_persist(
                    data_root,
                    request_id,
                    publication_ready,
                    post_publication_fence.as_ref(),
                ) {
                    Ok((finish, finalized_job)) => {
                        run.job = finalized_job;
                        run.coverage_certificate = finish.coverage_certificate;
                    }
                    Err(_) => {
                        run.did_work = false;
                        run.terminal_persistence_pending = true;
                        let state = self.lock_state();
                        if let Some(pending) = state.pending_terminal_persistence.as_ref() {
                            run.job =
                                job_with_queued_successors(&state, pending.terminal_job.clone());
                        }
                    }
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
        request_id: &str,
    ) -> PostPublicationRouteCoverageFence {
        let scoped_catalog = {
            let state = self.lock_state();
            find_attempt(&state, request_id)
                .and_then(|attempt| attempt.admitted_authority.as_ref())
                .map(|authority| authority.discovery().watch_catalog().clone())
        };
        if let Some(catalog) = scoped_catalog {
            return self.regular_post_publication_route_coverage_fence_with(
                request_id,
                move |_authority, routes| {
                    Ok(source_backed_requested_route_observations(&catalog, routes))
                },
            );
        }
        PostPublicationRouteCoverageFence::fail_closed()
    }

    fn regular_post_publication_route_coverage_fence_with<Sample>(
        &self,
        request_id: &str,
        sample: Sample,
    ) -> PostPublicationRouteCoverageFence
    where
        Sample: FnOnce(
            Option<&ExplicitSourceCatalogAuthority>,
            &BTreeSet<SourceRouteIdentity>,
        ) -> Result<BTreeMap<SourceRouteIdentity, Option<String>>>,
    {
        // A verified publication already covers each route through its exact
        // admission watermark. Provider sampling is needed only to prove that
        // watcher events delivered during capture did not change its content.
        let routes = {
            let state = self.lock_state();
            let admitted = state.route_admission_watermarks.get(request_id);
            find_attempt(&state, request_id).map_or_else(BTreeSet::new, |attempt| {
                attempt
                    .route_observations
                    .keys()
                    .filter(|route| {
                        admitted
                            .and_then(|watermarks| watermarks.get(*route))
                            .zip(state.route_event_watermarks.get(*route))
                            .is_some_and(|(admitted, current)| current > admitted)
                    })
                    .cloned()
                    .collect()
            })
        };
        if routes.is_empty() {
            return PostPublicationRouteCoverageFence::fail_closed();
        }
        self.post_publication_route_coverage_fence_with(request_id, routes, sample)
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
            &BTreeSet<SourceRouteIdentity>,
        ) -> Result<BTreeMap<SourceRouteIdentity, Option<String>>>,
    {
        if routes.is_empty() {
            return PostPublicationRouteCoverageFence::fail_closed();
        }
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
                attempt.and_then(|attempt| attempt.requested_explicit_source_catalog().cloned());
            (seen_watermarks, requested_catalog)
        };
        let mut sampled = sample(requested_catalog.as_ref(), &routes).unwrap_or_default();
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

    #[cfg(any(test, feature = "test-support"))]
    pub fn set_route_observations_for_test(
        &self,
        request_id: &str,
        observations: BTreeMap<SourceRouteIdentity, String>,
    ) {
        self.set_route_observations(request_id, observations);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn regular_post_publication_route_coverage_fence_for_test<Sample>(
        &self,
        request_id: &str,
        sample: Sample,
    ) -> PostPublicationRouteCoverageFence
    where
        Sample: FnOnce(
            &BTreeSet<SourceRouteIdentity>,
        ) -> Result<BTreeMap<SourceRouteIdentity, Option<String>>>,
    {
        self.regular_post_publication_route_coverage_fence_with(request_id, |_catalog, routes| {
            sample(routes)
        })
    }
}
