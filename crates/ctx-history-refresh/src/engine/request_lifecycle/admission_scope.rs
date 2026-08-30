use super::*;

impl CoreRefreshEngine {
    pub(crate) fn attempt_history_progress(
        &self,
        request_id: &str,
    ) -> Result<ctx_history_capture_model::SharedAttemptHistoryProgress> {
        let state = self.lock_state();
        find_attempt(&state, request_id)
            .and_then(|attempt| attempt.attempt_history_progress.clone())
            .ok_or_else(|| {
                anyhow!("source refresh execution has no active history progress handle")
            })
    }

    pub fn status(&self, request_id: &str) -> Option<RefreshStatus> {
        let state = self.lock_state();
        projected_status_json(&state, request_id).map(RefreshStatus::from_schema_v1_fields)
    }

    pub(super) fn refresh_intent(&self, request_id: &str) -> Option<RefreshIntent> {
        let state = self.lock_state();
        find_attempt(&state, request_id).map(|attempt| attempt.intent.clone())
    }

    #[cfg(test)]
    pub(super) fn refresh_scope(&self, request_id: &str) -> Option<SourceBackedRefreshScope> {
        let state = self.lock_state();
        find_attempt(&state, request_id).map(|attempt| attempt.refresh_scope.clone())
    }

    pub(super) fn reconciliation_demand(
        &self,
        request_id: &str,
    ) -> Option<SourceBackedReconciliationDemand> {
        let state = self.lock_state();
        find_attempt(&state, request_id).map(|attempt| attempt.reconciliation_demand)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn request_catalog_authority_for_test(
        &self,
        request_id: &str,
    ) -> Option<ExplicitSourceCatalogAuthority> {
        let state = self.lock_state();
        find_attempt(&state, request_id)
            .and_then(|attempt| attempt.requested_explicit_source_catalog().cloned())
    }

    pub(super) fn admit_refresh(
        &self,
        request_id: &str,
    ) -> Result<ctx_history_refresh_execution::AdmittedRefresh> {
        let now_ms = source_route_ledger_now_ms();
        let mut state = self.lock_state();
        if state.route_admissions.contains_key(request_id) {
            bail!("source refresh request `{request_id}` already has retained route admissions");
        }
        let admitted_authority = find_attempt(&state, request_id)
            .and_then(|attempt| attempt.admitted_authority.clone())
            .ok_or_else(|| anyhow!("source refresh execution has no admitted authority"))?;
        let scope = find_attempt(&state, request_id)
            .map(|attempt| attempt.refresh_scope.clone())
            .ok_or_else(|| anyhow!("source refresh request `{request_id}` is unknown"))?;
        let intent = find_attempt(&state, request_id)
            .map(|attempt| attempt.intent.clone())
            .ok_or_else(|| anyhow!("source refresh request `{request_id}` is unknown"))?;
        let authority_matches_scope = match (admitted_authority.coverage(), &scope) {
            (
                ctx_history_refresh_execution::AdmittedRefreshCoverage::CompleteCatalog,
                SourceBackedRefreshScope::All,
            ) => true,
            (
                ctx_history_refresh_execution::AdmittedRefreshCoverage::SelectedRoutes,
                SourceBackedRefreshScope::Exact(routes),
            ) => routes == admitted_authority.exact_routes(),
            _ => false,
        };
        if !authority_matches_scope {
            bail!("source refresh execution does not match its admitted exact scope");
        }
        let exact_routes = admitted_authority.exact_routes().clone();
        if exact_routes.len() > SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT {
            bail!(
                "daemon exact source refresh exceeds {SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT} routes"
            );
        }
        let should_seed = matches!(intent, RefreshIntent::SelectedImport(_))
            || matches!(scope, SourceBackedRefreshScope::All);
        if matches!(intent, RefreshIntent::SelectedImport(_)) {
            state
                .automatic_retry_checkpoints
                .retain(|route, _| !exact_routes.contains(route));
            let automatic_retry_checkpoints = state.automatic_retry_checkpoints.clone();
            if let Some(attempt) = find_attempt_mut(&mut state, request_id) {
                attempt.automatic_retry_checkpoints = automatic_retry_checkpoints;
            }
        }
        if should_seed {
            let watermark = state.dirty_routes.seed_watermark();
            for route in &exact_routes {
                state
                    .route_event_watermarks
                    .entry(route.clone())
                    .and_modify(|current| *current = (*current).max(watermark))
                    .or_insert(watermark);
            }
            state.dirty_routes.seed_exact_routes(
                exact_routes.iter().cloned(),
                watermark,
                // Logical admission is explicit authority, so its exact first
                // attempt bypasses watcher debounce without admitting any peer.
                now_ms.saturating_sub(1_000),
            );
        }
        let admissions = if exact_routes.is_empty() {
            Vec::new()
        } else {
            state
                .dirty_routes
                .admit_exact_routes(&exact_routes, now_ms)
                .ok_or_else(|| {
                    anyhow!("one or more exact source routes are no longer due for admission")
                })?
        };
        // An exhaustive obligation is transferred to this admitted attempt.
        // Subsequent watcher evidence re-adds its own reason, while failure
        // finalization re-arms this attempt's reason.  Do not leave cleanup
        // to terminal success: it cannot distinguish a pre-admission reason
        // from a newer one recorded while the executor was running.
        if find_attempt(&state, request_id).is_some_and(|attempt| {
            attempt.reconciliation_demand == SourceBackedReconciliationDemand::Exhaustive
        }) {
            for admission in &admissions {
                state
                    .hermes_routes_requiring_exhaustive_recovery
                    .remove(admission.route());
                state
                    .routes_requiring_exhaustive_reconciliation
                    .remove(admission.route());
            }
        }
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
        state
            .route_admission_watermarks
            .insert(request_id.to_owned(), admitted_watermarks);
        let incremental_exact = find_attempt(&state, request_id).is_some_and(|attempt| {
            attempt.reconciliation_demand == SourceBackedReconciliationDemand::Incremental
                && matches!(scope, SourceBackedRefreshScope::Exact(_))
        });
        let admitted_routes = state
            .route_admissions
            .get(request_id)
            .into_iter()
            .flatten()
            .map(|admission| admission.route().clone())
            .collect::<Vec<_>>();
        let mut route_worksets = BTreeMap::new();
        for route in admitted_routes {
            if let Some(workset) = state.route_worksets.remove(&route) {
                if incremental_exact {
                    route_worksets.insert(route, workset);
                }
            }
        }
        admitted_authority.with_execution_facts(route_worksets)
    }

    #[cfg(test)]
    pub fn admit_refresh_scope_for_test(
        &self,
        request_id: &str,
        scope: &SourceBackedRefreshScope,
    ) -> Result<BTreeSet<SourceRouteIdentity>> {
        if self.refresh_scope(request_id).as_ref() != Some(scope) {
            bail!("test source refresh scope does not match the queued request");
        }
        self.admit_refresh(request_id).map(|_| BTreeSet::new())
    }
}
