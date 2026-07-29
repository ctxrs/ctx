use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{self, Child, Command, Stdio},
    time::{Duration as StdDuration, Instant, SystemTime},
};

#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
#[cfg(windows)]
use std::os::windows::process::CommandExt as _;

use anyhow::{anyhow, Context, Result};
use ctx_history_core::{database_path, utc_now};
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
        daemon_lock_is_active, daemon_lock_is_owned_by, daemon_lock_is_stale, daemon_lock_path,
        daemon_root_path, daemon_status_path, open_or_create_pid_lock_file, process_state,
        read_daemon_status, write_daemon_status, write_private_json_file, ProcessState,
    },
    runtime_limits::{
        DAEMON_AUTOSTART_IDLE_EXIT_SECONDS_DEFAULT, DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS_DEFAULT,
        DAEMON_AUTOSTART_OFF_ENV, DAEMON_BACKGROUND_CHILD_ENV, DAEMON_IDLE_EXIT_SECONDS_CAP,
        DAEMON_QUERY_ENDPOINT_FILE,
    },
};

mod autostart;
mod handoff;
mod installation;
mod recovery;

pub(crate) use autostart::{
    autostart_daemon_and_wait, daemon_autostart_can_reuse_existing,
    daemon_autostart_suppression_reason, maybe_autostart_daemon, maybe_autostart_daemon_for_search,
};
use autostart::{
    configured_daemon_autostart_command, daemon_autostart_allowed, daemon_autostart_command,
    daemon_restart_allowed, daemon_restart_trigger, parse_daemon_trigger, request_daemon_autostart,
};
#[cfg(test)]
use autostart::{daemon_handoff_observation_from, wait_for_daemon_handoff_with};

pub(super) use handoff::{
    acknowledge_daemon_restart_requests, current_process_owns_daemon_upgrade_handoff,
    daemon_upgrade_handoff_blocks_current_process, read_daemon_restart_request,
    write_daemon_restart_request,
};
pub(crate) use handoff::{
    begin_current_daemon_upgrade_handoff, begin_daemon_upgrade_handoff,
    complete_replacement_daemon_handoff, finish_replacement_daemon_handoff,
    mark_replacement_helper_handoff, DaemonUpgradeHandoff,
};
use handoff::{
    daemon_query_endpoint_path, daemon_upgrade_handoff_is_active, remove_daemon_restart_requests,
};
#[cfg(test)]
use handoff::{read_daemon_upgrade_handoff, write_daemon_upgrade_handoff};

pub(super) use installation::InstallationDaemonLease;
#[cfg(test)]
use installation::{
    open_installation_daemon_quiescence_lock_at, read_installation_daemon_restarts_from,
    wait_for_installation_daemon_quiescence_at,
};
use installation::{read_installation_daemon_restarts, wait_for_installation_daemon_quiescence};

pub(super) use recovery::resume_completed_installation_daemons;
use recovery::{
    clear_legacy_daemon_readiness, restart_acknowledged_installation_daemons,
    restart_acknowledged_legacy_installation_daemons, wait_for_daemon_ready_ack,
    wait_for_legacy_replacement_daemon, wait_for_replacement_daemon,
};
#[cfg(test)]
use recovery::{
    legacy_daemon_query_endpoint_is_ready, legacy_daemon_query_service_is_ready,
    legacy_daemon_status_is_ready,
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

pub(super) fn daemon_autostart_u64_env(name: &str, default: u64, max: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.min(max))
        .unwrap_or(default)
}

pub(super) fn maybe_autostart_daemon_inner(
    data_root: &Path,
    config: &AppConfig,
    trigger: DaemonTriggerCommandArg,
) {
    let _ = request_daemon_autostart(data_root, config, trigger);
}

const DAEMON_UPGRADE_STOP_TIMEOUT: StdDuration = StdDuration::from_secs(75);
const DAEMON_UPGRADE_RESTART_TIMEOUT: StdDuration = StdDuration::from_secs(5);
const DAEMON_UPGRADE_POLL_INTERVAL: StdDuration = StdDuration::from_millis(50);
const DAEMON_UPGRADE_HANDOFF_STALE_AFTER: StdDuration = StdDuration::from_secs(15 * 60);
const DAEMON_INSTALLATION_QUIESCE_TIMEOUT: StdDuration = StdDuration::from_secs(75);
const DAEMON_UPGRADE_HANDOFF_FILE: &str = "upgrade-handoff.json";
const DAEMON_UPGRADE_RESTART_REQUEST_DIR: &str = "upgrade-restart-requests";
const DAEMON_UPGRADE_HANDOFF_TOKEN_ENV: &str = "CTX_DAEMON_UPGRADE_HANDOFF_TOKEN";
// Installation recovery may restart several registered data-root daemons
// serially before this daemon can publish final readiness. Keep setup bounded,
// but allow that established five-second-per-registration path ample room.
const DAEMON_SETUP_HANDOFF_POLL_ATTEMPTS: usize = 12_001;
const DAEMON_SETUP_HANDOFF_MAX_HEARTBEAT_AGE_MS: i64 = 30_000;
const DAEMON_SETUP_HANDOFF_MAX_FUTURE_HEARTBEAT_MS: i64 = 5_000;

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
