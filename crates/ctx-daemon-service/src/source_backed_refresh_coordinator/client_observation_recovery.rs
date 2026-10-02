use super::*;
use ctx_daemon_runtime::{
    daemon_lock_path, observe_pid_advisory_guard, pid_from_lock_json,
    pid_lock_uses_advisory_protocol, process_state, read_pid_lock_json,
    DaemonQueryEndpointIdentity, ProcessState,
};

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
            "daemon source refresh outcome for request {} is no longer observable after {} recovery attempts; outcome is unknown; disconnect_policy={}",
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

pub(super) fn retryable_refresh_transport_error(error: &anyhow::Error) -> bool {
    // The submission marker also wraps decoding failures. Those are protocol
    // errors, not evidence that a slow or disconnected owner needs another try.
    if let Some(decode) = error.downcast_ref::<serde_json::Error>() {
        // A peer that closes without sending any ACK produces the parser's
        // empty-input EOF. Partial or malformed JSON remains an explicit error.
        return decode.is_eof()
            && decode.line() == 1
            && decode.column() == 0
            && DaemonSourceRefreshServiceUnavailable::request_may_have_been_submitted(error);
    }
    if error
        .downcast_ref::<ctx_daemon_runtime::DaemonQueryResponseTooLarge>()
        .is_some()
    {
        return false;
    }
    if error
        .downcast_ref::<DaemonSourceRefreshServiceUnavailable>()
        .is_some()
    {
        return true;
    }
    error.downcast_ref::<std::io::Error>().is_some_and(|error| {
        matches!(
            error.kind(),
            std::io::ErrorKind::TimedOut
                | std::io::ErrorKind::UnexpectedEof
                | std::io::ErrorKind::WouldBlock
                | std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::NotConnected
                | std::io::ErrorKind::NotFound
                | std::io::ErrorKind::Interrupted
        )
    })
}

