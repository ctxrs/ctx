use super::*;

/// One call's bounded, already-admitted queue prefix. Recovery never restores
/// this optimization: each durable logical request is admitted again normally.
pub(super) struct QueuedRefreshBatch {
    members: Vec<(String, Option<String>)>,
    routes: BTreeMap<SourceRouteIdentity, Option<EventWatermark>>,
}

impl QueuedRefreshBatch {
    pub(super) fn snapshot(state: &CoreRefreshEngineState, request_id: &str) -> Option<Self> {
        if state.watch_uncertain_through.is_some() {
            return None;
        }
        let root = find_attempt(state, request_id)?;
        let authority = batch_authority(state, root)?;
        let routes = authority
            .exact_routes()
            .iter()
            .map(|route| {
                (
                    route.clone(),
                    state.route_event_watermarks.get(route).copied(),
                )
            })
            .collect();
        let members = state
            .pending_request_ids
            .iter()
            .take(SOURCE_REFRESH_ACTIVE_PENDING_LIMIT.saturating_sub(1))
            .map_while(|request_id| {
                let member = find_attempt(state, request_id)?;
                let other = batch_authority(state, member)?;
                let discovery = authority.discovery();
                let other_discovery = other.discovery();
                (member.reconciliation_demand == root.reconciliation_demand
                    && member.route_observations == root.route_observations
                    && other.exact_routes() == authority.exact_routes()
                    && other_discovery.report() == discovery.report()
                    && other_discovery.watch_catalog() == discovery.watch_catalog()
                    && other_discovery.configured_provider_roots()
                        == discovery.configured_provider_roots()
                    && other_discovery.automatic_provider_discovery()
                        == discovery.automatic_provider_discovery())
                .then(|| (request_id.clone(), member.previous_generation.clone()))
            })
            .collect::<Vec<_>>();
        (!members.is_empty()).then_some(Self { members, routes })
    }

    /// Bind this pre-capture membership to the physical admission and the
    /// verified full-catalog terminal. Optional warm-skip observations cannot
    /// replace or invalidate this new capture's exact route accounting.
    pub(super) fn bind_capture(
        self,
        state: &CoreRefreshEngineState,
        request_id: &str,
        terminal: &CoreRefreshTerminalSuccess,
    ) -> Option<CoveredQueuedRefreshBatch> {
        let root = find_attempt(state, request_id)?;
        let admitted = state.route_admission_watermarks.get(request_id)?;
        let receipt = terminal.receipt();
        if root.state != SourceBackedRefreshState::Running
            || receipt.route_results.len() != self.routes.len()
            || !receipt.route_results.iter().all(|result| {
                SourceRouteIdentity::from_sha256(result.route_identity.clone())
                    .ok()
                    .is_some_and(|route| self.routes.contains_key(&route))
                    && result.outcome.is_success()
                    && source_backed_route_retry_disposition(result).is_none()
            })
            || !self.routes.iter().all(|(route, required)| {
                admitted
                    .get(route)
                    .is_some_and(|captured| required.is_none_or(|required| *captured >= required))
            })
        {
            return None;
        }
        Some(CoveredQueuedRefreshBatch {
            members: self.members,
            receipt: receipt.clone(),
        })
    }
}

/// Request completion from one newly verified capture, separate from a dirty
/// route certificate. This proof grants no warm skip or watcher acknowledgement.
/// Its members were admitted before capture began, including routes for which
/// the watcher has no bounded observation token. Unknown remains unknown.
pub(super) struct CoveredQueuedRefreshBatch {
    members: Vec<(String, Option<String>)>,
    receipt: SourceBackedRefreshReceipt,
}

