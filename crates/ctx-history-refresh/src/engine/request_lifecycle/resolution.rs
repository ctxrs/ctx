use super::*;

impl CoreRefreshEngine {
    pub fn run_next(&self, data_root: &Path) -> Option<SourceBackedRefreshRun> {
        match self.resolve_active_pending_admission(data_root) {
            Ok(Some(run)) => return Some(run),
            Ok(None) => {}
            Err(error) => return self.admission_persistence_retry_run(error),
        }
        if self.active_request_admission_pending() {
            return None;
        }
        if let Some(run) =
            self.resolve_fully_covered_continuation_with(data_root, |catalog, routes| {
                let discovery = self.runtime.discovery_context(data_root)?;
                source_backed_requested_route_observation_fence(
                    &discovery,
                    self.journal.as_ref(),
                    data_root,
                    catalog,
                    routes,
                )
            })
        {
            return Some(run);
        }
        self.run_next_with_verified_index_opener(data_root, |index_root| {
            Ok(Arc::new(open_verified_index(index_root)?))
        })
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn run_next_with_post_publication_sampler_for_test<Sample>(
        &self,
        data_root: &Path,
        sample: Sample,
    ) -> Option<SourceBackedRefreshRun>
    where
        Sample: FnOnce(
            Option<&ExplicitSourceCatalogAuthority>,
        ) -> Result<BTreeMap<SourceRouteIdentity, Option<String>>>,
    {
        if let Some(run) = self
            .resolve_fully_covered_continuation_with(data_root, |catalog, _routes| sample(catalog))
        {
            return Some(run);
        }
        self.run_next_with_verified_index_opener(data_root, |index_root| {
            Ok(Arc::new(open_verified_index(index_root)?))
        })
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn resolve_fully_covered_continuation_for_test<Sample>(
        &self,
        data_root: &Path,
        sample: Sample,
    ) -> Option<SourceBackedRefreshRun>
    where
        Sample: FnOnce(
            Option<&ExplicitSourceCatalogAuthority>,
        ) -> Result<BTreeMap<SourceRouteIdentity, Option<String>>>,
    {
        self.resolve_fully_covered_continuation_with(data_root, |catalog, _routes| sample(catalog))
    }

    fn resolve_fully_covered_continuation_with<Sample>(
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
            let routes = continuation
                .covered_route_results
                .keys()
                .filter(|route| !continuation.ledger_eligible_routes.contains(*route))
                .cloned()
                .collect();
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
        let publication_metadata = state
            .pinned_core_publication
            .as_ref()
            .filter(|authority| authority.generation_id() == published_generation)
            .and_then(|authority| {
                SourceBackedPublicationMetadata::decode(authority.verified_index_ref()).ok()
            })
            .filter(|metadata| metadata.request_id == continuation.predecessor_request_id);
        let invalid_routes = continuation
            .covered_route_results
            .keys()
            .filter(|route| {
                if continuation.ledger_eligible_routes.contains(*route) {
                    return continuation.admission_event_watermarks.get(*route)
                        != state.route_event_watermarks.get(*route);
                }
                let admitted_observation = continuation
                    .admission_route_observations
                    .get(*route)
                    .and_then(Option::as_deref);
                admitted_observation.is_none_or(|admitted| {
                    publication_metadata
                        .as_ref()
                        .and_then(|metadata| metadata.route_observations.get(*route))
                        .is_none_or(|published| published != admitted)
                        || !post_publication_fence.exactly_matches(route, admitted)
                })
            })
            .cloned()
            .collect::<BTreeSet<_>>();
        if !invalid_routes.is_empty() {
            let continuation = state.manual_all_continuations.get_mut(&request_id)?;
            for route in invalid_routes {
                continuation.invalidate_route(&route);
            }
            return None;
        }
        let coverage_certificate = publication_metadata.map(|metadata| {
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
        // A fresh logical demand can be admitted while the provider sample
        // above runs without the state lock. Its exact predecessor is this
        // logical request, so release that durable fence with this terminal
        // image. Coverage is intentionally not inherited transitively here:
        // the later snapshot executes unless it establishes its own proof.
        let successor_fence_snapshots = state
            .manual_all_continuations
            .iter_mut()
            .filter_map(|(successor_id, successor)| {
                (successor.predecessor_request_id == request_id && !successor.predecessor_finished)
                    .then(|| {
                        let snapshot = successor.clone();
                        successor.predecessor_finished = true;
                        (successor_id.clone(), snapshot)
                    })
            })
            .collect::<BTreeMap<_, _>>();
        let terminal_job = durable_job_json(&state, &request_id)?;
        if let Err(error) = self.write_status(data_root, &terminal_job) {
            for (successor_id, snapshot) in successor_fence_snapshots {
                state
                    .manual_all_continuations
                    .insert(successor_id, snapshot);
            }
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
            |request_id| self.post_publication_route_coverage_fence(data_root, request_id),
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
        let publication_ready = !run.failed && !run.terminal_persistence_pending;
        if let Some(request_id) = run.job.get("request_id").and_then(Value::as_str) {
            if !run.terminal_persistence_pending {
                let post_publication_fence = publication_ready.then(|| coverage_fence(request_id));
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
        self.regular_post_publication_route_coverage_fence_with(request_id, |catalog, routes| {
            let discovery = self.runtime.discovery_context(data_root)?;
            source_backed_requested_route_observation_fence(
                &discovery,
                self.journal.as_ref(),
                data_root,
                catalog,
                routes,
            )
        })
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
                attempt.and_then(|attempt| attempt.requested_explicit_source_catalog.clone());
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