#[derive(Clone, Debug, PartialEq)]
struct RefreshOwnerIdentity {
    owner_id: String,
    pid: u32,
    started_at_ms: i64,
    endpoint: DaemonQueryEndpointIdentity,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum RefreshOwnerObservation {
    Live,
    Lost,
    Unknown,
}

fn observe_refresh_owner(
    data_root: &Path,
    expected: Option<&RefreshOwnerIdentity>,
    owned_live: bool,
) -> Result<(RefreshOwnerObservation, Option<RefreshOwnerIdentity>)> {
    observe_refresh_owner_with_process_state(data_root, expected, owned_live, process_state)
}

fn observe_refresh_owner_with_process_state(
    data_root: &Path,
    expected: Option<&RefreshOwnerIdentity>,
    owned_live: bool,
    mut observe_process: impl FnMut(u32) -> ProcessState,
) -> Result<(RefreshOwnerObservation, Option<RefreshOwnerIdentity>)> {
    let lock_path = daemon_lock_path(data_root);
    let before = read_pid_lock_json(&lock_path);
    let held = observe_pid_advisory_guard(&lock_path);
    let endpoint = crate::query_service::read_daemon_service_endpoint_identity(
        data_root,
        crate::query_service::DaemonIpcService::SourceRefresh,
    )?;
    let after = read_pid_lock_json(&lock_path);
    if held == Some(false) {
        return Ok((RefreshOwnerObservation::Lost, None));
    }
    let Some(lock) = before.filter(|lock| Some(lock) == after.as_ref()) else {
        return Ok((RefreshOwnerObservation::Unknown, None));
    };
    if !pid_lock_uses_advisory_protocol(&lock) {
        return Ok((RefreshOwnerObservation::Unknown, None));
    }
    let Some(pid) = pid_from_lock_json(&lock) else {
        return Ok((RefreshOwnerObservation::Unknown, None));
    };
    if lock.get("released").and_then(Value::as_bool) == Some(true)
        || observe_process(pid) == ProcessState::NotRunning
    {
        return Ok((RefreshOwnerObservation::Lost, None));
    }
    let (Some(owner_id), Some(started_at_ms)) = (
        lock.get("owner_id").and_then(Value::as_str),
        lock.get("started_at_ms").and_then(Value::as_i64),
    ) else {
        return Ok((RefreshOwnerObservation::Unknown, None));
    };
    if let Some(expected) = expected {
        if expected.owner_id != owner_id
            || expected.pid != pid
            || expected.started_at_ms != started_at_ms
            || endpoint
                .as_ref()
                .is_some_and(|endpoint| endpoint != &expected.endpoint)
        {
            return Ok((RefreshOwnerObservation::Lost, None));
        }
    }
    // Endpoint disappearance alone is an IPC outage. A retained matching
    // endpoint identity still belongs to this exact, guarded owner.
    let endpoint = endpoint.or_else(|| expected.map(|owner| owner.endpoint.clone()));
    let Some(endpoint) = endpoint.filter(|endpoint| endpoint.owner_pid == pid) else {
        return Ok((RefreshOwnerObservation::Unknown, None));
    };
    if held != Some(true) || lock.get("released").and_then(Value::as_bool) != Some(false) {
        return Ok((RefreshOwnerObservation::Unknown, None));
    }
    Ok((
        // Exact retained Child::try_wait is stronger than an unavailable
        // redundant PID probe. Metadata and a contended guard alone are not.
        if owned_live {
            RefreshOwnerObservation::Live
        } else {
            RefreshOwnerObservation::Unknown
        },
        Some(RefreshOwnerIdentity {
            owner_id: owner_id.to_owned(),
            pid,
            started_at_ms,
            endpoint,
        }),
    ))
}

// One invocation owns one payload and these two allowances through admission,
// forgotten replay, and terminal observation. Neither allowance resets at ACK.
pub(super) struct WaitRefreshRecovery {
    request_id: String,
    trigger: RefreshRequestTrigger,
    allow_daemon_autostart: bool,
    admission: Value,
    status: Value,
    owner: Option<RefreshOwnerIdentity>,
    owner_restored: bool,
    forgotten_replayed: bool,
}

impl WaitRefreshRecovery {
    pub(super) fn new(
        data_root: &Path,
        request: &RefreshRequest,
        allow_daemon_autostart: bool,
    ) -> Result<Self> {
        let admission = wait_authority_request_json(SourceBackedRefreshMode::Wait, request)?;
        let request_id = admission["request_id"]
            .as_str()
            .context("canonical request ID")?
            .to_owned();
        Ok(Self {
            status: compact_json(json!({
                "schema_version": 1, "op": SOURCE_REFRESH_STATUS_OP, "request_id": request_id,
            })),
            request_id,
            trigger: request.trigger(),
            allow_daemon_autostart,
            admission,
            owner: observe_refresh_owner(data_root, None, false)?.1,
            owner_restored: false,
            forgotten_replayed: false,
        })
    }

    pub(super) fn request(
        &mut self,
        availability: &dyn crate::DaemonAvailabilityPort,
        data_root: &Path,
        admission: bool,
    ) -> Result<Value> {
        availability.checkpoint()?;
        if admission && !self.allow_daemon_autostart && self.owner.is_none() {
            return Err(SourceBackedRefreshDaemonUnavailable::new(None).into());
        }
        let mut owner = self.owner.take();
        let request_id = self.request_id.clone();
        let trigger = self.trigger;
        let result = self.recover_response(
            admission,
            |duration| availability.pause(duration),
            StdInstant::now,
            || availability.checkpoint(),
            |request| {
                daemon_source_refresh_request_with_cancellation(
                    availability,
                    data_root,
                    request.clone(),
                    SOURCE_REFRESH_IPC_TIMEOUT,
                    SOURCE_REFRESH_RESPONSE_MAX_BYTES,
                )
            },
            |restore| {
                if restore {
                    recover_wait_refresh_request(
                        availability,
                        data_root,
                        &request_id,
                        trigger,
                        true,
                    )?;
                    owner = observe_refresh_owner(data_root, None, false)?.1;
                }
                let proof = match owner.as_ref() {
                    Some(owner) => availability.source_refresh_owner_is_live(
                        data_root,
                        &owner.owner_id,
                        owner.pid,
                    )?,
                    None => None,
                };
                if proof == Some(false) {
                    return Ok(RefreshOwnerObservation::Lost);
                }
                let (observation, _) =
                    observe_refresh_owner(data_root, owner.as_ref(), proof == Some(true))?;
                // Without a captured identity, a newly visible owner does not
                // prove that the worker receiving the original request survived.
                Ok(
                    if owner.is_none() && observation == RefreshOwnerObservation::Live {
                        RefreshOwnerObservation::Unknown
                    } else {
                        observation
                    },
                )
            },
        );
        self.owner = owner;
        result
    }

