use super::*;
use std::fmt;

pub(crate) fn maybe_autostart_daemon(
    data_root: &Path,
    config: &AppConfig,
    trigger: DaemonTriggerCommandArg,
) {
    maybe_autostart_daemon_inner(data_root, config, trigger);
}

pub(crate) fn daemon_autostart_suppression_reason() -> Option<&'static str> {
    if semantic_env_flag(DAEMON_BACKGROUND_CHILD_ENV) {
        Some("daemon_child")
    } else if semantic_env_flag("CI") {
        Some("ci")
    } else if semantic_env_flag(DAEMON_AUTOSTART_OFF_ENV) {
        Some("autostart_disabled")
    } else {
        None
    }
}

pub(crate) fn autostart_daemon_and_wait(
    data_root: &Path,
    config: &AppConfig,
    trigger: DaemonTriggerCommandArg,
) -> Result<DaemonHandoff> {
    if daemon_autostart_suppression_reason().is_none() {
        super::super::daemon_supervisor::ensure_daemon_supervisor(data_root)
            .context("establish persistent ctx daemon supervision")?;
    }
    let mut recovery_attempted = false;
    loop {
        let request = request_daemon_autostart(data_root, config, trigger).map_err(|error| {
            if error.is::<BinaryIdentityHandoffError>() {
                error
            } else {
                anyhow!(
                    "ctx daemon did not start: {error:#}. Run `ctx daemon status --format json`, then `ctx daemon run` for details"
                )
            }
        })?;
        let (mut child, pending_restart_request, existing_owner) = match request {
            DaemonAutostartRequest::Existing(owner) => (None, None, Some(owner)),
            DaemonAutostartRequest::Deferred(path) => (None, Some(path), None),
            DaemonAutostartRequest::Spawned(child) => (Some(child), None, None),
            DaemonAutostartRequest::Suppressed(reason) => {
                return Err(anyhow!(
                    "ctx daemon start was suppressed ({reason}); retry after it clears or run `ctx setup --no-daemon`"
                ));
            }
        };
        let expected_failure_pid = child.as_ref().map(Child::id);
        let deadline = Instant::now() + DAEMON_SETUP_HANDOFF_TIMEOUT;
        let handoff = wait_for_daemon_handoff_with(
            DAEMON_SETUP_HANDOFF_POLL_ATTEMPTS,
            || {
                if pending_restart_request
                    .as_ref()
                    .is_some_and(|path| path.exists())
                {
                    DaemonHandoffObservation::Pending
                } else {
                    daemon_handoff_observation(
                        data_root,
                        expected_failure_pid,
                        config,
                        deadline
                            .saturating_duration_since(Instant::now())
                            .min(DAEMON_HEALTH_TIMEOUT),
                    )
                }
            },
            || {
                let Some(child) = child.as_mut() else {
                    return Ok(None);
                };
                let Some(exit) = child.try_wait()? else {
                    return Ok(None);
                };
                let detail = read_daemon_status(data_root)
                    .and_then(|status| {
                        (status.get("pid").and_then(Value::as_u64) == Some(u64::from(child.id())))
                            .then(|| {
                                status
                                    .get("last_error")
                                    .and_then(Value::as_str)
                                    .map(str::to_owned)
                            })
                            .flatten()
                    })
                    .unwrap_or_else(|| format!("daemon process exited with {exit}"));
                Ok(Some(detail))
            },
            || {
                std::thread::sleep(
                    DAEMON_UPGRADE_POLL_INTERVAL
                        .min(deadline.saturating_duration_since(Instant::now())),
                )
            },
        );
        match handoff {
            Ok(handoff) => return Ok(handoff),
            Err(error)
                if !recovery_attempted
                    && daemon_autostart_suppression_reason().is_none()
                    && error.is::<DaemonHandoffTimeout>()
                    && existing_owner.is_some() =>
            {
                let Some(existing_owner) = existing_owner.as_ref() else {
                    return Err(anyhow!("daemon owner identity disappeared before recovery"));
                };
                recover_unusable_daemon_owner(data_root, existing_owner)?;
                recovery_attempted = true;
            }
            Err(error) => {
                return Err(anyhow!(
                    "ctx daemon did not become ready: {error}. Run `ctx daemon status --format json`, then `ctx daemon run` for details"
                ));
            }
        }
    }
}

