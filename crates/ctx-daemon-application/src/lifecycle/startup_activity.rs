use super::*;
use ctx_daemon_runtime::observe_process_cpu;

/// Startup may spend time recovering Core before an IPC endpoint exists.
/// A live owner extends observation only; the lifecycle endpoint still owns
/// readiness. The caller checks the exact child's exit and cancellation each
/// iteration. An I/O wait or unchanged CPU counters do not establish failure.
pub(super) fn observe(
    observation: DaemonHandoffObservation,
    data_root: &Path,
    child_unreaped: bool,
    config: &DaemonConfigSnapshot,
) -> DaemonHandoffObservation {
    if observation == DaemonHandoffObservation::Pending
        && (child_unreaped || pending_owner_identity(data_root, config).is_some())
    {
        DaemonHandoffObservation::Starting
    } else {
        observation
    }
}

fn pending_owner_identity(
    data_root: &Path,
    config: &DaemonConfigSnapshot,
) -> Option<DaemonOwnerIdentity> {
    let owner = read_daemon_owner_identity(data_root).ok()??;
    let status = read_daemon_status(data_root)?;
    if status["status"] != "running"
        || status["pid"].as_u64() != Some(u64::from(owner.pid))
        || status["started_at_ms"].as_i64() != Some(owner.started_at_ms)
        || status["config_reload"]["status"] != "pending"
        || !readiness_receipt::daemon_requested_config_matches(&status, config)
    {
        return None;
    }
    let lock = read_pid_lock_json(&daemon_lock_path(data_root))?;
    // Reuse native process-birth inspection to authenticate the lock token.
    // Its CPU counters have no bearing on whether startup may keep waiting.
    let process = observe_process_cpu(owner.pid).ok()?;
    (process
        .identity
        .matches_private_json_token(&lock["process_creation_token"])
        && read_daemon_owner_identity(data_root).ok()?.as_ref() == Some(&owner))
    .then_some(owner)
}

#[derive(Default)]
pub(super) struct StartupActivityTracker {
    last_owner_heartbeat: Option<(DaemonOwnerIdentity, i64)>,
}

impl StartupActivityTracker {
    pub(super) fn observe(
        &mut self,
        observation: DaemonHandoffObservation,
        data_root: &Path,
        child_unreaped: bool,
        config: &DaemonConfigSnapshot,
        readiness: DaemonReadinessRequirement,
    ) -> DaemonHandoffObservation {
        let observed = observe(observation, data_root, child_unreaped, config);
        if observed == DaemonHandoffObservation::Pending
            && (cold_start_owner_within_window(data_root, utc_now().timestamp_millis())
                || self.observe_advancing_owner_heartbeat(data_root, config, readiness))
        {
            DaemonHandoffObservation::Starting
        } else {
            observed
        }
    }

    fn observe_advancing_owner_heartbeat(
        &mut self,
        data_root: &Path,
        config: &DaemonConfigSnapshot,
        readiness: DaemonReadinessRequirement,
    ) -> bool {
        let Some((owner, heartbeat_at_ms)) = live_owner_heartbeat(data_root, config, readiness)
        else {
            self.last_owner_heartbeat = None;
            return false;
        };
        match &mut self.last_owner_heartbeat {
            Some((previous_owner, previous_heartbeat)) if *previous_owner == owner => {
                if heartbeat_at_ms > *previous_heartbeat {
                    *previous_heartbeat = heartbeat_at_ms;
                    true
                } else {
                    false
                }
            }
            slot => {
                *slot = Some((owner, heartbeat_at_ms));
                false
            }
        }
    }
}

fn live_owner_heartbeat(
    data_root: &Path,
    config: &DaemonConfigSnapshot,
    readiness: DaemonReadinessRequirement,
) -> Option<(DaemonOwnerIdentity, i64)> {
    let owner = read_daemon_owner_identity(data_root).ok()??;
    let status = read_daemon_status(data_root)?;
    let now_ms = utc_now().timestamp_millis();
    let DaemonHandoffObservation::Running(handoff) = daemon_handoff_status_observation_from(
        Some(&status),
        Some(&owner),
        None,
        config,
        readiness,
        now_ms,
    ) else {
        return None;
    };
    let heartbeat = handoff.heartbeat_at_ms;
    if heartbeat <= 0
        || now_ms.saturating_sub(heartbeat) > DAEMON_SETUP_HANDOFF_MAX_HEARTBEAT_AGE_MS
        || heartbeat.saturating_sub(now_ms) > DAEMON_SETUP_HANDOFF_MAX_FUTURE_HEARTBEAT_MS
    {
        return None;
    }
    let lock = read_pid_lock_json(&daemon_lock_path(data_root))?;
    let process = observe_process_cpu(owner.pid).ok()?;
    (process
        .identity
        .matches_private_json_token(&lock["process_creation_token"])
        && read_daemon_owner_identity(data_root).ok()?.as_ref() == Some(&owner))
    .then_some((owner, heartbeat))
}

fn cold_start_owner_within_window(data_root: &Path, now_ms: i64) -> bool {
    let Ok(Some(owner)) = read_daemon_owner_identity(data_root) else {
        return false;
    };
    if owner.started_at_ms <= 0
        || now_ms.saturating_sub(owner.started_at_ms) > DAEMON_SETUP_HANDOFF_MAX_HEARTBEAT_AGE_MS
        || owner.started_at_ms.saturating_sub(now_ms) > DAEMON_SETUP_HANDOFF_MAX_FUTURE_HEARTBEAT_MS
    {
        return false;
    }
    if let Some(status) = read_daemon_status(data_root) {
        let status_matches_owner = status.get("pid").and_then(Value::as_u64)
            == Some(u64::from(owner.pid))
            && status.get("started_at_ms").and_then(Value::as_i64) == Some(owner.started_at_ms);
        if status_matches_owner {
            return false;
        }
    }
    let Some(lock) = read_pid_lock_json(&daemon_lock_path(data_root)) else {
        return false;
    };
    let Ok(process) = observe_process_cpu(owner.pid) else {
        return false;
    };
    process
        .identity
        .matches_private_json_token(&lock["process_creation_token"])
        && read_daemon_owner_identity(data_root)
            .ok()
            .flatten()
            .as_ref()
            == Some(&owner)
}

#[cfg(test)]
mod tests;