impl CoveredQueuedRefreshBatch {
    pub(super) fn publish_covered_members<Published>(
        self,
        state: &mut CoreRefreshEngineState,
        root_request_id: &str,
        certificate: Option<&SourceBackedRefreshCoverageCertificate>,
        did_work: bool,
        published: &mut Published,
    ) -> Option<SourceBackedRefreshRun>
    where
        Published: FnMut(&Value) -> Result<()>,
    {
        let certificate = certificate?;
        let root = find_attempt(state, root_request_id)?.clone();
        let receipt = &self.receipt;
        if root.receipt.as_ref() != Some(receipt)
            || certificate.request_id != root_request_id
            || certificate.published_generation != receipt.published_generation
        {
            return None;
        }

        let mut last = None;
        for (request_id, previous_generation) in self.members {
            // Only this pre-capture prefix may consume the proof. New arrivals
            // and admission-pending requests always keep their own next run.
            if state.active_request_id.as_deref() != Some(request_id.as_str())
                || find_attempt(state, &request_id)
                    .is_none_or(|attempt| batch_authority(state, attempt).is_none())
            {
                break;
            }
            let attempt = find_attempt_mut(state, &request_id)?;
            let mut receipt = receipt.clone();
            receipt.previous_generation = previous_generation.clone();
            receipt.generation_changed =
                previous_generation.as_deref() != Some(receipt.published_generation.as_str());
            // Carry only verified execution results. Identity, fingerprint,
            // trigger and admission acknowledgement remain member-owned.
            attempt.previous_generation = previous_generation;
            attempt.published_generation = Some(receipt.published_generation.clone());
            attempt.started_at_ms = root.started_at_ms;
            attempt.finished_at_ms = root.finished_at_ms;
            attempt.state = SourceBackedRefreshState::Published;
            attempt.progress = root.progress.clone();
            attempt.progress_total_sources_known = root.progress_total_sources_known;
            attempt.scanned_routes = root.scanned_routes;
            attempt.unsupported_routes = root.unsupported_routes;
            attempt.request_source_count = root.request_source_count;
            attempt.certified_source_count = root.certified_source_count;
            attempt.certified_source_bytes = root.certified_source_bytes;
            attempt.receipt = Some(receipt);
            attempt.route_observations = root.route_observations.clone();
            attempt.timings = root.timings;
            attempt.last_error = None;
            update_automatic_retry_after_publication(state, &request_id);
            // The physical owner already finalized the dirty-route ledger.
            // Rebinding this exact proof must not acknowledge a newer event.
            let mut coverage = certificate.clone();
            coverage.request_id = request_id.clone();
            let job = durable_job_json(state, &request_id)?;
            let pending = published(&job).is_err();
            if pending {
                state.pending_terminal_persistence = Some(PendingTerminalPersistence {
                    request_id: request_id.clone(),
                    terminal_job: job.clone(),
                    outcome: PendingTerminalOutcome::Published {
                        did_work,
                        coverage_certificate: Some(coverage.clone()),
                    },
                });
            } else {
                advance_after_terminal_attempt(
                    state,
                    &request_id,
                    Some(coverage.published_generation.clone()),
                );
            }
            last = Some(SourceBackedRefreshRun {
                job,
                did_work: did_work && !pending,
                failed: false,
                terminal_persistence_pending: pending,
                scope: root.refresh_scope.clone(),
                coverage_certificate: (!pending).then_some(coverage),
            });
            if pending {
                break;
            }
        }
        last
    }
}

fn batch_authority<'a>(
    state: &CoreRefreshEngineState,
    attempt: &'a SourceBackedRefreshAttempt,
) -> Option<&'a ctx_history_refresh_execution::AdmittedRefresh> {
    if attempt.state != SourceBackedRefreshState::Queued
        || attempt.intent != RefreshIntent::AutomaticMaintenance
        || attempt.refresh_scope != SourceBackedRefreshScope::All
        || attempt.admission_durability_indeterminate
        || state
            .unacknowledged_admissions
            .contains_key(&attempt.request_id)
        || state
            .admission_resolutions_in_flight
            .contains(&attempt.request_id)
        || state.route_admissions.contains_key(&attempt.request_id)
    {
        return None;
    }
    attempt.admitted_authority.as_ref().filter(|authority| {
        authority.coverage()
            == ctx_history_refresh_execution::AdmittedRefreshCoverage::CompleteCatalog
            && authority.route_worksets().is_empty()
    })
}
