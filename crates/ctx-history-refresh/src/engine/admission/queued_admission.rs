use super::*;

impl CoreRefreshEngine {
    /// Prepare one bounded automatic prefix through ordinary per-request
    /// admission. Only successfully persisted Queued members can later share
    /// capture; a blocked or failed peer keeps its normal turn at the root.
    pub(in crate::engine) fn prepare_queued_batch_admissions(&self, data_root: &Path) {
        let request_ids = {
            let state = self.lock_state();
            let Some(root) = state
                .active_request_id
                .as_deref()
                .and_then(|id| find_attempt(&state, id))
            else {
                return;
            };
            if root.state != SourceBackedRefreshState::Queued
                || root.intent != RefreshIntent::AutomaticMaintenance
                || root.refresh_scope != SourceBackedRefreshScope::All
            {
                return;
            }
            state
                .pending_request_ids
                .iter()
                .take(SOURCE_REFRESH_ACTIVE_PENDING_LIMIT.saturating_sub(1))
                .map_while(|id| {
                    let peer = find_attempt(&state, id)?;
                    (peer.intent == root.intent
                        && peer.refresh_scope == root.refresh_scope
                        && peer.reconciliation_demand == root.reconciliation_demand
                        && matches!(
                            peer.state,
                            SourceBackedRefreshState::AdmissionPending
                                | SourceBackedRefreshState::Queued
                        ))
                    .then(|| id.clone())
                })
                .collect::<Vec<_>>()
        };
        for request_id in request_ids {
            if find_attempt(&self.lock_state(), &request_id)
                .is_some_and(|peer| peer.state == SourceBackedRefreshState::Queued)
            {
                continue;
            }
            let Some(claim) = self.claim_pending_admission(&request_id) else {
                break;
            };
            let resolution = self.resolve_pending_admission_claim(data_root, &claim);
            if self
                .complete_claimed_pending_admission(data_root, &claim, resolution)
                .is_err()
                || find_attempt(&self.lock_state(), &request_id)
                    .is_none_or(|peer| peer.state != SourceBackedRefreshState::Queued)
            {
                break;
            }
        }
    }

    pub(super) fn claim_active_pending_admission(&self) -> Option<PendingAdmissionClaim> {
        let request_id = self.lock_state().active_request_id.clone()?;
        self.claim_pending_admission(&request_id)
    }

    fn claim_pending_admission(&self, request_id: &str) -> Option<PendingAdmissionClaim> {
        let mut state = self.lock_state();
        if state.watch_uncertain_through.is_some() {
            return None;
        }
        let request_id = request_id.to_owned();
        let attempt = find_attempt(&state, &request_id)?;
        if attempt.state != SourceBackedRefreshState::AdmissionPending
            || state.unacknowledged_admissions.contains_key(&request_id)
            || state.admission_resolutions_in_flight.contains(&request_id)
        {
            return None;
        }
        let claim = PendingAdmissionClaim {
            request_id: request_id.clone(),
            intent: attempt.intent.clone(),
            persisted_scope: attempt.refresh_scope.clone(),
            watch_catalog_revision: state.watch_catalog_revision,
            route_event_watermarks: state.route_event_watermarks.clone(),
        };
        state
            .admission_resolutions_in_flight
            .insert(request_id.clone());
        Some(claim)
    }
}
