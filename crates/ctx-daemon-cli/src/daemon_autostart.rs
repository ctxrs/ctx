#[cfg(test)]
use std::{collections::BTreeMap, ffi::OsStr};
use std::{
    collections::BTreeSet,
    env, fs, io,
    path::{Path, PathBuf},
    process::{self, Child},
    time::{Duration as StdDuration, Instant},
};

use anyhow::{anyhow, Context, Result};
use ctx_daemon_runtime::NormalizedLaunch;
use ctx_history_core::utc_now;
use serde_json::{json, Value};
use uuid::Uuid;

#[cfg(test)]
use crate::config::DAEMON_MODE_ENV;
use crate::{compact_json, config::AppConfig, DaemonTriggerCommandArg};

#[cfg(test)]
use super::runtime_limits::{DAEMON_AUTOSTART_OFF_ENV, DAEMON_BACKGROUND_CHILD_ENV};
use super::{
    paths_status::{
        daemon_lock_is_active, daemon_lock_is_owned_by, daemon_lock_path, daemon_root_path,
        pid_lock_guard_path, write_private_json_file,
    },
    runtime_limits::DAEMON_QUERY_ENDPOINT_FILE,
};

mod autostart;
mod handoff;
mod installation;
mod recovery;

#[cfg(test)]
use autostart::configured_daemon_autostart_command;
#[cfg(test)]
use autostart::daemon_autostart_allowed;
#[cfg(test)]
use autostart::handoff_mismatched_daemon_owner;
pub use autostart::{
    autostart_daemon_and_wait, autostart_daemon_for_setup_and_wait,
    daemon_autostart_suppression_reason, maybe_autostart_daemon, observe_daemon_for_setup_and_wait,
    restart_daemon_with_current_environment_and_wait, start_finite_core_worker_and_wait,
};
use autostart::{
    daemon_autostart_command, daemon_restart_allowed, daemon_restart_trigger, parse_daemon_trigger,
    spawn_daemon_child, spawn_daemon_child_for_upgrade_handoff,
};
#[cfg(test)]
use handoff::daemon_upgrade_handoff_is_active;
pub use handoff::prepare_daemon_uninstall;
use handoff::remove_daemon_restart_requests;
pub(super) use handoff::{
    acknowledge_daemon_restart_requests, current_process_owns_daemon_upgrade_handoff,
    daemon_upgrade_handoff_blocks_current_process, defer_restart_for_upgrade_handoff,
    read_daemon_restart_request, terminate_current_executable_daemon, write_daemon_restart_request,
};
pub use handoff::{
    begin_current_daemon_upgrade_handoff, begin_daemon_upgrade_handoff,
    begin_legacy_daemon_upgrade_handoff, complete_replacement_daemon_handoff,
    finish_replacement_daemon_handoff, mark_replacement_helper_handoff,
    replacement_helper_owns_daemon_handoff, DaemonUpgradeHandoff,
};
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
    restart_acknowledged_installation_daemons_with, wait_for_daemon_ready_ack,
    wait_for_replacement_daemon,
};

const DAEMON_UPGRADE_STOP_TIMEOUT: StdDuration = StdDuration::from_secs(5);
const DAEMON_UPGRADE_RESTART_TIMEOUT: StdDuration = StdDuration::from_secs(5);
const DAEMON_UPGRADE_POLL_INTERVAL: StdDuration = StdDuration::from_millis(50);
const DAEMON_UPGRADE_HANDOFF_STALE_AFTER: StdDuration = StdDuration::from_secs(15 * 60);
const DAEMON_INSTALLATION_QUIESCE_TIMEOUT: StdDuration = StdDuration::from_secs(75);
const DAEMON_UPGRADE_HANDOFF_TOKEN_ENV: &str = "CTX_DAEMON_UPGRADE_HANDOFF_TOKEN";
const DAEMON_HEALTH_TIMEOUT: StdDuration = StdDuration::from_millis(500);
const DAEMON_HEALTH_RESPONSE_MAX_BYTES: u64 = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaemonHandoff {
    pub pid: u32,
    pub heartbeat_at_ms: i64,
}

pub struct DaemonSetupHandoff {
    pub handoff: DaemonHandoff,
}

#[cfg(test)]
#[path = "daemon_autostart/tests.rs"]
mod telemetry_tests;
