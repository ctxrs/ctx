use std::{
    collections::BTreeSet,
    env, fs, io,
    path::{Path, PathBuf},
    process::{self, Child, Command, Stdio},
    time::{Duration as StdDuration, Instant, SystemTime},
};

#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
#[cfg(windows)]
use std::os::windows::process::CommandExt as _;

use anyhow::{anyhow, Context, Result};
use ctx_history_core::utc_now;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    compact_json,
    config::{AppConfig, DAEMON_MODE_ENV},
    DaemonStartModeArg, DaemonTriggerCommandArg,
};

#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
#[cfg(windows)]
const DETACHED_PROCESS: u32 = 0x0000_0008;

use super::{
    health_search::{create_private_dir_all, secure_private_file_permissions, semantic_env_flag},
    paths_status::{
        daemon_lock_is_active, daemon_lock_is_owned_by, daemon_lock_is_stale,
        daemon_lock_matches_executable, daemon_lock_path, daemon_root_path, executable_sha256,
        observe_pid_advisory_lock, open_or_create_pid_lock_file, pid_from_lock_json,
        pid_lock_guard_path, process_executable_sha256, process_state, read_daemon_status,
        read_pid_lock_json, write_daemon_status, write_private_json_file,
        PidAdvisoryLockObservation, ProcessState,
    },
    query_service::daemon_source_refresh_request,
    runtime_limits::{
        DAEMON_AUTOSTART_OFF_ENV, DAEMON_BACKGROUND_CHILD_ENV, DAEMON_IDLE_EXIT_SECONDS_CAP,
        DAEMON_QUERY_ENDPOINT_FILE,
    },
};

mod autostart;
mod handoff;
mod installation;
mod recovery;

#[cfg(test)]
use autostart::configure_narrow_daemon_environment;
pub(super) use autostart::handoff_mismatched_daemon_owner;
pub(crate) use autostart::{
    autostart_daemon_and_wait, daemon_autostart_suppression_reason, maybe_autostart_daemon,
};
use autostart::{
    configured_daemon_autostart_command, daemon_autostart_command, daemon_restart_allowed,
    daemon_restart_trigger, parse_daemon_trigger, request_daemon_autostart, spawn_daemon_child,
};
#[cfg(test)]
use autostart::{
    daemon_autostart_allowed, daemon_handoff_observation_from,
    daemon_live_endpoint_observation_from, wait_for_daemon_handoff_with,
};

pub(crate) use handoff::prepare_daemon_uninstall;
pub(super) use handoff::{
    acknowledge_daemon_restart_requests, current_process_owns_daemon_upgrade_handoff,
    daemon_upgrade_handoff_blocks_current_process, read_daemon_restart_request,
    terminate_current_executable_daemon, write_daemon_restart_request,
};
pub(crate) use handoff::{
    begin_current_daemon_upgrade_handoff, begin_daemon_upgrade_handoff,
    begin_legacy_daemon_upgrade_handoff, complete_replacement_daemon_handoff,
    finish_replacement_daemon_handoff, mark_replacement_helper_handoff,
    replacement_helper_owns_daemon_handoff, DaemonUpgradeHandoff,
};
use handoff::{daemon_upgrade_handoff_is_active, remove_daemon_restart_requests};
#[cfg(test)]
use handoff::{read_daemon_upgrade_handoff, write_daemon_upgrade_handoff};

pub(super) use installation::InstallationDaemonLease;
#[cfg(test)]
use installation::{
    open_installation_daemon_quiescence_lock_at, read_installation_daemon_restarts_from,
    registered_installation_daemon_roots_from, wait_for_installation_daemon_quiescence_at,
};
use installation::{
    read_installation_daemon_restarts, wait_for_installation_daemon_quiescence_for,
};

pub(super) use recovery::resume_completed_installation_daemons;
use recovery::{
    restart_acknowledged_installation_daemons, wait_for_daemon_ready_ack,
    wait_for_replacement_daemon,
};

pub(super) fn daemon_autostart_exe() -> Result<PathBuf> {
    env::var("CTX_DAEMON_AUTOSTART_EXE")
        .ok()
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(|| env::current_exe().context("resolve ctx daemon autostart executable"))
}

pub(super) fn write_daemon_autostart_status(
    data_root: &Path,
    trigger: DaemonTriggerCommandArg,
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
            "start_mode": DaemonStartModeArg::Auto.as_str(),
            "trigger_command": trigger.as_str(),
            "last_error": last_error,
        })),
    )
}

pub(super) fn daemon_autostart_u64_env(name: &str, max: u64) -> Option<u64> {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.min(max))
}

pub(super) fn maybe_autostart_daemon_inner(
    data_root: &Path,
    config: &AppConfig,
    trigger: DaemonTriggerCommandArg,
) {
    if daemon_autostart_suppression_reason().is_none()
        && super::daemon_supervisor::ensure_daemon_supervisor(data_root).is_err()
    {
        return;
    }
    let _ = request_daemon_autostart(data_root, config, trigger);
}

const DAEMON_UPGRADE_STOP_TIMEOUT: StdDuration = StdDuration::from_secs(5);
const DAEMON_UPGRADE_RESTART_TIMEOUT: StdDuration = StdDuration::from_secs(5);
const DAEMON_UPGRADE_POLL_INTERVAL: StdDuration = StdDuration::from_millis(50);
const DAEMON_UPGRADE_HANDOFF_STALE_AFTER: StdDuration = StdDuration::from_secs(15 * 60);
const DAEMON_INSTALLATION_QUIESCE_TIMEOUT: StdDuration = StdDuration::from_secs(75);
const DAEMON_UPGRADE_HANDOFF_FILE: &str = "upgrade-handoff.json";
const DAEMON_UPGRADE_RESTART_REQUEST_DIR: &str = "upgrade-restart-requests";
const DAEMON_UPGRADE_HANDOFF_TOKEN_ENV: &str = "CTX_DAEMON_UPGRADE_HANDOFF_TOKEN";
// Foreground setup/import/retrieval must never inherit the supervisor's
// potentially multi-root recovery horizon. It verifies one usable endpoint
// promptly; durable supervisor recovery continues independently.
const DAEMON_SETUP_HANDOFF_POLL_ATTEMPTS: usize = 101;
const DAEMON_SETUP_HANDOFF_MAX_HEARTBEAT_AGE_MS: i64 = 30_000;
const DAEMON_SETUP_HANDOFF_MAX_FUTURE_HEARTBEAT_MS: i64 = 5_000;
const DAEMON_HEALTH_TIMEOUT: StdDuration = StdDuration::from_millis(500);
const DAEMON_HEALTH_RESPONSE_MAX_BYTES: u64 = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DaemonHandoff {
    pub(crate) pid: u32,
    pub(crate) heartbeat_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DaemonHandoffObservation {
    Pending,
    Running(DaemonHandoff),
    Failed(String),
}

enum DaemonAutostartRequest {
    Suppressed(&'static str),
    Existing,
    Deferred(PathBuf),
    Spawned(Child),
}

#[cfg(test)]
#[path = "daemon_autostart/tests.rs"]
mod telemetry_tests;