fn read_daemon_owner_identity(data_root: &Path) -> Result<Option<DaemonOwnerIdentity>> {
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
    let Some(binary_sha256) = value
        .get("binary_sha256")
        .and_then(Value::as_str)
        .filter(|digest| !digest.is_empty())
    else {
        return Ok(None);
    };
    if !daemon_lock_is_owned_by(data_root, pid) {
        return Ok(None);
    }
    Ok(Some(DaemonOwnerIdentity {
        owner_id: owner_id.to_owned(),
        pid,
        started_at_ms,
        binary_sha256: binary_sha256.to_owned(),
    }))
}

fn daemon_owner_has_active_refresh(data_root: &Path, observed_owner: &DaemonOwnerIdentity) -> bool {
    let status = read_daemon_status(data_root);
    let refresh_job = read_daemon_job_status(&daemon_core_refresh_job_path(data_root));
    daemon_owned_source_refresh_is_active(
        status.as_ref(),
        refresh_job.as_ref(),
        Some(observed_owner.pid),
        utc_now().timestamp_millis(),
    )
}

fn recover_unusable_daemon_owner(
    data_root: &Path,
    observed_owner: &DaemonOwnerIdentity,
) -> Result<()> {
    let executable = daemon_autostart_exe()?;
    let terminated = recover_unusable_daemon_owner_with(
        observed_owner,
        || read_daemon_owner_identity(data_root),
        || daemon_owner_has_active_refresh(data_root, observed_owner),
        || {
            Ok(daemon_source_refresh_endpoint_is_usable(
                data_root,
                observed_owner.pid,
                DAEMON_HEALTH_TIMEOUT,
            ))
        },
        |owner_id| {
            super::handoff::terminate_identity_verified_residual_daemon_owner(
                data_root,
                &executable,
                Some(owner_id),
            )
        },
    )?;
    if terminated {
        let deadline = Instant::now() + DAEMON_UPGRADE_RESTART_TIMEOUT;
        while read_daemon_owner_identity(data_root)?.as_ref() == Some(observed_owner)
            && Instant::now() < deadline
        {
            std::thread::sleep(DAEMON_UPGRADE_POLL_INTERVAL);
        }
    }
    Ok(())
}

pub(super) fn recover_unusable_daemon_owner_with(
    observed_owner: &DaemonOwnerIdentity,
    mut current_owner: impl FnMut() -> Result<Option<DaemonOwnerIdentity>>,
    mut active_refresh: impl FnMut() -> bool,
    mut endpoint_usable: impl FnMut() -> Result<bool>,
    mut terminate: impl FnMut(&str) -> Result<()>,
) -> Result<bool> {
    if current_owner()?.as_ref() != Some(observed_owner) || active_refresh() {
        return Ok(false);
    }
    if endpoint_usable()? {
        return Ok(false);
    }
    // The health probe can race a supervisor or another foreground recovery.
    // Revalidate the complete advisory-lock owner identity after the bounded
    // probe, and re-check active work, before any destructive action.
    if current_owner()?.as_ref() != Some(observed_owner) || active_refresh() {
        return Ok(false);
    }
    terminate(&observed_owner.owner_id)?;
    Ok(true)
}

