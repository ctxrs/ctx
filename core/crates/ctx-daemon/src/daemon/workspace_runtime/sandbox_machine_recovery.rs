use super::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};

mod helper_cleanup;
mod init;
mod readiness;
mod running;
mod state;

use self::helper_cleanup::cleanup_ctx_managed_sandbox_helper_processes;
#[cfg(test)]
pub(in crate::daemon::workspace_runtime) use self::helper_cleanup::collect_ctx_managed_sandbox_helper_pids;
#[cfg(test)]
pub(in crate::daemon::workspace_runtime) use self::helper_cleanup::collect_ctx_managed_sandbox_helper_pids_from_ps_output;
#[cfg(test)]
#[allow(unused_imports)]
pub(in crate::daemon::workspace_runtime) use self::helper_cleanup::{
    is_ctx_managed_sandbox_helper_process_command, kill_ctx_managed_sandbox_helper_processes,
    literal_pkill_pattern,
};
pub(in crate::daemon::workspace_runtime) use self::init::initialize_sandbox_machine_with_image;
pub(super) use self::init::{initialize_sandbox_machine, run_sandbox_machine_init};
pub(in crate::daemon::workspace_runtime) use self::readiness::sandbox_machine_temp_state_paths;
use self::readiness::{
    best_effort_start_machine_after_init, clear_stale_sandbox_machine_temp_state,
    configured_sandbox_machine_memory_mb, format_heartbeat_elapsed,
    sandbox_machine_heartbeat_interval, wait_for_sandbox_machine_ready,
};
#[cfg_attr(test, allow(unused_imports))]
pub(in crate::daemon::workspace_runtime) use self::running::ensure_sandbox_machine_running_with_observer;
pub(super) use self::state::{
    looks_like_missing_machine_error, looks_like_recoverable_machine_start_error,
    looks_like_running_but_unreachable_machine_start_error, sandbox_machine_present,
    sandbox_machine_singleflight_lock,
};

use ctx_harness_setup::{
    observe_log, observe_progress, HarnessSetupLogLevel, HarnessSetupObserver, HarnessSetupPhase,
    HarnessSetupProgressUpdate,
};
use ctx_store::Store;
use tokio::sync::Mutex;