    fn recover_response<S, N, C, R, O>(
        &mut self,
        mut admission: bool,
        mut sleep: S,
        mut now: N,
        mut checkpoint: C,
        mut roundtrip: R,
        mut observe_owner: O,
    ) -> Result<Value>
    where
        S: FnMut(StdDuration) -> Result<()>,
        N: FnMut() -> StdInstant,
        C: FnMut() -> Result<()>,
        R: FnMut(&Value) -> Result<Option<Value>>,
        O: FnMut(bool) -> Result<RefreshOwnerObservation>,
    {
        let mut retries = 0_usize;
        let mut uncertain_since = None;
        loop {
            checkpoint()?;
            let outcome = roundtrip(if admission {
                &self.admission
            } else {
                &self.status
            });
            checkpoint()?;
            match outcome {
                Ok(Some(response)) => {
                    if source_refresh_request_is_unknown(&response, &self.request_id)? {
                        if self.forgotten_replayed {
                            return Err(retained_request_unobservable(
                                &self.request_id,
                                usize::from(self.owner_restored || self.forgotten_replayed),
                            ));
                        }
                        self.forgotten_replayed = true;
                        admission = true;
                        continue;
                    }
                    validate_daemon_refresh_response(&response)?;
                    validate_source_refresh_status_response_authority(&response, &self.request_id)?;
                    source_refresh_protocol_status(&response)?;
                    return Ok(response);
                }
                Err(error) if !retryable_refresh_transport_error(&error) => return Err(error),
                Ok(None) | Err(_) => {}
            }
            match observe_owner(false)? {
                RefreshOwnerObservation::Live => uncertain_since = None,
                RefreshOwnerObservation::Lost => {
                    if !self.allow_daemon_autostart || self.owner_restored {
                        return Err(retained_request_unobservable(
                            &self.request_id,
                            usize::from(self.owner_restored || self.forgotten_replayed),
                        ));
                    }
                    self.owner_restored = true;
                    checkpoint()?;
                    observe_owner(true)?;
                    checkpoint()?;
                    // Startup recovery is authoritative. Ask about the original
                    // ID before any replay, even when its ACK was never seen.
                    admission = false;
                    uncertain_since = None;
                    continue;
                }
                RefreshOwnerObservation::Unknown => {
                    let observed_at = now();
                    let started_at = *uncertain_since.get_or_insert(observed_at);
                    if observed_at.saturating_duration_since(started_at)
                        >= RETAINED_REQUEST_CONTINUOUS_OUTAGE_BUDGET
                    {
                        return Err(retained_request_unobservable(
                            &self.request_id,
                            usize::from(self.owner_restored || self.forgotten_replayed),
                        ));
                    }
                }
            }
            let backoff = match retries {
                0 => 25,
                1 => 50,
                _ => 100,
            };
            retries = retries.saturating_add(1);
            checkpoint()?;
            sleep(StdDuration::from_millis(backoff))?;
            checkpoint()?;
        }
    }
}

#[cfg(test)]
#[path = "client_observation_recovery_tests.rs"]
mod tests;
