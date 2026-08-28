use super::*;

pub(super) fn validate_admission_observations(
    observations: BTreeMap<SourceRouteIdentity, Option<String>>,
) -> Result<BTreeMap<SourceRouteIdentity, Option<String>>> {
    if observations.len() > SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT {
        bail!("source refresh admission fence exceeds its bounded route capacity");
    }
    if observations
        .values()
        .flatten()
        .any(|observation| !is_sha256_identity(observation))
    {
        bail!("source refresh admission fence returned an invalid route observation");
    }
    Ok(observations)
}

pub(super) fn increment_response_barrier(state: &mut CoreRefreshEngineState, request_id: &str) {
    state
        .unacknowledged_admissions
        .entry(request_id.to_owned())
        .and_modify(|count| *count = count.saturating_add(1))
        .or_insert(1);
}

pub(super) fn durable_request_id<'a>(
    state: &'a CoreRefreshEngineState,
    request_id: &'a str,
) -> &'a str {
    if find_attempt(state, request_id).is_some_and(|attempt| !attempt.state.is_active()) {
        return request_id;
    }
    state
        .pending_scheduler_retry_root_id
        .as_deref()
        .or_else(|| {
            state
                .pending_terminal_persistence
                .as_ref()
                .map(|pending| pending.request_id.as_str())
        })
        .or(state.active_request_id.as_deref())
        .unwrap_or(request_id)
}

pub(super) struct AdmissionReservationSnapshot {
    active_request_id: Option<String>,
    pending_request_ids: VecDeque<String>,
    attempts: VecDeque<SourceBackedRefreshAttempt>,
    response_barriers: BTreeMap<String, usize>,
}

impl AdmissionReservationSnapshot {
    pub(super) fn capture(state: &CoreRefreshEngineState) -> Self {
        Self {
            active_request_id: state.active_request_id.clone(),
            pending_request_ids: state.pending_request_ids.clone(),
            attempts: state.attempts.clone(),
            response_barriers: state.unacknowledged_admissions.clone(),
        }
    }

    pub(super) fn restore(self, state: &mut CoreRefreshEngineState) {
        state.active_request_id = self.active_request_id;
        state.pending_request_ids = self.pending_request_ids;
        state.attempts = self.attempts;
        state.unacknowledged_admissions = self.response_barriers;
    }
}

pub(super) struct AdmissionResolutionSnapshot {
    attempts: VecDeque<SourceBackedRefreshAttempt>,
}

impl AdmissionResolutionSnapshot {
    pub(super) fn capture(state: &CoreRefreshEngineState) -> Self {
        Self {
            attempts: state.attempts.clone(),
        }
    }

    pub(super) fn restore(self, state: &mut CoreRefreshEngineState) {
        state.attempts = self.attempts;
    }
}
