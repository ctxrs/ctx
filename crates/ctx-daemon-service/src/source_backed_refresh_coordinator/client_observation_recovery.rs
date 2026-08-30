use super::*;

const REQUEST_BOUND_STATUS_RECOVERY_ATTEMPT_LIMIT: usize = 3;
const RETAINED_REQUEST_CONTINUOUS_OUTAGE_BUDGET: StdDuration = StdDuration::from_secs(30);
pub(super) const DISCONNECT_POLICY: &str = "request_outcome_unknown_after_acknowledgement";

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

#[cfg(test)]
pub(super) fn request_bound_status_with_recovery<S, R>(
    request_id: &str,
    mut sleep: S,
    roundtrip: R,
) -> Result<Option<Value>>
where
    S: FnMut(StdDuration),
    R: FnMut() -> Result<Option<Value>>,
{
    request_bound_status_with_recovery_cancellable(
        request_id,
        |duration| {
            sleep(duration);
            Ok(())
        },
        || Ok(()),
        roundtrip,
    )
}

fn request_bound_status_with_recovery_cancellable<S, C, R>(
    request_id: &str,
    mut sleep: S,
    mut checkpoint: C,
    mut roundtrip: R,
) -> Result<Option<Value>>
where
    S: FnMut(StdDuration) -> Result<()>,
    C: FnMut() -> Result<()>,
    R: FnMut() -> Result<Option<Value>>,
{
    checkpoint()?;
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
        checkpoint()?;
        sleep(backoff)?;
        checkpoint()?;
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

#[cfg(test)]
pub(super) fn request_bound_status_with_outage_budget<S, N, R>(
    request_id: &str,
    mut sleep: S,
    now: N,
    roundtrip: R,
) -> Result<Option<Value>>
where
    S: FnMut(StdDuration),
    N: FnMut() -> StdInstant,
    R: FnMut() -> Result<Option<Value>>,
{
    request_bound_status_with_outage_budget_cancellable(
        request_id,
        |duration| {
            sleep(duration);
            Ok(())
        },
        now,
        || Ok(()),
        roundtrip,
    )
}

pub(super) fn request_bound_status_with_outage_budget_cancellable<S, N, C, R>(
    request_id: &str,
    mut sleep: S,
    mut now: N,
    mut checkpoint: C,
    mut roundtrip: R,
) -> Result<Option<Value>>
where
    S: FnMut(StdDuration) -> Result<()>,
    N: FnMut() -> StdInstant,
    C: FnMut() -> Result<()>,
    R: FnMut() -> Result<Option<Value>>,
{
    let mut outage_started_at = None;
    loop {
        checkpoint()?;
        let burst_started_at = now();
        match request_bound_status_with_recovery_cancellable(
            request_id,
            &mut sleep,
            &mut checkpoint,
            &mut roundtrip,
        ) {
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
                checkpoint()?;
                sleep(SOURCE_REFRESH_POLL_INTERVAL)?;
                checkpoint()?;
            }
            outcome => return outcome,
        }
    }
}

#[cfg(test)]
#[path = "client_observation_recovery_tests.rs"]
mod tests;
