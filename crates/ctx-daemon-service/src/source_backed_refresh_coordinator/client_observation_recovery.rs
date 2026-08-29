use super::*;

const REQUEST_BOUND_STATUS_RECOVERY_ATTEMPT_LIMIT: usize = 3;
const RETAINED_REQUEST_CONTINUOUS_OUTAGE_BUDGET: StdDuration = StdDuration::from_secs(30);
const TYPED_UNKNOWN_RECOVERY_ATTEMPT_LIMIT: usize = 3;
pub(super) const DISCONNECT_POLICY: &str = "request_outcome_unknown_after_acknowledgement";

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(in super::super) enum SourceRefreshRequestRecoveryFailureReason {
    AttemptsExhausted,
    RequestIdChanged,
    ReenqueueFailed,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(in super::super) enum SourceRefreshRequestRetention {
    NotRetained,
    MayBeRetained,
}

#[derive(Debug)]
pub(in super::super) struct SourceRefreshRequestRecoveryFailed {
    pub(in super::super) request_id: String,
    pub(in super::super) recovery_attempts: usize,
    pub(in super::super) reason: SourceRefreshRequestRecoveryFailureReason,
    pub(in super::super) retention: SourceRefreshRequestRetention,
    pub(in super::super) disconnect_policy: Option<&'static str>,
    pub(in super::super) detail: Option<String>,
}

impl fmt::Display for SourceRefreshRequestRecoveryFailed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = match self.reason {
            SourceRefreshRequestRecoveryFailureReason::AttemptsExhausted => {
                "typed unknown-request recovery attempts were exhausted"
            }
            SourceRefreshRequestRecoveryFailureReason::RequestIdChanged => {
                "typed unknown-request recovery returned a different logical request ID"
            }
            SourceRefreshRequestRecoveryFailureReason::ReenqueueFailed => {
                "same-ID recovery could not durably re-admit the logical request"
            }
        };
        write!(
            formatter,
            "daemon source refresh request {} could not be conclusively recovered after {} recovery attempts: {reason}",
            self.request_id, self.recovery_attempts
        )?;
        match self.retention {
            SourceRefreshRequestRetention::NotRetained => {
                formatter.write_str("; request_retained=false")?;
            }
            SourceRefreshRequestRetention::MayBeRetained => {
                write!(
                    formatter,
                    "; request_retained=unknown; disconnect_policy={}",
                    self.disconnect_policy.unwrap_or(DISCONNECT_POLICY)
                )?;
            }
        }
        if let Some(detail) = self.detail.as_deref() {
            write!(formatter, ": {detail}")?;
        }
        Ok(())
    }
}

impl std::error::Error for SourceRefreshRequestRecoveryFailed {}

#[derive(Debug)]
pub(in super::super) struct TypedUnknownRequestRecovery {
    attempts: usize,
}

impl TypedUnknownRequestRecovery {
    pub(in super::super) fn new(_initial_request_id: &str) -> Self {
        Self { attempts: 0 }
    }

    fn begin_attempt(&mut self, request_id: &str) -> Result<StdDuration> {
        if self.attempts >= TYPED_UNKNOWN_RECOVERY_ATTEMPT_LIMIT {
            return Err(SourceRefreshRequestRecoveryFailed {
                request_id: request_id.to_owned(),
                recovery_attempts: self.attempts,
                reason: SourceRefreshRequestRecoveryFailureReason::AttemptsExhausted,
                retention: SourceRefreshRequestRetention::NotRetained,
                disconnect_policy: None,
                detail: None,
            }
            .into());
        }
        let backoff = match self.attempts {
            0 => StdDuration::from_millis(25),
            1 => StdDuration::from_millis(50),
            _ => StdDuration::from_millis(100),
        };
        self.attempts = self.attempts.saturating_add(1);
        Ok(backoff)
    }

    fn accept_recovered_request_id(
        &mut self,
        previous_request_id: &str,
        recovered_request_id: String,
    ) -> Result<String> {
        if recovered_request_id != previous_request_id {
            return Err(SourceRefreshRequestRecoveryFailed {
                request_id: previous_request_id.to_owned(),
                recovery_attempts: self.attempts,
                reason: SourceRefreshRequestRecoveryFailureReason::RequestIdChanged,
                retention: SourceRefreshRequestRetention::NotRetained,
                disconnect_policy: None,
                detail: Some(format!(
                    "recovery response named logical request {recovered_request_id}"
                )),
            }
            .into());
        }
        Ok(recovered_request_id)
    }
}

pub(in super::super) fn recover_typed_unknown_request_with<S, R>(
    recovery: &mut TypedUnknownRequestRecovery,
    request_id: &str,
    sleep: S,
    reenqueue: R,
) -> Result<String>
where
    S: FnOnce(StdDuration),
    R: FnOnce() -> Result<String>,
{
    recover_typed_unknown_request_with_policy(recovery, request_id, sleep, reenqueue, true)
}