pub(super) fn request_daemon_autostart(
    data_root: &Path,
    config: &AppConfig,
    trigger: DaemonTriggerCommandArg,
) -> Result<DaemonAutostartRequest> {
    // Suppression disables spawning, not reuse. Test harnesses and managed
    // callers can intentionally provide an already-owned daemon while
    // forbidding any additional detached process.
    if config.daemon.enabled && daemon_lock_is_active(data_root) {
        let executable = daemon_autostart_exe()?;
        if daemon_lock_matches_executable(data_root, &executable)? {
            return Ok(DaemonAutostartRequest::Existing(
                read_daemon_owner_identity(data_root)?.ok_or_else(|| {
                    anyhow!("active ctx daemon lock has no stable owner identity")
                })?,
            ));
        }
        if daemon_autostart_suppression_reason().is_some() {
            return Err(binary_identity_handoff_error());
        }
        handoff_mismatched_daemon_owner(data_root, &executable)?;
        if daemon_lock_is_active(data_root) {
            return Ok(DaemonAutostartRequest::Existing(
                read_daemon_owner_identity(data_root)?.ok_or_else(|| {
                    anyhow!("active replacement ctx daemon has no stable owner identity")
                })?,
            ));
        }
    }
    if let Some(reason) = daemon_autostart_suppression_reason() {
        return Ok(DaemonAutostartRequest::Suppressed(reason));
    }
    if !daemon_autostart_allowed(data_root, config) {
        return Ok(DaemonAutostartRequest::Suppressed("not_allowed"));
    }
    if crate::upgrade::installation_upgrade_is_active().unwrap_or(false) {
        return Ok(DaemonAutostartRequest::Suppressed(
            "installation_upgrade_active",
        ));
    }
    if daemon_upgrade_handoff_is_active(data_root) {
        let request =
            write_daemon_restart_request(data_root, trigger, &Uuid::now_v7().to_string())?;
        return Ok(DaemonAutostartRequest::Deferred(request));
    }
    let lock_path = daemon_lock_path(data_root);
    if lock_path.exists() && !daemon_lock_is_stale(&lock_path) {
        let executable = daemon_autostart_exe()?;
        handoff_mismatched_daemon_owner(data_root, &executable)?;
        if daemon_lock_is_active(data_root) {
            return Ok(DaemonAutostartRequest::Existing(
                read_daemon_owner_identity(data_root)?.ok_or_else(|| {
                    anyhow!("active replacement ctx daemon has no stable owner identity")
                })?,
            ));
        }
    }
    let exe = match daemon_autostart_exe() {
        Ok(exe) => exe,
        Err(error) => {
            let _ = write_daemon_autostart_status(
                data_root,
                trigger,
                "failed",
                Some("current_exe"),
                Some(format!("{error:#}")),
                None,
            );
            return Err(error);
        }
    };
    let mut command = configured_daemon_autostart_command(&exe, data_root, trigger, None);
    match spawn_daemon_child(&mut command) {
        Ok(child) => Ok(DaemonAutostartRequest::Spawned(child)),
        Err(error) => {
            let _ = write_daemon_autostart_status(
                data_root,
                trigger,
                "failed",
                Some("spawn_failed"),
                Some(error.to_string()),
                None,
            );
            Err(error).context("spawn ctx daemon")
        }
    }
}

pub(in crate::semantic) fn handoff_mismatched_daemon_owner(
    data_root: &Path,
    expected_executable: &Path,
) -> Result<()> {
    if !daemon_lock_is_active(data_root)
        || daemon_lock_matches_executable(data_root, expected_executable)?
    {
        return Ok(());
    }
    let expected_canonical = fs::canonicalize(expected_executable)
        .with_context(|| format!("resolve ctx executable {}", expected_executable.display()))?;
    let expected_sha256 = executable_sha256(expected_executable)?;
    let owner_pid = read_pid_lock_json(&daemon_lock_path(data_root))
        .as_ref()
        .and_then(pid_from_lock_json)
        .ok_or_else(binary_identity_handoff_error)?;
    let response = daemon_source_refresh_request(
        data_root,
        compact_json(json!({
            "schema_version": 1,
            "op": "supervisor_handoff",
        })),
        DAEMON_HEALTH_TIMEOUT,
        DAEMON_HEALTH_RESPONSE_MAX_BYTES,
    )
    .map_err(|_| binary_identity_handoff_error())?;
    let accepted = response.as_ref().is_some_and(|value| {
        value.get("ok").and_then(Value::as_bool) == Some(true)
            && value
                .get("pid")
                .and_then(Value::as_u64)
                .and_then(|pid| u32::try_from(pid).ok())
                == Some(owner_pid)
    });
    if accepted {
        let deadline = Instant::now() + DAEMON_UPGRADE_STOP_TIMEOUT;
        while daemon_lock_is_active(data_root) {
            if daemon_lock_matches_cached_identity(data_root, &expected_canonical, &expected_sha256)
            {
                return Ok(());
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(DAEMON_UPGRADE_POLL_INTERVAL);
        }
    }
    if daemon_lock_is_active(data_root) {
        super::handoff::terminate_identity_verified_residual_daemon(data_root, expected_executable)
            .map_err(|_| binary_identity_handoff_error())?;
    }
    let deadline = Instant::now() + DAEMON_UPGRADE_RESTART_TIMEOUT;
    while daemon_lock_is_active(data_root) {
        if Instant::now() >= deadline {
            return Err(binary_identity_handoff_error());
        }
        std::thread::sleep(DAEMON_UPGRADE_POLL_INTERVAL);
    }
    Ok(())
}

fn daemon_lock_matches_cached_identity(
    data_root: &Path,
    expected_canonical: &Path,
    expected_sha256: &str,
) -> bool {
    read_pid_lock_json(&daemon_lock_path(data_root)).is_some_and(|value| {
        value
            .get("binary")
            .and_then(Value::as_str)
            .map(Path::new)
            .and_then(|path| fs::canonicalize(path).ok())
            .as_deref()
            == Some(expected_canonical)
            && value.get("binary_sha256").and_then(Value::as_str) == Some(expected_sha256)
    })
}

fn binary_identity_handoff_error() -> anyhow::Error {
    BinaryIdentityHandoffError.into()
}

#[derive(Debug)]
struct BinaryIdentityHandoffError;

impl fmt::Display for BinaryIdentityHandoffError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "a live ctx daemon is owned by a different binary image; run `ctx daemon disable --prepare-uninstall`, then retry",
        )
    }
}

