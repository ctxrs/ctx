use super::*;

pub(super) fn daemon_handoff_status_observation_from(
    status: Option<&Value>,
    owner: Option<&DaemonOwnerIdentity>,
    expected_failure_pid: Option<u32>,
    expected_config: &DaemonConfigSnapshot,
    readiness: DaemonReadinessRequirement,
    now_ms: i64,
) -> DaemonHandoffObservation {
    let Some(status) = status else {
        return DaemonHandoffObservation::Pending;
    };
    let status_name = status.get("status").and_then(Value::as_str);
    let status_pid = status
        .get("pid")
        .and_then(Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok());
    let status_started_at_ms = status
        .get("started_at_ms")
        .and_then(Value::as_i64)
        .filter(|started_at_ms| *started_at_ms > 0);
    let last_error = || {
        status
            .get("last_error")
            .and_then(Value::as_str)
            .unwrap_or("daemon startup failed")
            .to_owned()
    };
    let heartbeat_is_fresh = || {
        status
            .get("heartbeat_at_ms")
            .and_then(Value::as_i64)
            .is_some_and(|heartbeat| {
                heartbeat > 0
                    && now_ms.saturating_sub(heartbeat) <= DAEMON_SETUP_HANDOFF_MAX_HEARTBEAT_AGE_MS
                    && heartbeat.saturating_sub(now_ms)
                        <= DAEMON_SETUP_HANDOFF_MAX_FUTURE_HEARTBEAT_MS
            })
    };
    if status_name == Some("failed") {
        let belongs_to_handoff = owner.is_some_and(|owner| {
            status_pid == Some(owner.pid)
                && status_started_at_ms == Some(owner.started_at_ms)
                && expected_failure_pid.is_none_or(|expected| expected == owner.pid)
        });
        if belongs_to_handoff && heartbeat_is_fresh() {
            return DaemonHandoffObservation::Failed(last_error());
        }
        return DaemonHandoffObservation::Pending;
    }
    let Some(owner) = owner else {
        return DaemonHandoffObservation::Pending;
    };
    if status_name != Some("running")
        || status_pid != Some(owner.pid)
        || status_started_at_ms != Some(owner.started_at_ms)
    {
        return DaemonHandoffObservation::Pending;
    }
    match status
        .get("config_reload")
        .and_then(|reload| reload.get("status"))
        .and_then(Value::as_str)
    {
        Some("activation_failed")
            if readiness == DaemonReadinessRequirement::Core
                && expected_config.semantic_enabled
                && daemon_requested_config_matches(status, expected_config)
                && daemon_applied_degraded_semantic_config_matches(status, expected_config) => {}
        Some("failed" | "activation_failed") => {
            if heartbeat_is_fresh() {
                let error = status
                    .get("config_reload")
                    .and_then(|reload| reload.get("last_error"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(last_error);
                return DaemonHandoffObservation::Failed(error);
            }
            return DaemonHandoffObservation::Pending;
        }
        Some("applied") if daemon_applied_config_matches(status, expected_config) => {}
        _ => return DaemonHandoffObservation::Pending,
    }
    let heartbeat_at_ms = status
        .get("heartbeat_at_ms")
        .and_then(Value::as_i64)
        .filter(|heartbeat| *heartbeat > 0)
        .unwrap_or_default();
    DaemonHandoffObservation::Running(DaemonHandoff {
        pid: owner.pid,
        heartbeat_at_ms,
    })
}

pub(super) fn daemon_lifecycle_response_observation(
    response: &Value,
    expected_pid: u32,
) -> DaemonLifecycleEndpointObservation {
    let identity_matches = response.get("schema_version").and_then(Value::as_u64) == Some(1)
        && response.get("ok").and_then(Value::as_bool) == Some(true)
        && response.get("owner").and_then(Value::as_str) == Some("daemon")
        && response.get("service").and_then(Value::as_str) == Some("lifecycle")
        && response
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok())
            == Some(expected_pid);
    if !identity_matches {
        return DaemonLifecycleEndpointObservation::Unavailable;
    }
    match response.get("readiness").and_then(Value::as_str) {
        Some("starting") => DaemonLifecycleEndpointObservation::Starting,
        Some("ready") => DaemonLifecycleEndpointObservation::Ready,
        _ => DaemonLifecycleEndpointObservation::Unavailable,
    }
}

fn daemon_applied_config_matches(status: &Value, expected: &DaemonConfigSnapshot) -> bool {
    let Some(applied) = status
        .get("config_reload")
        .and_then(|reload| reload.get("applied"))
    else {
        return false;
    };
    daemon_config_value_matches(applied, expected)
}

fn daemon_requested_config_matches(status: &Value, expected: &DaemonConfigSnapshot) -> bool {
    let Some(requested) = status
        .get("config_reload")
        .and_then(|reload| reload.get("requested"))
    else {
        return false;
    };
    daemon_config_value_matches(requested, expected)
}

fn daemon_config_value_matches(value: &Value, expected: &DaemonConfigSnapshot) -> bool {
    value.get("daemon_enabled").and_then(Value::as_bool) == Some(expected.enabled)
        && value.get("daemon_mode").and_then(Value::as_str) == Some(expected.mode.as_str())
        && value.get("semantic_enabled").and_then(Value::as_bool) == Some(expected.semantic_enabled)
        && value.get("semantic_executor").and_then(Value::as_str)
            == Some(expected.semantic_executor.as_str())
        && value
            .get("semantic_contract_fingerprint")
            .and_then(Value::as_str)
            == Some(expected.semantic_contract_fingerprint.as_str())
}

fn daemon_applied_degraded_semantic_config_matches(
    status: &Value,
    expected: &DaemonConfigSnapshot,
) -> bool {
    let Some(applied) = status
        .get("config_reload")
        .and_then(|reload| reload.get("applied"))
    else {
        return false;
    };
    applied.get("daemon_enabled").and_then(Value::as_bool) == Some(expected.enabled)
        && applied.get("daemon_mode").and_then(Value::as_str) == Some(expected.mode.as_str())
        && applied.get("semantic_enabled").and_then(Value::as_bool) == Some(false)
        && applied.get("semantic_executor").is_none_or(Value::is_null)
        && applied
            .get("semantic_contract_fingerprint")
            .is_none_or(Value::is_null)
}