pub(in super::super) fn recover_typed_unknown_coalesced_request_with<S, R>(
    recovery: &mut TypedUnknownRequestRecovery,
    request_id: &str,
    sleep: S,
    reenqueue: R,
) -> Result<String>
where
    S: FnOnce(StdDuration),
    R: FnOnce() -> Result<String>,
{
    recover_typed_unknown_request_with_policy(recovery, request_id, sleep, reenqueue, false)
}

fn recover_typed_unknown_request_with_policy<S, R>(
    recovery: &mut TypedUnknownRequestRecovery,
    request_id: &str,
    sleep: S,
    reenqueue: R,
    require_same_observation_id: bool,
) -> Result<String>
where
    S: FnOnce(StdDuration),
    R: FnOnce() -> Result<String>,
{
    let backoff = recovery.begin_attempt(request_id)?;
    sleep(backoff);
    let recovered_request_id = reenqueue().map_err(|error| {
        let retention = if error
            .downcast_ref::<SourceRefreshAdmissionRecoveryFailed>()
            .is_some()
        {
            SourceRefreshRequestRetention::MayBeRetained
        } else {
            SourceRefreshRequestRetention::NotRetained
        };
        SourceRefreshRequestRecoveryFailed {
            request_id: request_id.to_owned(),
            recovery_attempts: recovery.attempts,
            reason: SourceRefreshRequestRecoveryFailureReason::ReenqueueFailed,
            retention,
            disconnect_policy: (retention == SourceRefreshRequestRetention::MayBeRetained)
                .then_some(DISCONNECT_POLICY),
            detail: Some(format!("{error:#}")),
        }
    })?;
    if require_same_observation_id {
        recovery.accept_recovered_request_id(request_id, recovered_request_id)
    } else {
        Ok(recovered_request_id)
    }
}

#[derive(Debug)]
pub struct SourceRefreshObservationRecoveryFailed {
    pub request_id: String,
    pub recovery_attempts: usize,
    pub disconnect_policy: &'static str,
}

impl fmt::Display for SourceRefreshObservationRecoveryFailed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "daemon source refresh outcome for durably admitted request {} is no longer observable after {} recovery attempts; outcome is unknown; disconnect_policy={}",
            self.request_id, self.recovery_attempts, self.disconnect_policy
        )
    }
}

impl std::error::Error for SourceRefreshObservationRecoveryFailed {}

pub(super) fn retained_request_unobservable(
    request_id: &str,
    recovery_attempts: usize,
) -> anyhow::Error {
    SourceRefreshObservationRecoveryFailed {
        request_id: request_id.to_owned(),
        recovery_attempts,
        disconnect_policy: DISCONNECT_POLICY,
    }
    .into()
}

pub(super) fn request_bound_status_with_recovery<S, R>(
    request_id: &str,
    mut sleep: S,
    mut roundtrip: R,
) -> Result<Option<Value>>
where
    S: FnMut(StdDuration),
    R: FnMut() -> Result<Option<Value>>,
{
    match roundtrip() {
        Ok(response) => return Ok(response),
        Err(error)
            if error
                .downcast_ref::<DaemonSourceRefreshServiceUnavailable>()
                .is_some() =>
        {
            return Err(error);
        }
        Err(_) => {}
    }

    for recovery_attempt in 0..REQUEST_BOUND_STATUS_RECOVERY_ATTEMPT_LIMIT {
        let backoff = match recovery_attempt {
            0 => StdDuration::from_millis(25),
            1 => StdDuration::from_millis(50),
            _ => StdDuration::from_millis(100),
        };
        sleep(backoff);
        match roundtrip() {
            Ok(response) => return Ok(response),
            Err(error)
                if error
                    .downcast_ref::<DaemonSourceRefreshServiceUnavailable>()
                    .is_some() =>
            {
                return Err(error);
            }
            Err(_) => {}
        }
    }

    Err(retained_request_unobservable(
        request_id,
        REQUEST_BOUND_STATUS_RECOVERY_ATTEMPT_LIMIT,
    ))
}

pub(super) fn request_bound_status_with_outage_budget<S, N, R>(
    request_id: &str,
    mut sleep: S,
    mut now: N,
    mut roundtrip: R,
) -> Result<Option<Value>>
where
    S: FnMut(StdDuration),
    N: FnMut() -> StdInstant,
    R: FnMut() -> Result<Option<Value>>,
{
    let mut outage_started_at = None;
    loop {
        let burst_started_at = now();
        match request_bound_status_with_recovery(request_id, &mut sleep, &mut roundtrip) {
            Err(error)
                if error
                    .downcast_ref::<SourceRefreshObservationRecoveryFailed>()
                    .is_some() =>
            {
                let outage_started_at = *outage_started_at.get_or_insert(burst_started_at);
                if now().saturating_duration_since(outage_started_at)
                    >= RETAINED_REQUEST_CONTINUOUS_OUTAGE_BUDGET
                {
                    return Err(error);
                }
                sleep(SOURCE_REFRESH_POLL_INTERVAL);
            }
            outcome => return outcome,
        }
    }
}

#[cfg(test)]
#[path = "client_observation_recovery_tests.rs"]
mod tests;
