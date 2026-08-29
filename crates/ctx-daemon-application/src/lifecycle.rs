use std::{
    cell::Cell,
    collections::BTreeMap,
    env,
    ffi::{OsStr, OsString},
    fmt, fs, io,
    path::Path,
    process::Child,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use ctx_daemon_runtime::{
    daemon_lock_is_active, daemon_lock_is_owned_by, daemon_lock_is_stale,
    daemon_lock_matches_executable, daemon_lock_path, executable_sha256, pid_from_lock_json,
    read_daemon_status, read_pid_lock_json, spawn_detached, write_daemon_status,
    DaemonHandoffRestartDeferral, NormalizedLaunch,
};
use ctx_history_core::utc_now;
use serde_json::{json, Value};

use crate::*;
mod finite_worker;
mod launch;
mod readiness_receipt;
#[cfg(test)]
mod tests;
use finite_worker::reap_owned_candidate;
pub use finite_worker::FiniteCoreWorkerLease;
use launch::configured_finite_core_worker_command;
#[cfg(test)]
use launch::normalized_daemon_launch_for_test;
pub use launch::{
    configured_daemon_autostart_command, daemon_autostart_command, spawn_daemon_child,
    spawn_daemon_child_for_upgrade_handoff, spawn_detached_daemon_child,
};
use readiness_receipt::{
    daemon_handoff_status_observation_from, daemon_lifecycle_response_observation,
};

const DAEMON_AUTOSTART_OFF_ENV: &str = "CTX_DAEMON_AUTOSTART_OFF";
const DAEMON_BACKGROUND_CHILD_ENV: &str = "CTX_DAEMON_BACKGROUND_CHILD";
const DAEMON_UPGRADE_POLL_INTERVAL: Duration = Duration::from_millis(50);
const DAEMON_SETUP_HANDOFF_STALL_POLL_ATTEMPTS: usize = 101;
const DAEMON_SETUP_HANDOFF_STALL_TIMEOUT: Duration = Duration::from_secs(5);
const DAEMON_SETUP_HANDOFF_MAX_HEARTBEAT_AGE_MS: i64 = 30_000;
const DAEMON_SETUP_HANDOFF_MAX_FUTURE_HEARTBEAT_MS: i64 = 5_000;
const DAEMON_HEALTH_TIMEOUT: Duration = Duration::from_millis(500);
const DAEMON_HEALTH_RESPONSE_MAX_BYTES: u64 = 16 * 1024;
const DAEMON_UPGRADE_STOP_TIMEOUT: Duration = Duration::from_secs(5);
const DAEMON_UPGRADE_RESTART_TIMEOUT: Duration = Duration::from_secs(5);
const DAEMON_UPGRADE_HANDOFF_TOKEN_ENV: &str = "CTX_DAEMON_UPGRADE_HANDOFF_TOKEN";
const DAEMON_MODE_ENV: &str = "CTX_DAEMON_MODE";
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaemonHandoff {
    pub pid: u32,
    pub heartbeat_at_ms: i64,
}
#[derive(Debug)]
pub enum DaemonStartError {
    Suppressed(&'static str),
    BinaryIdentity(anyhow::Error),
    Start(anyhow::Error),
    Ready(anyhow::Error),
}
#[derive(Debug, Clone, PartialEq, Eq)]
enum DaemonHandoffObservation {
    Pending,
    Starting,
    Running(DaemonHandoff),
    Failed(String),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DaemonReadinessRequirement {
    Full,
    Core,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DaemonLifecycleEndpointObservation {
    Unavailable,
    Starting,
    Ready,
}
#[derive(Debug)]
struct DaemonHandoffTimeout;
impl fmt::Display for DaemonHandoffTimeout {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("timed out waiting for live daemon lifecycle readiness")
    }
}
impl std::error::Error for DaemonHandoffTimeout {}
#[derive(Debug, Clone, PartialEq, Eq)]
struct DaemonOwnerIdentity {
    owner_id: String,
    pid: u32,
    started_at_ms: i64,
    binary_sha256: String,
}
enum DaemonAutostartRequest {
    Suppressed(&'static str),
    Existing(DaemonOwnerIdentity),
    Deferred(DaemonHandoffRestartDeferral),
    Spawned(Child),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DaemonLaunchProfile {
    Persistent,
    FiniteCoreWorker,
}
fn daemon_autostart_exe() -> Result<std::path::PathBuf> {
    std::env::var("CTX_DAEMON_AUTOSTART_EXE")
        .ok()
        .map(std::path::PathBuf::from)
        .map(Ok)
        .unwrap_or_else(|| {
            std::env::current_exe().context("resolve ctx daemon autostart executable")
        })
}
fn semantic_env_flag(name: &str) -> bool {
    matches!(std::env::var(name).as_deref(), Ok("1" | "true" | "TRUE"))
}
pub fn daemon_autostart_suppression_reason() -> Option<&'static str> {
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
fn write_daemon_autostart_status(
    data_root: &Path,
    trigger: DaemonTrigger,
    status: &str,
    reason: Option<&str>,
    last_error: Option<String>,
    pid: Option<u32>,
) -> Result<()> {
    let now = utc_now().timestamp_millis();
    write_daemon_status(
        data_root,
        &compact_json(json!({
            "schema_version": 1,
            "status": status,
            "reason": reason,
            "pid": pid,
            "started_at_ms": Value::Null,
            "heartbeat_at_ms": now,
            "finished_at_ms": now,
            "start_mode": DaemonStartMode::Auto.as_str(),
            "trigger_command": trigger.as_str(),
            "last_error": last_error,
        })),
    )
}

fn daemon_autostart_u64_env(name: &str, max: u64) -> Option<u64> {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value: &u64| *value > 0)
        .map(|value| value.min(max))
}

fn hosted_uninstall_fences_daemon_autostart(host: &dyn DaemonApplicationHost) -> bool {
    host.hosted_uninstall_active().unwrap_or(true)
}

pub fn daemon_start_is_fenced(host: &dyn DaemonApplicationHost) -> bool {
    hosted_uninstall_fences_daemon_autostart(host)
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
    daemon_lock_matches_executable(data_root, &daemon_autostart_exe()?)
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

fn wait_for_daemon_owner_identity(data_root: &Path) -> Result<Option<DaemonOwnerIdentity>> {
    let deadline = Instant::now() + DAEMON_SETUP_HANDOFF_STALL_TIMEOUT;
    loop {
        if let Some(owner) = read_daemon_owner_identity(data_root)? {
            return Ok(Some(owner));
        }
        if !daemon_lock_is_active(data_root) || Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(DAEMON_UPGRADE_POLL_INTERVAL);
    }
}

fn recover_unusable_daemon_owner(
    host: &dyn DaemonApplicationHost,
    data_root: &Path,
    observed_owner: &DaemonOwnerIdentity,
) -> Result<()> {
    let executable = daemon_autostart_exe()?;
    let terminated = recover_unusable_daemon_owner_with(
        observed_owner,
        || {
            Ok(daemon_lifecycle_endpoint_observation(
                host,
                data_root,
                observed_owner.pid,
                DAEMON_HEALTH_TIMEOUT,
            ) != DaemonLifecycleEndpointObservation::Unavailable)
        },
        || read_daemon_owner_identity(data_root),
        |owner_id| {
            ctx_daemon_runtime::terminate_identity_verified_residual_daemon_owner(
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

fn recover_unusable_daemon_owner_with(
    observed_owner: &DaemonOwnerIdentity,
    mut endpoint_usable: impl FnMut() -> Result<bool>,
    mut current_owner: impl FnMut() -> Result<Option<DaemonOwnerIdentity>>,
    mut terminate: impl FnMut(&str) -> Result<()>,
) -> Result<bool> {
    if endpoint_usable()? {
        return Ok(false);
    }
    // The health probe can race a supervisor or another foreground recovery.
    // Revalidate the complete advisory-lock owner identity after the bounded
    // probe immediately before any destructive action.
    if current_owner()?.as_ref() != Some(observed_owner) {
        return Ok(false);
    }
    terminate(&observed_owner.owner_id)?;
    Ok(true)
}

fn request_daemon_autostart(
    host: &dyn DaemonApplicationHost,
    data_root: &Path,
    config: &DaemonConfigSnapshot,
    trigger: DaemonTrigger,
    profile: DaemonLaunchProfile,
) -> Result<DaemonAutostartRequest> {
    if hosted_uninstall_fences_daemon_autostart(host) {
        return Ok(DaemonAutostartRequest::Suppressed(
            "hosted_uninstall_active",
        ));
    }
    if profile == DaemonLaunchProfile::Persistent && config.enabled {
        if let Some(deferral) = host.defer_restart_for_upgrade_handoff(data_root, trigger)? {
            return Ok(DaemonAutostartRequest::Deferred(deferral));
        }
    }
    // Suppression disables spawning, not reuse. Test harnesses and managed
    // callers can intentionally provide an already-owned daemon while
    // forbidding any additional detached process.
    if (config.enabled || profile == DaemonLaunchProfile::FiniteCoreWorker)
        && daemon_lock_is_active(data_root)
    {
        let executable = daemon_autostart_exe()?;
        if daemon_lock_matches_executable(data_root, &executable)? {
            return Ok(DaemonAutostartRequest::Existing(
                wait_for_daemon_owner_identity(data_root)?.ok_or_else(|| {
                    anyhow!("active ctx daemon lock has no stable owner identity")
                })?,
            ));
        }
        if daemon_autostart_suppression_reason().is_some() {
            return Err(binary_identity_handoff_error());
        }
        handoff_mismatched_daemon_owner(host, data_root, &executable)?;
        if daemon_lock_is_active(data_root) {
            return Ok(DaemonAutostartRequest::Existing(
                wait_for_daemon_owner_identity(data_root)?.ok_or_else(|| {
                    anyhow!("active replacement ctx daemon has no stable owner identity")
                })?,
            ));
        }
    }
    if let Some(reason) = daemon_autostart_suppression_reason() {
        return Ok(DaemonAutostartRequest::Suppressed(reason));
    }
    if profile == DaemonLaunchProfile::Persistent && !daemon_autostart_allowed(data_root, config) {
        return Ok(DaemonAutostartRequest::Suppressed("not_allowed"));
    }
    let automatic_recovery_allowed = profile == DaemonLaunchProfile::Persistent
        && config.enabled
        && config.mode == DaemonMode::Full
        && host
            .automatic_upgrade_recovery_allowed(data_root)
            .unwrap_or(false);
    if host.installation_upgrade_active().unwrap_or(false) && !automatic_recovery_allowed {
        return Ok(DaemonAutostartRequest::Suppressed(
            "installation_upgrade_active",
        ));
    }
    let lock_path = daemon_lock_path(data_root);
    if lock_path.exists() && !daemon_lock_is_stale(&lock_path) {
        let executable = daemon_autostart_exe()?;
        handoff_mismatched_daemon_owner(host, data_root, &executable)?;
        if daemon_lock_is_active(data_root) {
            return Ok(DaemonAutostartRequest::Existing(
                wait_for_daemon_owner_identity(data_root)?.ok_or_else(|| {
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
    let launch = match profile {
        DaemonLaunchProfile::Persistent => {
            configured_daemon_autostart_command(&exe, data_root, trigger, None)
        }
        DaemonLaunchProfile::FiniteCoreWorker => {
            configured_finite_core_worker_command(&exe, data_root, trigger)
        }
    };
    match launch.and_then(|launch| match profile {
        DaemonLaunchProfile::Persistent => spawn_daemon_child(host, launch),
        DaemonLaunchProfile::FiniteCoreWorker => {
            launch::spawn_attached_finite_core_worker(host, launch)
        }
    }) {
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

pub fn request_daemon_start(
    host: &dyn DaemonApplicationHost,
    data_root: &Path,
    config: &DaemonConfigSnapshot,
    trigger: DaemonTrigger,
) -> Result<()> {
    let _request = request_daemon_autostart(
        host,
        data_root,
        config,
        trigger,
        DaemonLaunchProfile::Persistent,
    )?;
    Ok(())
}

pub fn start_daemon_and_wait(
    host: &dyn DaemonApplicationHost,
    data_root: &Path,
    config: &DaemonConfigSnapshot,
    trigger: DaemonTrigger,
) -> std::result::Result<DaemonHandoff, DaemonStartError> {
    start_daemon_profile_and_wait(
        host,
        data_root,
        config,
        trigger,
        DaemonLaunchProfile::Persistent,
        DaemonReadinessRequirement::Full,
    )
    .map(|started| started.handoff)
}

pub fn start_core_daemon_and_wait(
    host: &dyn DaemonApplicationHost,
    data_root: &Path,
    config: &DaemonConfigSnapshot,
    trigger: DaemonTrigger,
) -> std::result::Result<DaemonHandoff, DaemonStartError> {
    start_daemon_profile_and_wait(
        host,
        data_root,
        config,
        trigger,
        DaemonLaunchProfile::Persistent,
        DaemonReadinessRequirement::Core,
    )
    .map(|started| started.handoff)
}

pub fn start_finite_core_worker_and_wait(
    host: &dyn DaemonApplicationHost,
    data_root: &Path,
    config: &DaemonConfigSnapshot,
    trigger: DaemonTrigger,
) -> std::result::Result<FiniteCoreWorkerLease, DaemonStartError> {
    let started = start_daemon_profile_and_wait(
        host,
        data_root,
        config,
        trigger,
        DaemonLaunchProfile::FiniteCoreWorker,
        DaemonReadinessRequirement::Full,
    )?;
    // Retain only the direct child matching the authenticated singleton winner.
    let mut child = started.child;
    if child
        .as_ref()
        .is_some_and(|candidate| candidate.id() != started.handoff.pid)
    {
        reap_owned_candidate(&mut child);
    }
    Ok(FiniteCoreWorkerLease::authenticated(started.handoff, child))
}
struct StartedDaemonProfile {
    handoff: DaemonHandoff,
    child: Option<Child>,
}
fn start_daemon_profile_and_wait(
    host: &dyn DaemonApplicationHost,
    data_root: &Path,
    config: &DaemonConfigSnapshot,
    trigger: DaemonTrigger,
    profile: DaemonLaunchProfile,
    readiness: DaemonReadinessRequirement,
) -> std::result::Result<StartedDaemonProfile, DaemonStartError> {
    let mut recovery_attempted = false;
    loop {
        let request = request_daemon_autostart(host, data_root, config, trigger, profile).map_err(
            |error| {
                if error.is::<BinaryIdentityHandoffError>() {
                    DaemonStartError::BinaryIdentity(error)
                } else {
                    DaemonStartError::Start(error)
                }
            },
        )?;
        let (mut child, pending_restart_request, existing_owner) = match request {
            DaemonAutostartRequest::Existing(owner) => (None, None, Some(owner)),
            DaemonAutostartRequest::Deferred(deferral) => (
                None,
                match deferral {
                    DaemonHandoffRestartDeferral::RestartRequest(path) => Some(path),
                    DaemonHandoffRestartDeferral::ReplacementPending => None,
                },
                None,
            ),
            DaemonAutostartRequest::Spawned(child) => (Some(child), None, None),
            DaemonAutostartRequest::Suppressed(reason) => {
                return Err(DaemonStartError::Suppressed(reason));
            }
        };
        let expected_failure_pid = child.as_ref().map(Child::id);
        let deadline = Cell::new(Instant::now() + DAEMON_SETUP_HANDOFF_STALL_TIMEOUT);
        let handoff = wait_for_daemon_handoff_with(
            DAEMON_SETUP_HANDOFF_STALL_POLL_ATTEMPTS,
            || {
                if pending_restart_request
                    .as_ref()
                    .is_some_and(|path| path.exists())
                {
                    DaemonHandoffObservation::Pending
                } else {
                    daemon_handoff_observation(
                        host,
                        data_root,
                        expected_failure_pid,
                        config,
                        readiness,
                        deadline
                            .get()
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
                if exit.success() && daemon_lock_is_active(data_root) {
                    let executable = daemon_autostart_exe()?;
                    if daemon_lock_matches_executable(data_root, &executable)? {
                        // Another same-binary cold-start child won singleton
                        // ownership. Join its authenticated readiness handoff
                        // instead of treating the losing child's clean exit as
                        // startup failure.
                        return Ok(None);
                    }
                }
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
            || deadline.set(Instant::now() + DAEMON_SETUP_HANDOFF_STALL_TIMEOUT),
            || {
                std::thread::sleep(
                    DAEMON_UPGRADE_POLL_INTERVAL
                        .min(deadline.get().saturating_duration_since(Instant::now())),
                )
            },
        );
        match handoff {
            Ok(handoff) => {
                return Ok(StartedDaemonProfile { handoff, child });
            }
            Err(error)
                if !recovery_attempted
                    && daemon_autostart_suppression_reason().is_none()
                    && error.is::<DaemonHandoffTimeout>()
                    && existing_owner.is_some() =>
            {
                let Some(existing_owner) = existing_owner.as_ref() else {
                    return Err(DaemonStartError::Ready(anyhow!(
                        "daemon owner identity disappeared before recovery"
                    )));
                };
                recover_unusable_daemon_owner(host, data_root, existing_owner)
                    .map_err(DaemonStartError::Ready)?;
                recovery_attempted = true;
            }
            Err(error) => {
                if profile == DaemonLaunchProfile::FiniteCoreWorker {
                    reap_owned_candidate(&mut child);
                }
                return Err(DaemonStartError::Ready(error));
            }
        }
    }
}

/// Observes an identity-stable ready daemon owner without changing lifecycle
/// state. This path never ensures supervision, requests startup, terminates an
/// owner, or spawns a child.
pub fn observe_daemon_and_wait(
    host: &dyn DaemonApplicationHost,
    data_root: &Path,
    config: &DaemonConfigSnapshot,
) -> Result<DaemonHandoff> {
    let deadline = Cell::new(Instant::now() + DAEMON_SETUP_HANDOFF_STALL_TIMEOUT);
    wait_for_observed_daemon_handoff_with(
        DAEMON_SETUP_HANDOFF_STALL_POLL_ATTEMPTS,
        || {
            daemon_handoff_observation(
                host,
                data_root,
                None,
                config,
                DaemonReadinessRequirement::Full,
                deadline
                    .get()
                    .saturating_duration_since(Instant::now())
                    .min(DAEMON_HEALTH_TIMEOUT),
            )
        },
        || deadline.set(Instant::now() + DAEMON_SETUP_HANDOFF_STALL_TIMEOUT),
        || {
            std::thread::sleep(
                DAEMON_UPGRADE_POLL_INTERVAL
                    .min(deadline.get().saturating_duration_since(Instant::now())),
            )
        },
    )
}

pub fn handoff_mismatched_daemon_owner(
    host: &dyn DaemonApplicationHost,
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
    let response = host.request_lifecycle_wakeup(
        data_root,
        compact_json(json!({
            "schema_version": 1,
            "op": "supervisor_handoff",
        })),
        DAEMON_HEALTH_TIMEOUT,
        DAEMON_HEALTH_RESPONSE_MAX_BYTES,
    );
    let accepted = response.ok().flatten().as_ref().is_some_and(|value| {
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
        ctx_daemon_runtime::terminate_identity_verified_residual_daemon(
            data_root,
            expected_executable,
        )
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
    host: &dyn DaemonApplicationHost,
    data_root: &Path,
    expected_failure_pid: Option<u32>,
    expected_config: &DaemonConfigSnapshot,
    readiness: DaemonReadinessRequirement,
    health_timeout: Duration,
) -> DaemonHandoffObservation {
    let status = read_daemon_status(data_root);
    let owner_before_probe = read_daemon_owner_identity(data_root).ok().flatten();
    let now_ms = utc_now().timestamp_millis();
    let observation = daemon_handoff_status_observation_from(
        status.as_ref(),
        owner_before_probe.as_ref(),
        expected_failure_pid,
        expected_config,
        readiness,
        now_ms,
    );
    if !matches!(observation, DaemonHandoffObservation::Running(_)) {
        return observation;
    }
    let Some(owner_before_probe) = owner_before_probe.as_ref() else {
        return DaemonHandoffObservation::Pending;
    };
    if health_timeout.is_zero() {
        return DaemonHandoffObservation::Pending;
    }
    let endpoint = daemon_lifecycle_endpoint_observation(
        host,
        data_root,
        owner_before_probe.pid,
        health_timeout,
    );
    let owner_after_probe = read_daemon_owner_identity(data_root).ok().flatten();
    complete_daemon_handoff_observation(
        observation,
        Some(owner_before_probe),
        owner_after_probe.as_ref(),
        endpoint,
    )
}

fn complete_daemon_handoff_observation(
    observation: DaemonHandoffObservation,
    owner_before_probe: Option<&DaemonOwnerIdentity>,
    owner_after_probe: Option<&DaemonOwnerIdentity>,
    endpoint: DaemonLifecycleEndpointObservation,
) -> DaemonHandoffObservation {
    match observation {
        DaemonHandoffObservation::Running(_)
            if owner_before_probe.is_none() || owner_before_probe != owner_after_probe =>
        {
            DaemonHandoffObservation::Pending
        }
        DaemonHandoffObservation::Running(handoff) => match endpoint {
            DaemonLifecycleEndpointObservation::Unavailable => DaemonHandoffObservation::Pending,
            DaemonLifecycleEndpointObservation::Starting => DaemonHandoffObservation::Starting,
            DaemonLifecycleEndpointObservation::Ready => DaemonHandoffObservation::Running(handoff),
        },
        observation => observation,
    }
}

fn daemon_lifecycle_endpoint_observation(
    host: &dyn DaemonApplicationHost,
    data_root: &Path,
    expected_pid: u32,
    timeout: Duration,
) -> DaemonLifecycleEndpointObservation {
    host.request_lifecycle_wakeup(
        data_root,
        compact_json(json!({
            "schema_version": 1,
            "op": "lifecycle_ping",
        })),
        timeout,
        DAEMON_HEALTH_RESPONSE_MAX_BYTES,
    )
    .ok()
    .flatten()
    .map_or(
        DaemonLifecycleEndpointObservation::Unavailable,
        |response| daemon_lifecycle_response_observation(&response, expected_pid),
    )
}

fn wait_for_daemon_handoff_with(
    attempts: usize,
    mut observe: impl FnMut() -> DaemonHandoffObservation,
    mut child_failure: impl FnMut() -> Result<Option<String>>,
    mut renew_starting_progress: impl FnMut(),
    mut pause: impl FnMut(),
) -> Result<DaemonHandoff> {
    if attempts == 0 {
        return Err(DaemonHandoffTimeout.into());
    }
    let mut stalled_attempts = 0;
    loop {
        match observe() {
            DaemonHandoffObservation::Running(handoff) => return Ok(handoff),
            DaemonHandoffObservation::Failed(error) => return Err(anyhow!(error)),
            DaemonHandoffObservation::Starting => {
                stalled_attempts = 0;
                renew_starting_progress();
            }
            DaemonHandoffObservation::Pending => stalled_attempts += 1,
        }
        if let Some(error) = child_failure()? {
            return Err(anyhow!(error));
        }
        if stalled_attempts >= attempts {
            return Err(DaemonHandoffTimeout.into());
        }
        pause();
    }
}

fn wait_for_observed_daemon_handoff_with(
    attempts: usize,
    observe: impl FnMut() -> DaemonHandoffObservation,
    renew_starting_progress: impl FnMut(),
    pause: impl FnMut(),
) -> Result<DaemonHandoff> {
    wait_for_daemon_handoff_with(
        attempts,
        observe,
        || Ok(None),
        renew_starting_progress,
        pause,
    )
}

pub fn daemon_restart_allowed(host: &dyn DaemonApplicationHost, data_root: &Path) -> Result<bool> {
    Ok(daemon_autostart_allowed(
        data_root,
        &host.daemon_config(data_root)?,
    ))
}

pub fn daemon_autostart_allowed(_data_root: &Path, config: &DaemonConfigSnapshot) -> bool {
    config.enabled && !semantic_env_flag(DAEMON_AUTOSTART_OFF_ENV)
}

pub fn daemon_restart_trigger(data_root: &Path) -> Option<DaemonTrigger> {
    if !daemon_lock_is_active(data_root) {
        return None;
    }
    let trigger = read_daemon_status(data_root).and_then(|status| {
        status
            .get("trigger_command")
            .and_then(Value::as_str)
            .and_then(DaemonTrigger::parse_persisted)
    });
    trigger.or(Some(DaemonTrigger::Search))
}

pub fn parse_persisted_trigger(value: Option<&str>) -> Option<DaemonTrigger> {
    value.and_then(DaemonTrigger::parse_persisted)
}
