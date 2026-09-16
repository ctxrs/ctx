use super::*;

/// Returns whether a live daemon is already owned by this exact executable.
///
/// Ordinary foreground commands use this to reuse a healthy installed daemon
/// without reconciling native supervision from the invoking shell's ambient
/// environment. Explicit setup and binary-mismatch repair still follow the
/// full supervisor handoff path.
pub fn active_daemon_matches_current_executable(data_root: &Path) -> Result<bool> {
    if !daemon_lock_is_active(data_root) {
        return Ok(false);
    }
    daemon_lock_matches_executable(data_root, &daemon_autostart_exe()?)
}

/// A Core request may reuse a responsive existing owner without reinstalling
/// supervision. This grants no native image-verification or upgrade authority.
pub fn observe_ready_core_daemon(
    host: &dyn DaemonApplicationHost,
    data_root: &Path,
    config: &DaemonConfigSnapshot,
) -> Result<Option<DaemonHandoff>> {
    if !config.enabled || !active_daemon_matches_current_executable(data_root)? {
        return Ok(None);
    }
    let Some(lock) = read_pid_lock_json(&daemon_lock_path(data_root)) else {
        return Ok(None);
    };
    let executable = daemon_autostart_exe()?;
    match ctx_daemon_runtime::daemon_owner_binary_identity_matches(&lock, &executable) {
        Ok(true) => {}
        Ok(false) => return Err(binary_identity_handoff_error()),
        #[cfg(target_os = "linux")]
        Err(error) if error.is::<ctx_daemon_runtime::ProcessExecutableInspectionDenied>() => {
            let denied = error
                .downcast_ref::<ctx_daemon_runtime::ProcessExecutableInspectionDenied>()
                .expect("typed denied inspection");
            if verify_inspection_denied_owner(data_root, &executable, denied).is_err() {
                return Ok(None);
            }
        }
        _ => return Ok(None),
    }
    let observation = daemon_handoff_observation(
        host,
        data_root,
        None,
        config,
        DaemonReadinessRequirement::Core,
        DAEMON_HEALTH_TIMEOUT,
    );
    if read_pid_lock_json(&daemon_lock_path(data_root)).as_ref() != Some(&lock)
        || !active_daemon_matches_current_executable(data_root)?
    {
        return Ok(None);
    }
    Ok(match observation {
        DaemonHandoffObservation::Running(handoff) => Some(handoff),
        _ => None,
    })
}

/// Linux may deny /proc/PID/exe while the same-user daemon remains usable.
/// This alternative proves live ownership, not executable-image equality, and
/// must never be used to authorize forced termination or executable replacement.
#[cfg(target_os = "linux")]
pub(crate) fn verify_inspection_denied_owner(
    data_root: &Path,
    executable: &Path,
    denied: &ctx_daemon_runtime::ProcessExecutableInspectionDenied,
) -> Result<u32> {
    use ctx_daemon_runtime::{
        daemon_lock_binary_identity_matches, daemon_query_roundtrip_linux_owner,
        observe_pid_advisory_lock, process_state, DaemonQueryEndpoint, ProcessState,
    };
    let lock_path = daemon_lock_path(data_root);
    let lock = read_pid_lock_json(&lock_path)
        .ok_or_else(|| anyhow!("daemon ownership is unavailable during inspection"))?;
    let owner = read_daemon_owner_identity(data_root)?
        .ok_or_else(|| anyhow!("daemon has no live owner during inspection"))?;
    let held = || {
        observe_pid_advisory_lock(&lock_path)
            .is_some_and(|observation| observation.held && !observation.released)
    };
    if owner.pid != denied.pid
        || !held()
        || lock.get("lock_protocol").and_then(Value::as_str)
            != Some(ctx_daemon_runtime::PID_LOCK_PROTOCOL)
        || process_state(owner.pid) != ProcessState::Running
        || !daemon_lock_binary_identity_matches(&lock, executable)?
        || lock
            .get("data_root")
            .and_then(Value::as_str)
            .map(Path::new)
            .and_then(|root| fs::canonicalize(root).ok())
            != Some(fs::canonicalize(data_root)?)
    {
        return Err(anyhow!("daemon ownership changed or cannot be verified"));
    }
    let endpoint = ctx_daemon_service::read_daemon_service_endpoint_identity(
        data_root,
        ctx_daemon_service::DaemonIpcService::SourceRefresh,
    )?
    .ok_or_else(|| anyhow!("daemon has no authenticated lifecycle endpoint"))?;
    if endpoint.owner_pid != owner.pid {
        return Err(anyhow!("daemon lifecycle endpoint has a different owner"));
    }
    let DaemonQueryEndpoint::Unix { path, token } = &endpoint.endpoint;
    let request = format!(
        "{}\n",
        json!({
            "schema_version": 1, "op": "lifecycle_ping", "token": token,
        })
    );
    let response = daemon_query_roundtrip_linux_owner(
        path,
        owner.pid,
        request.as_bytes(),
        DAEMON_HEALTH_TIMEOUT,
        DAEMON_HEALTH_RESPONSE_MAX_BYTES,
    )?;
    let response: Value = serde_json::from_slice(&response)?;
    if daemon_lifecycle_response_observation(&response, owner.pid)
        == DaemonLifecycleEndpointObservation::Unavailable
        || read_pid_lock_json(&lock_path).as_ref() != Some(&lock)
        || read_daemon_owner_identity(data_root)?.as_ref() != Some(&owner)
        || !held()
    {
        return Err(anyhow!(
            "daemon lifecycle response did not verify stable ownership"
        ));
    }
    Ok(owner.pid)
}
