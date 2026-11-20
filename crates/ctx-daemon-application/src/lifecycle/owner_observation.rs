use super::*;

pub(super) fn read_daemon_owner_identity(data_root: &Path) -> Result<Option<DaemonOwnerIdentity>> {
    if !daemon_lock_is_active(data_root) {
        return Ok(None);
    }
    let Some(value) = read_pid_lock_json(&daemon_lock_path(data_root)) else {
        return Ok(None);
    };
    let Some(pid) = pid_from_lock_json(&value) else {
        return Ok(None);
    };
    let Some(owner_id) = value
        .get("owner_id")
        .and_then(Value::as_str)
        .filter(|owner_id| !owner_id.is_empty())
    else {
        return Ok(None);
    };
    let Some(started_at_ms) = value
        .get("started_at_ms")
        .and_then(Value::as_i64)
        .filter(|started_at_ms| *started_at_ms > 0)
    else {
        return Ok(None);
    };
    let binary_sha256 = value
        .get("binary_sha256")
        .and_then(Value::as_str)
        .filter(|digest| !digest.is_empty())
        .map(str::to_owned);
    let binary_metadata = daemon_lock_executable_metadata(&value);
    if (value.get("binary_metadata").is_some() && binary_metadata.is_none())
        || (binary_sha256.is_none() && binary_metadata.is_none())
    {
        return Ok(None);
    }
    if !daemon_lock_is_owned_by(data_root, pid) {
        return Ok(None);
    }
    Ok(Some(DaemonOwnerIdentity {
        owner_id: owner_id.to_owned(),
        pid,
        started_at_ms,
        binary_sha256,
        binary_metadata,
    }))
}

pub(super) fn handoff_health_probe_timeout(remaining: Duration, stalled_probes: usize) -> Duration {
    let target = match stalled_probes {
        0 => DAEMON_HEALTH_TIMEOUT,
        1 => DAEMON_HEALTH_TIMEOUT.saturating_mul(3),
        _ => DAEMON_RECOVERY_HEALTH_TIMEOUT,
    };
    remaining.min(target)
}

pub(super) fn owner_has_fresh_heartbeat_or_is_starting(
    status: Option<&Value>,
    owner: &DaemonOwnerIdentity,
    now_ms: i64,
) -> bool {
    let within_fresh_window = |timestamp_ms: i64| {
        timestamp_ms > 0
            && now_ms.saturating_sub(timestamp_ms) <= DAEMON_SETUP_HANDOFF_MAX_HEARTBEAT_AGE_MS
            && timestamp_ms.saturating_sub(now_ms) <= DAEMON_SETUP_HANDOFF_MAX_FUTURE_HEARTBEAT_MS
    };
    if let Some(status) = status {
        let status_pid = status
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok());
        let status_started_at_ms = status
            .get("started_at_ms")
            .and_then(Value::as_i64)
            .filter(|started_at_ms| *started_at_ms > 0);
        if status_pid == Some(owner.pid) && status_started_at_ms == Some(owner.started_at_ms) {
            if status.get("status").and_then(Value::as_str) != Some("running") {
                return false;
            }
            if status
                .pointer("/config_reload/status")
                .and_then(Value::as_str)
                == Some("pending")
            {
                return true;
            }
            return status
                .get("heartbeat_at_ms")
                .and_then(Value::as_i64)
                .is_some_and(within_fresh_window);
        }
    }
    within_fresh_window(owner.started_at_ms)
}

pub(super) fn probe_daemon_lifecycle_endpoint_with_retries(
    attempts: usize,
    mut probe: impl FnMut() -> DaemonLifecycleEndpointObservation,
    mut pause: impl FnMut(),
    checkpoint: &mut dyn FnMut() -> Result<()>,
) -> Result<bool> {
    for attempt in 0..attempts {
        checkpoint()?;
        if probe() != DaemonLifecycleEndpointObservation::Unavailable {
            return Ok(true);
        }
        if attempt + 1 < attempts {
            checkpoint()?;
            pause();
            checkpoint()?;
        }
    }
    Ok(false)
}