impl std::error::Error for BinaryIdentityHandoffError {}

fn daemon_handoff_observation(
    data_root: &Path,
    expected_failure_pid: Option<u32>,
    expected_config: &AppConfig,
    health_timeout: StdDuration,
) -> DaemonHandoffObservation {
    let status = read_daemon_status(data_root);
    let lock_pid = super::super::paths_status::read_pid_lock_file(&daemon_lock_path(data_root));
    let lock_active = lock_pid.is_some_and(|pid| daemon_lock_is_owned_by(data_root, pid));
    let now_ms = utc_now().timestamp_millis();
    let observation = daemon_handoff_observation_from(
        status.as_ref(),
        lock_pid,
        lock_active,
        expected_failure_pid,
        Some(expected_config),
        now_ms,
    );
    let refresh_job = read_daemon_job_status(&daemon_core_refresh_job_path(data_root));
    let active_refresh = daemon_owned_source_refresh_is_active(
        status.as_ref(),
        refresh_job.as_ref(),
        lock_pid,
        now_ms,
    );
    // The daemon's first scheduler tick can enter a long synchronous refresh
    // before setup gets an endpoint response. The base observation has already
    // verified fresh status, lock ownership, and applied config; a refresh
    // started by that same daemon is sufficient bounded handoff evidence.
    if matches!(observation, DaemonHandoffObservation::Running(_)) && active_refresh {
        return observation;
    }
    let endpoint_usable = lock_active
        && !health_timeout.is_zero()
        && lock_pid.is_some_and(|pid| {
            daemon_source_refresh_endpoint_is_usable(data_root, pid, health_timeout)
        });
    complete_daemon_handoff_observation(
        observation,
        status.as_ref(),
        lock_pid,
        lock_active,
        expected_config,
        endpoint_usable,
        active_refresh,
    )
}

pub(super) fn complete_daemon_handoff_observation(
    observation: DaemonHandoffObservation,
    status: Option<&Value>,
    lock_pid: Option<u32>,
    lock_active: bool,
    expected_config: &AppConfig,
    endpoint_usable: bool,
    active_refresh: bool,
) -> DaemonHandoffObservation {
    match observation {
        DaemonHandoffObservation::Running(handoff) if endpoint_usable || active_refresh => {
            DaemonHandoffObservation::Running(handoff)
        }
        DaemonHandoffObservation::Running(_) => DaemonHandoffObservation::Pending,
        DaemonHandoffObservation::Pending if lock_active && endpoint_usable => {
            daemon_live_endpoint_observation_from(status, lock_pid, expected_config)
        }
        observation => observation,
    }
}

