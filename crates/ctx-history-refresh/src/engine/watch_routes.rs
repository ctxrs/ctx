use super::*;

impl CoreRefreshEngine {
    pub fn initialize_watch_route_authority(
        &self,
        routes: impl IntoIterator<Item = SourceRouteIdentity>,
    ) {
        let routes = routes.into_iter().collect::<BTreeSet<_>>();
        let mut state = self.lock_state();
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
        state
            .route_worksets
            .retain(|route, _| routes.contains(route));
        state.known_route_ids = routes;
        state.watch_routes_initialized = true;
    }

    /// Atomically installs the watcher snapshot used to authorize exact
    /// member work. Replacing a snapshot never carries member paths from the
    /// preceding registration authority into the new catalog.
    pub fn install_watch_catalog(&self, catalog: SourceBackedWatchCatalog) {
        let routes = catalog.route_ids().cloned().collect::<BTreeSet<_>>();
        let mut state = self.lock_state();
        let newly_uncertain = state.watch_uncertain_through.map(|watermark| {
            (
                routes
                    .difference(&state.known_route_ids)
                    .cloned()
                    .collect::<Vec<_>>(),
                watermark,
            )
        });
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
        state.known_route_ids = routes;
        state.watch_catalog = Some(catalog);
        state.watch_routes_initialized = true;
        if let Some((new_routes, watermark)) = newly_uncertain {
            state
                .routes_requiring_exhaustive_reconciliation
                .extend(new_routes.iter().cloned());
            for route in &new_routes {
                state
                    .route_event_watermarks
                    .insert(route.clone(), watermark);
            }
            state.dirty_routes.seed_exact_routes(
                new_routes,
                watermark,
                source_route_ledger_now_ms(),
            );
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn reconcile_watch_routes(
        &self,
        routes: impl IntoIterator<Item = SourceRouteIdentity>,
        watermark: EventWatermark,
        observed_at_ms: u64,
    ) {
        let routes = routes.into_iter().collect::<BTreeSet<_>>();
        self.initialize_watch_route_authority(routes.iter().cloned());
        self.schedule_route_reconciliation(routes, watermark, observed_at_ms, false);
    }

    pub fn schedule_startup_route_reconciliation(
        &self,
        routes: impl IntoIterator<Item = SourceRouteIdentity>,
        watermark: EventWatermark,
        observed_at_ms: u64,
    ) {
        self.schedule_route_reconciliation(routes, watermark, observed_at_ms, true);
    }

    fn schedule_route_reconciliation(
        &self,
        routes: impl IntoIterator<Item = SourceRouteIdentity>,
        watermark: EventWatermark,
        observed_at_ms: u64,
        requires_exhaustive_reconciliation: bool,
    ) {
        let mut state = self.lock_state();
        let routes = routes
            .into_iter()
            .filter(|route| state.known_route_ids.contains(route))
            .collect::<Vec<_>>();
        if requires_exhaustive_reconciliation {
            state
                .routes_requiring_exhaustive_reconciliation
                .extend(routes.iter().cloned());
        }
        for route in &routes {
            state
                .route_event_watermarks
                .entry(route.clone())
                .and_modify(|current| *current = (*current).max(watermark))
                .or_insert(watermark);
        }
        state
            .dirty_routes
            .seed_exact_routes(routes, watermark, observed_at_ms);
    }

    /// Performs the bounded provider-neutral startup preflight. The watcher is
    /// already active when this runs. Only a generation-bound exact
    /// `Unchanged` observation stays clean; every other route enters the
    /// normal fail-closed refresh path.
    pub fn schedule_startup_route_observation(
        &self,
        catalog: &SourceBackedWatchCatalog,
        watermark: EventWatermark,
        observed_at_ms: u64,
    ) {
        self.schedule_startup_route_observation_with_budget(
            catalog,
            watermark,
            observed_at_ms,
            SOURCE_REFRESH_STARTUP_OBSERVATION_BUDGET,
        );
    }

    fn schedule_startup_route_observation_with_budget(
        &self,
        catalog: &SourceBackedWatchCatalog,
        watermark: EventWatermark,
        observed_at_ms: u64,
        budget: StdDuration,
    ) {
        let authority = self.pinned_core_publication();
        let metadata = authority.as_deref().and_then(|authority| {
            SourceBackedPublicationMetadata::decode(authority.verified_index_ref()).ok()
        });
        let missing_routes = authority
            .as_deref()
            .map(|authority| {
                authority
                    .verified_index_ref()
                    .manifest()
                    .source_routes()
                    .iter()
                    .filter(|snapshot| snapshot.missing_state().is_some())
                    .map(|snapshot| snapshot.route_identity().clone())
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        let mut dirty = startup_routes_requiring_refresh(
            catalog,
            metadata
                .as_ref()
                .map(|metadata| &metadata.route_observations),
            &missing_routes,
            budget,
        );
        let route_controls = metadata
            .as_ref()
            .map(|metadata| &metadata.route_controls)
            .cloned()
            .unwrap_or_default();
        let controlled_routes = catalog
            .route_ids()
            .filter(|route| catalog.route_control_expectation(route).is_some())
            .cloned()
            .collect::<BTreeSet<_>>();
        let hermes_control_recovery = hermes_routes_requiring_control_recovery(
            catalog,
            &route_controls,
            i64::try_from(observed_at_ms).unwrap_or(i64::MAX),
        );
        dirty.extend(hermes_control_recovery.iter().cloned());
        dirty.sort();
        dirty.dedup();
        let mut state = self.lock_state();
        state
            .hermes_routes_requiring_exhaustive_recovery
            .retain(|route| !controlled_routes.contains(route));
        state
            .hermes_routes_requiring_exhaustive_recovery
            .extend(hermes_control_recovery.iter().cloned());
        let dirty = dirty
            .into_iter()
            .filter(|route| state.known_route_ids.contains(route))
            .collect::<Vec<_>>();
        for route in &dirty {
            state
                .route_event_watermarks
                .entry(route.clone())
                .and_modify(|current| *current = (*current).max(watermark))
                .or_insert(watermark);
        }
        state
            .dirty_routes
            .seed_exact_routes(dirty, watermark, observed_at_ms);
    }

    pub fn record_watch_routes(
        &self,
        routes: impl IntoIterator<Item = (SourceRouteIdentity, EventWatermark)>,
        observed_at_ms: u64,
    ) {
        self.record_watch_routes_with_members_mode(routes, BTreeMap::new(), observed_at_ms, false);
    }

    pub fn record_watch_routes_with_members(
        &self,
        routes: impl IntoIterator<Item = (SourceRouteIdentity, EventWatermark)>,
        members: BTreeMap<SourceRouteIdentity, BTreeSet<PathBuf>>,
        observed_at_ms: u64,
    ) {
        self.record_watch_routes_with_members_mode(routes, members, observed_at_ms, true);
    }

    pub fn record_watch_routes_requiring_exhaustive_reconciliation(
        &self,
        routes: impl IntoIterator<Item = (SourceRouteIdentity, EventWatermark)>,
        observed_at_ms: u64,
    ) {
        self.record_watch_routes_with_members_mode(routes, BTreeMap::new(), observed_at_ms, true);
    }

    fn record_watch_routes_with_members_mode(
        &self,
        routes: impl IntoIterator<Item = (SourceRouteIdentity, EventWatermark)>,
        mut members: BTreeMap<SourceRouteIdentity, BTreeSet<PathBuf>>,
        observed_at_ms: u64,
        missing_member_requires_exhaustive: bool,
    ) {
        let mut state = self.lock_state();
        for (route, watermark) in routes {
            if state.known_route_ids.contains(&route) {
                let recorded =
                    state
                        .dirty_routes
                        .record_event(route.clone(), watermark, observed_at_ms);
                if recorded {
                    let workset = members
                        .remove(&route)
                        .map(SourceBackedRefreshWorkset::members)
                        .unwrap_or_default();
                    state
                        .route_worksets
                        .entry(route.clone())
                        .and_modify(|current| current.merge(workset.clone()))
                        .or_insert(workset);
                    if missing_member_requires_exhaustive
                        && state.route_worksets.get(&route).is_some_and(|workset| {
                            matches!(workset, SourceBackedRefreshWorkset::Exhaustive)
                        })
                    {
                        state
                            .routes_requiring_exhaustive_reconciliation
                            .insert(route.clone());
                    }
                    state
                        .route_event_watermarks
                        .insert(route.clone(), watermark);
                }
            }
        }
    }

    pub fn watch_routes_initialized(&self) -> bool {
        self.lock_state().watch_routes_initialized
    }

    pub fn next_dirty_route_due_in_ms(&self, now_ms: u64) -> Option<u64> {
        self.lock_state()
            .dirty_routes
            .next_due_at_ms()
            .map(|due| due.saturating_sub(now_ms))
    }

    pub fn has_scheduled_route_work(&self) -> bool {
        self.lock_state().dirty_routes.next_due_at_ms().is_some()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn scheduled_route_ids_for_test(&self) -> BTreeSet<SourceRouteIdentity> {
        self.lock_state().dirty_routes.route_ids()
    }

    #[cfg(test)]
    pub fn active_reconciliation_demand_for_test(
        &self,
    ) -> Option<SourceBackedReconciliationDemand> {
        let state = self.lock_state();
        state
            .active_request_id
            .as_deref()
            .and_then(|request_id| find_attempt(&state, request_id))
            .map(|attempt| attempt.reconciliation_demand)
    }

    #[cfg(test)]
    pub fn set_route_event_watermark_for_test(
        &self,
        route: SourceRouteIdentity,
        watermark: EventWatermark,
    ) {
        self.lock_state()
            .route_event_watermarks
            .insert(route, watermark);
    }

    #[cfg(test)]
    pub fn route_event_watermark_for_test(
        &self,
        route: &SourceRouteIdentity,
    ) -> Option<EventWatermark> {
        self.lock_state().route_event_watermarks.get(route).copied()
    }

    /// Projects only durable retained-route missing grace back into the exact
    /// watcher ledger. Healthy routes never enter this safety path.
    pub fn schedule_pending_missing_route_rechecks(
        &self,
        data_root: &Path,
        watcher_watermark: EventWatermark,
        observed_at_ms: u64,
    ) -> Result<usize> {
        let Some(index) = open_published_generation(data_root, self.journal.as_ref())? else {
            return Ok(0);
        };
        let generation_id = index.generation_id().to_owned();
        let pending = index
            .manifest()
            .source_routes()
            .iter()
            .filter(|route| route.missing_state().is_some())
            .map(|route| route.route_identity().clone())
            .collect::<Vec<_>>();

        let mut state = self.lock_state();
        if !state.watch_routes_initialized {
            return Ok(0);
        }
        if state
            .current_published_generation
            .as_deref()
            .is_some_and(|current| current != generation_id.as_str())
        {
            // Publication advanced after this safety read. The next safety
            // pass will inspect its exact active manifest.
            return Ok(0);
        }
        let pending = pending
            .into_iter()
            .filter(|route| state.known_route_ids.contains(route))
            .collect::<Vec<_>>();
        let watermark = state.dirty_routes.seed_watermark().max(watcher_watermark);
        Ok(state
            .dirty_routes
            .seed_clean_exact_routes(pending, watermark, observed_at_ms))
    }

    /// Inspects only persisted Core Hermes control receipts and queues exact
    /// route reconciliation when their one-hour deadline is due.
    pub fn enqueue_overdue_hermes_exact_reconciliation(
        &self,
        data_root: &Path,
        catalog: &SourceBackedWatchCatalog,
        now_ms: u64,
    ) -> Result<bool> {
        let Some(index) = open_published_generation(data_root, self.journal.as_ref())? else {
            return Ok(false);
        };
        let routes = overdue_hermes_exact_routes(
            &index,
            i64::try_from(now_ms).unwrap_or(i64::MAX),
            |route| catalog.route_control_expectation(route).copied(),
        );
        if routes.is_empty() {
            return Ok(false);
        }
        let routes = {
            let mut state = self.lock_state();
            let routes = routes
                .into_iter()
                .filter(|route| state.known_route_ids.contains(route))
                .collect::<BTreeSet<_>>();
            if routes.is_empty() {
                return Ok(false);
            }
            let watermark = state.dirty_routes.seed_watermark();
            state
                .dirty_routes
                .seed_exact_routes(routes.iter().cloned(), watermark, now_ms);
            routes
        };
        let response = match self.enqueue_intent(
            Some(index.generation_id().to_owned()),
            SourceRefreshRuntimeMetadata::periodic(),
            RefreshIntent::AutomaticMaintenance,
            SourceBackedRefreshScope::Exact(routes),
            None,
            None,
        ) {
            Ok(response) => response,
            Err(error)
                if error
                    .downcast_ref::<SourceBackedRefreshQueueFull>()
                    .is_some() =>
            {
                return Ok(false)
            }
            Err(error) => return Err(error),
        };
        let request_id = response
            .get("request_id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("overdue Hermes exact refresh has no request ID"))?;
        self.persist_job_status(data_root, request_id)?;
        Ok(true)
    }
}
