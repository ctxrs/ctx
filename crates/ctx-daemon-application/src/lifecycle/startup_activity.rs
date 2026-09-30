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

#[cfg(test)]
mod tests;