pub(super) fn recover_unusable_daemon_owner(
    host: &dyn DaemonApplicationHost,
    data_root: &Path,
    observed_owner: &DaemonOwnerIdentity,
    checkpoint: &mut dyn FnMut() -> Result<()>,
) -> Result<()> {
    checkpoint()?;
    let executable = daemon_autostart_exe()?;
    let terminated = recover_unusable_daemon_owner_guarded(
        observed_owner,
        |checkpoint| {
            probe_daemon_lifecycle_endpoint_with_retries(
                DAEMON_RECOVERY_HEALTH_ATTEMPTS,
                || {
                    daemon_lifecycle_endpoint_observation(
                        host,
                        data_root,
                        observed_owner.pid,
                        DAEMON_RECOVERY_HEALTH_TIMEOUT,
                    )
                },
                || std::thread::sleep(DAEMON_UPGRADE_POLL_INTERVAL),
                checkpoint,
            )
        },
        || read_daemon_owner_identity(data_root),
        || {
            owner_has_fresh_heartbeat_or_is_starting(
                read_daemon_status(data_root).as_ref(),
                observed_owner,
                utc_now().timestamp_millis(),
            )
        },
        |owner_id| {
            ctx_daemon_runtime::terminate_identity_verified_residual_daemon_owner(
                data_root,
                &executable,
                Some(owner_id),
            )
        },
        &mut *checkpoint,
    )?;
    if terminated {
        let deadline = Instant::now() + DAEMON_UPGRADE_RESTART_TIMEOUT;
        loop {
            checkpoint()?;
            if read_daemon_owner_identity(data_root)?.as_ref() != Some(observed_owner)
                || Instant::now() >= deadline
            {
                break;
            }
            checkpoint()?;
            std::thread::sleep(DAEMON_UPGRADE_POLL_INTERVAL);
            checkpoint()?;
        }
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn recover_unusable_daemon_owner_with(
    observed_owner: &DaemonOwnerIdentity,
    mut endpoint_usable: impl FnMut() -> Result<bool>,
    current_owner: impl FnMut() -> Result<Option<DaemonOwnerIdentity>>,
    terminate: impl FnMut(&str) -> Result<()>,
    checkpoint: impl FnMut() -> Result<()>,
) -> Result<bool> {
    recover_unusable_daemon_owner_guarded(
        observed_owner,
        |_| endpoint_usable(),
        current_owner,
        || false,
        terminate,
        checkpoint,
    )
}

pub(super) fn recover_unusable_daemon_owner_guarded(
    observed_owner: &DaemonOwnerIdentity,
    mut endpoint_usable: impl FnMut(&mut dyn FnMut() -> Result<()>) -> Result<bool>,
    mut current_owner: impl FnMut() -> Result<Option<DaemonOwnerIdentity>>,
    mut owner_is_alive_or_starting: impl FnMut() -> bool,
    mut terminate: impl FnMut(&str) -> Result<()>,
    mut checkpoint: impl FnMut() -> Result<()>,
) -> Result<bool> {
    checkpoint()?;
    if endpoint_usable(&mut checkpoint)? {
        return Ok(false);
    }
    checkpoint()?;
    // Revalidate full owner identity and liveness immediately before any
    // destructive action; never terminate a live owner with a fresh heartbeat
    // or active startup.
    if current_owner()?.as_ref() != Some(observed_owner)
        || observed_owner.binary_sha256.is_none()
        || owner_is_alive_or_starting()
    {
        return Ok(false);
    }
    checkpoint()?;
    terminate(&observed_owner.owner_id)?;
    checkpoint()?;
    Ok(true)
}

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
    daemon_lock_matches_executable_metadata(data_root, &daemon_autostart_exe()?)
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
    match ctx_daemon_runtime::daemon_owner_metadata_identity_matches(&lock, &executable) {
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
        daemon_lock_metadata_identity_matches, daemon_query_roundtrip_linux_owner,
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
        || !daemon_lock_metadata_identity_matches(&lock, executable)?
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
