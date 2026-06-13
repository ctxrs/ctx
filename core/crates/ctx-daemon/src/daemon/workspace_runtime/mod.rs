#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
#[cfg(test)]
use std::time::Duration;

#[cfg(test)]
use anyhow::{Context, Result};
#[cfg(all(test, any(target_os = "macos", target_os = "windows")))]
use ctx_core::ids::SessionId;
#[cfg(all(test, any(target_os = "macos", target_os = "windows")))]
use ctx_core::models::ExecutionEnvironment;
#[cfg(test)]
use ctx_core::models::{Workspace, Worktree};
#[cfg(all(test, any(target_os = "macos", target_os = "windows")))]
use ctx_resource_utilization::SystemSnapshot;
#[cfg(all(test, any(target_os = "macos", target_os = "windows")))]
use ctx_settings_model::normalize_container_machine_idle_shutdown_seconds;
#[cfg(test)]
use ctx_settings_model::{
    ContainerExecutionSettings, ContainerRuntimeKind, ExecutionMode, ExecutionSettings,
};
#[cfg(all(test, any(target_os = "macos", target_os = "windows")))]
use ctx_store::StoreManager;
#[cfg(all(test, any(target_os = "macos", target_os = "windows")))]
use ctx_transport_runtime::terminals::TerminalManager;
#[cfg(test)]
use url::Url;

#[cfg(test)]
mod avf_gateway;
#[cfg(test)]
mod machine;
#[cfg(test)]
mod memory;
#[cfg(test)]
mod reclaim_unit_tests;
#[cfg(test)]
mod sandbox_machine_lifecycle;
#[cfg(test)]
mod sandbox_machine_recovery;
#[cfg(test)]
// EXCEPTION: these tests intentionally serialize env-var mutations with a sync lock
// that spans async calls so process-global state cannot interleave across test cases.
#[allow(clippy::await_holding_lock)]
mod tests;

#[cfg(test)]
pub use self::avf_gateway::ensure_avf_guest_gateway_proxy_for_test;
#[cfg(test)]
use self::machine::{
    ensure_managed_sandbox_cli_runtime, ensure_managed_sandbox_machine_cache,
    persist_sandbox_machine_cache_to_shared, persist_sandbox_machine_cache_to_shared_best_effort,
    sandbox_machine_cache_root, sandbox_machine_home_root, sandbox_machine_runtime_root,
    sandbox_machine_temp_root, seed_shared_sandbox_machine_cache,
    seed_shared_sandbox_machine_cache_best_effort,
};
#[cfg(test)]
pub use self::memory::{container_machine_memory_mb, container_machine_memory_mb_for_host_memory};
#[cfg(test)]
pub use self::sandbox_machine_lifecycle::SandboxMachineLifecycleExt;
#[cfg(test)]
use self::sandbox_machine_recovery::{
    run_sandbox_machine_init, sandbox_machine_present, sandbox_machine_singleflight_lock,
};
#[cfg(test)]
#[allow(unused_imports)]
use ctx_avf_linux_runtime::AVF_LINUX_HELPER_PATH_ENV;
#[cfg(test)]
#[allow(unused_imports)]
use ctx_harness_runtime::{
    container_image_present, ensure_builder_backend_launch_ready_with_observer,
    launch_ready_detail_message, launch_ready_gap_message, local_runtime_available,
    prefetch_container_image, prewarm_selected_runtime_for_launch_with_observer,
    prewarm_selected_runtime_with_observer, resolve_container_image, runtime_prewarm_ready_message,
    runtime_prewarm_target, sandbox_container_command, sandbox_machine_name,
    selected_runtime_launch_readiness_state, selected_runtime_launch_ready, selected_runtime_state,
    selected_shared_substrate_lifecycle, workspace_launch_ready_message,
};
#[cfg(test)]
#[allow(unused_imports)]
use ctx_harness_runtime::{
    sandbox_engine_ready, selected_sandbox_command_backend, selected_sandbox_command_mode,
    HarnessExecutionPlan, HarnessRuntimeKind, HarnessRuntimeStats, SandboxCommandBackend,
    CTX_AVF_HOST_DATA_ROOT_ENV, CTX_AVF_HOST_WORKTREE_ROOT_ENV, CTX_AVF_WORKSPACE_ID_ENV,
    CTX_AVF_WORKTREE_ID_ENV, CTX_HARNESS_LINUX_SANDBOX_ENV, CTX_HARNESS_RUNTIME_KIND_ENV,
};
#[cfg(test)]
#[allow(unused_imports)]
use ctx_sandbox_container_runtime::CTX_HARNESS_SANDBOX_CLI_PATH_ENV;
#[cfg(test)]
use ctx_sandbox_container_runtime::{
    command_output_message, command_output_with_timeout, sandbox_cli_invocation,
};
#[cfg(test)]
use ctx_sandbox_container_runtime::{
    ensure_managed_default_container_image_tar_with_source, managed_default_image_install_lock,
    sandbox_cli_binary_path,
};
#[cfg(test)]
use ctx_workspace_container::sandbox_machine_required;
#[cfg(test)]
use ctx_workspace_runtime::HarnessRuntimeManager;

#[cfg(test)]
const SANDBOX_MACHINE_CACHE_DIR_ENV: &str = "CTX_SANDBOX_MACHINE_CACHE_DIR";
#[cfg(test)]
const SANDBOX_INFO_TIMEOUT: Duration = Duration::from_secs(15);
#[cfg(test)]
const SANDBOX_MACHINE_START_TIMEOUT: Duration = Duration::from_secs(180);
#[cfg(test)]
const SANDBOX_MACHINE_INIT_TIMEOUT: Duration = Duration::from_secs(8 * 60);

#[cfg(test)]
fn sandbox_machine_init_created_machine_grace() -> Duration {
    if cfg!(test) {
        Duration::from_millis(300)
    } else {
        Duration::from_secs(10)
    }
}

#[cfg(test)]
fn sandbox_machine_init_poll_interval() -> Duration {
    if cfg!(test) {
        Duration::from_millis(50)
    } else {
        Duration::from_secs(1)
    }
}

#[cfg(test)]
fn sandbox_machine_ready_timeout() -> Duration {
    Duration::from_millis(300)
}

#[cfg(test)]
fn sandbox_machine_ready_poll_interval() -> Duration {
    Duration::from_millis(25)
}

#[cfg(test)]
use ctx_harness_setup::{observe_log, observe_phase};
