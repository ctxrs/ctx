use super::*;

const REQUEST_BOUND_STATUS_RECOVERY_ATTEMPT_LIMIT: usize = 3;
pub(super) const DISCONNECT_POLICY: &str = "retain_after_durable_admission";

#[derive(Debug)]
pub(super) struct SourceRefreshObservationRecoveryFailed {
    pub(super) request_id: String,
    pub(super) recovery_attempts: usize,
    pub(super) disconnect_policy: &'static str,
}

impl fmt::Display for SourceRefreshObservationRecoveryFailed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "daemon source refresh status for durably admitted request {} remains temporarily unobservable after {} recovery attempts; disconnect_policy={} and the request continues under daemon ownership",
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

#[cfg(test)]
#[path = "client_observation_recovery_tests.rs"]
mod tests;