pub(super) fn daemon_owned_source_refresh_is_active(
    daemon_status: Option<&Value>,
    refresh_job: Option<&Value>,
    lock_pid: Option<u32>,
    now_ms: i64,
) -> bool {
    let Some((daemon_status, refresh_job)) = daemon_status.zip(refresh_job) else {
        return false;
    };
    let daemon_started_at_ms = daemon_status
        .get("started_at_ms")
        .and_then(Value::as_i64)
        .filter(|started_at_ms| *started_at_ms > 0);
    let job_started_at_ms = refresh_job
        .get("last_run_at_ms")
        .and_then(Value::as_i64)
        .filter(|started_at_ms| {
            // Reject a stale job inherited from an earlier daemon owner and
            // timestamps too far in the future to be credible.
            daemon_started_at_ms.is_some_and(|daemon_started| *started_at_ms >= daemon_started)
                && *started_at_ms
                    <= now_ms.saturating_add(DAEMON_SETUP_HANDOFF_MAX_FUTURE_HEARTBEAT_MS)
        });
    daemon_status
        .get("pid")
        .and_then(Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
        == lock_pid
        && daemon_started_at_ms.is_some()
        && job_started_at_ms.is_some()
        && refresh_job.get("owner").and_then(Value::as_str) == Some("daemon")
        && refresh_job.get("kind").and_then(Value::as_str) == Some("source_backed")
        && refresh_job.get("status").and_then(Value::as_str) == Some("running")
        && refresh_job.get("request_state").and_then(Value::as_str) == Some("running")
        && refresh_job
            .get("request_id")
            .and_then(Value::as_str)
            .is_some_and(|request_id| !request_id.is_empty())
        && refresh_job
            .get("progress")
            .and_then(|progress| progress.get("phase"))
            .and_then(Value::as_str)
            .is_some_and(|phase| !phase.is_empty() && phase != "failed")
        && refresh_job.get("last_error").is_none_or(Value::is_null)
}

pub(super) fn daemon_live_endpoint_observation_from(
    status: Option<&Value>,
    lock_pid: Option<u32>,
    expected_config: &AppConfig,
) -> DaemonHandoffObservation {
    let Some(status) = status else {
        return DaemonHandoffObservation::Pending;
    };
    let status_pid = status
        .get("pid")
        .and_then(Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok());
    if status.get("status").and_then(Value::as_str) != Some("running")
        || status_pid.is_none()
        || status_pid != lock_pid
    {
        return DaemonHandoffObservation::Pending;
    }
    match status
        .get("config_reload")
        .and_then(|reload| reload.get("status"))
        .and_then(Value::as_str)
    {
        Some("failed" | "activation_failed") => {
            let error = status
                .get("config_reload")
                .and_then(|reload| reload.get("last_error"))
                .and_then(Value::as_str)
                .or_else(|| status.get("last_error").and_then(Value::as_str))
                .unwrap_or("daemon configuration failed");
            return DaemonHandoffObservation::Failed(error.to_owned());
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
        pid: status_pid.unwrap_or_default(),
        heartbeat_at_ms,
    })
}

fn daemon_source_refresh_endpoint_is_usable(
    data_root: &Path,
    expected_pid: u32,
    timeout: StdDuration,
) -> bool {
    daemon_source_refresh_request(
        data_root,
        compact_json(json!({
            "schema_version": 1,
            "op": "ping",
        })),
        timeout,
        DAEMON_HEALTH_RESPONSE_MAX_BYTES,
    )
    .ok()
    .flatten()
    .is_some_and(|response| {
        response.get("ok").and_then(Value::as_bool) == Some(true)
            && response
                .get("pid")
                .and_then(Value::as_u64)
                .and_then(|pid| u32::try_from(pid).ok())
                == Some(expected_pid)
    })
}

pub(super) fn daemon_handoff_observation_from(
    status: Option<&Value>,
    lock_pid: Option<u32>,
    lock_active: bool,
    expected_failure_pid: Option<u32>,
    expected_config: Option<&AppConfig>,
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
        let belongs_to_request = expected_failure_pid
            .map(|expected| status_pid == Some(expected))
            .unwrap_or_else(|| lock_active && status_pid.is_some() && status_pid == lock_pid);
        if belongs_to_request && heartbeat_is_fresh() {
            return DaemonHandoffObservation::Failed(last_error());
        }
        return DaemonHandoffObservation::Pending;
    }
    if status_name != Some("running")
        || !lock_active
        || status_pid.is_none()
        || status_pid != lock_pid
    {
        return DaemonHandoffObservation::Pending;
    }
    if !heartbeat_is_fresh() {
        return DaemonHandoffObservation::Pending;
    }
    match status
        .get("config_reload")
        .and_then(|reload| reload.get("status"))
        .and_then(Value::as_str)
    {
        Some("failed" | "activation_failed") => {
            let error = status
                .get("config_reload")
                .and_then(|reload| reload.get("last_error"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(last_error);
            return DaemonHandoffObservation::Failed(error);
        }
        Some("pending") => return DaemonHandoffObservation::Pending,
        Some("applied") => {
            if expected_config
                .is_some_and(|expected| !daemon_applied_config_matches(status, expected))
            {
                return DaemonHandoffObservation::Pending;
            }
        }
        None if expected_config.is_none() => {}
        None => return DaemonHandoffObservation::Pending,
        Some(_) => return DaemonHandoffObservation::Pending,
    }
    let Some(heartbeat_at_ms) = status
        .get("heartbeat_at_ms")
        .and_then(Value::as_i64)
        .filter(|_| heartbeat_is_fresh())
    else {
        return DaemonHandoffObservation::Pending;
    };
    DaemonHandoffObservation::Running(DaemonHandoff {
        pid: status_pid.unwrap_or_default(),
        heartbeat_at_ms,
    })
}

fn daemon_applied_config_matches(status: &Value, expected: &AppConfig) -> bool {
    let Some(applied) = status
        .get("config_reload")
        .and_then(|reload| reload.get("applied"))
    else {
        return false;
    };
    applied.get("daemon_enabled").and_then(Value::as_bool) == Some(expected.daemon.enabled)
        && applied.get("daemon_mode").and_then(Value::as_str) == Some(expected.daemon.mode.as_str())
        && applied.get("semantic_enabled").and_then(Value::as_bool)
            == Some(expected.semantic_search_enabled())
}

pub(super) fn wait_for_daemon_handoff_with(
    attempts: usize,
    mut observe: impl FnMut() -> DaemonHandoffObservation,
    mut child_failure: impl FnMut() -> Result<Option<String>>,
    mut pause: impl FnMut(),
) -> Result<DaemonHandoff> {
    for attempt in 0..attempts {
        match observe() {
            DaemonHandoffObservation::Running(handoff) => return Ok(handoff),
            DaemonHandoffObservation::Failed(error) => return Err(anyhow!(error)),
            DaemonHandoffObservation::Pending => {}
        }
        if let Some(error) = child_failure()? {
            return Err(anyhow!(error));
        }
        if attempt + 1 < attempts {
            pause();
        }
    }
    Err(DaemonHandoffTimeout.into())
}

pub(super) fn daemon_autostart_command(
    exe: &Path,
    data_root: &Path,
    trigger: DaemonTriggerCommandArg,
    idle_exit: Option<u64>,
    loop_interval: Option<u64>,
    handoff_token: Option<&str>,
) -> Command {
    let mut command = Command::new(exe);
    configure_narrow_daemon_environment(&mut command);
    command
        .arg("--data-root")
        .arg(data_root)
        .arg("daemon")
        .arg("run")
        .arg("--start-mode")
        .arg(DaemonStartModeArg::Auto.as_str())
        .arg("--trigger-command")
        .arg(trigger.as_str())
        .arg("--format=json")
        .env(DAEMON_BACKGROUND_CHILD_ENV, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(idle_exit) = idle_exit {
        command
            .arg("--idle-exit-seconds")
            .arg(idle_exit.to_string());
    }
    if let Some(loop_interval) = loop_interval {
        command
            .arg("--loop-interval-seconds")
            .arg(loop_interval.to_string());
    }
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    #[cfg(windows)]
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    if let Some(token) = handoff_token {
        command.env(DAEMON_UPGRADE_HANDOFF_TOKEN_ENV, token);
    }
    command
}

const DAEMON_CHILD_ENV_ALLOWLIST: &[&str] = &[
    "APPDATA",
    "ASTRBOT_ROOT",
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
    "COPILOT_HOME",
    "CTX_ANALYTICS_DEBUG",
    "CTX_ANALYTICS_DRY_RUN",
    "CTX_ANALYTICS_ENABLED",
    "CTX_ANALYTICS_ENDPOINT",
    "CTX_HISTORY_PLUGIN_PATH",
    "CTX_LOCAL_USAGE_ENABLED",
    "CTX_MACHINE_ID",
    "CTX_PRO_CHANNEL",
    "CTX_PRO_HELPER",
    "CTX_RUNTIME_DIR",
    "CTX_SEARCH_SEMANTIC",
    "CTX_SEMANTIC_COREML_NATIVE_COMPUTE",
    "CTX_SEMANTIC_MODEL_ONNX",
    "CTX_UPGRADE_AUTO",
    "CTX_UPGRADE_CHANNEL",
    "CTX_UPGRADE_INTERVAL_SECONDS",
    "DBUS_SESSION_BUS_ADDRESS",
    "FORGE_CONFIG",
    "HERMES_HOME",
    "HOME",
    "HOMEDRIVE",
    "HOMEPATH",
    "HTTPS_PROXY",
    "HTTP_PROXY",
    "KILO_DB",
    "LANG",
    "LC_ALL",
    "LOCALAPPDATA",
    "MIMOCODE_CONFIG_DIR",
    "MIMOCODE_DB",
    "MIMOCODE_DISABLE_CHANNEL_DB",
    "MIMOCODE_HOME",
    "NO_PROXY",
    "OPENCLAW_STATE_DIR",
    "PATH",
    "PI_CODING_AGENT_SESSION_DIR",
    "SHELLEY_DB",
    "SSL_CERT_DIR",
    "SSL_CERT_FILE",
    "SystemRoot",
    "TEMP",
    "TMP",
    "TMPDIR",
    "TZ",
    "USER",
    "USERPROFILE",
    "VIBE_HOME",
    "WINDIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_RUNTIME_DIR",
    "XDG_STATE_HOME",
    "https_proxy",
    "http_proxy",
    "no_proxy",
];
const DAEMON_PRO_CHANNEL_ENV: &str = "CTX_PRO_CHANNEL";

pub(super) fn configure_narrow_daemon_environment(command: &mut Command) {
    let inherited = DAEMON_CHILD_ENV_ALLOWLIST
        .iter()
        .filter_map(|name| env::var_os(name).map(|value| (*name, value)))
        .collect::<Vec<_>>();
    command.env_clear();
    command.envs(inherited);
}

pub(super) fn spawn_daemon_child(command: &mut Command) -> io::Result<Child> {
    validate_daemon_pro_channel_environment(command)?;
    crate::process_environment::sanitize_release_authority_env(command);
    command.spawn()
}

fn validate_daemon_pro_channel_environment(command: &Command) -> io::Result<()> {
    let channel = command
        .get_envs()
        .find(|(name, _)| *name == std::ffi::OsStr::new(DAEMON_PRO_CHANNEL_ENV))
        .and_then(|(_, value)| value);
    if channel.is_none_or(|value| {
        value == std::ffi::OsStr::new("stable") || value == std::ffi::OsStr::new("staging")
    }) {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{DAEMON_PRO_CHANNEL_ENV} must be stable or staging"),
    ))
}

pub(super) fn configured_daemon_autostart_command(
    exe: &Path,
    data_root: &Path,
    trigger: DaemonTriggerCommandArg,
    handoff_token: Option<&str>,
) -> Command {
    let mut command = daemon_autostart_command(
        exe,
        data_root,
        trigger,
        daemon_autostart_u64_env(
            "CTX_DAEMON_AUTOSTART_IDLE_EXIT_SECONDS",
            DAEMON_IDLE_EXIT_SECONDS_CAP,
        ),
        daemon_autostart_u64_env("CTX_DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS", 3_600),
        handoff_token,
    );
    // Preserve an explicit process override across detached launch and
    // replacement handoff. Config-selected mode remains reloadable because
    // the child reads the same data root instead of freezing it into an env
    // override.
    if let Some(mode) = env::var_os(DAEMON_MODE_ENV) {
        command.env(DAEMON_MODE_ENV, mode);
    }
    command
}

pub(super) fn daemon_restart_allowed(data_root: &Path) -> Result<bool> {
    Ok(daemon_autostart_allowed(
        data_root,
        &AppConfig::load(data_root)?,
    ))
}

pub(super) fn daemon_autostart_allowed(_data_root: &Path, config: &AppConfig) -> bool {
    config.daemon.enabled && !semantic_env_flag(DAEMON_AUTOSTART_OFF_ENV)
}

pub(super) fn daemon_restart_trigger(data_root: &Path) -> Option<DaemonTriggerCommandArg> {
    if !daemon_lock_is_active(data_root) {
        return None;
    }
    let trigger = read_daemon_status(data_root).and_then(|status| {
        parse_daemon_trigger(status.get("trigger_command").and_then(Value::as_str))
    });
    trigger.or(Some(DaemonTriggerCommandArg::Search))
}

pub(super) fn parse_daemon_trigger(value: Option<&str>) -> Option<DaemonTriggerCommandArg> {
    match value {
        Some("setup") => Some(DaemonTriggerCommandArg::Setup),
        Some("import") => Some(DaemonTriggerCommandArg::Import),
        Some("search") => Some(DaemonTriggerCommandArg::Search),
        _ => None,
    }
}
