#[cfg(any(target_os = "macos", target_os = "windows"))]
use super::sandbox_machine_recovery::ensure_sandbox_machine_running_with_observer;
use super::sandbox_machine_recovery::{
    collect_ctx_managed_sandbox_helper_pids,
    collect_ctx_managed_sandbox_helper_pids_from_ps_output, initialize_sandbox_machine,
    initialize_sandbox_machine_with_image, is_ctx_managed_sandbox_helper_process_command,
    kill_ctx_managed_sandbox_helper_processes, literal_pkill_pattern,
    looks_like_missing_machine_error, looks_like_recoverable_machine_start_error,
    looks_like_running_but_unreachable_machine_start_error, sandbox_machine_temp_state_paths,
};
use super::*;
use crate::test_support::write_running_container_sandbox_cli_shim;
use chrono::Utc;
use ctx_bundled_assets as bundled_assets;
use ctx_bundled_assets::test_support::{
    override_managed_ctx_harness_image_source_for_test,
    override_managed_sandbox_machine_cache_source_for_test, TestManagedCtxHarnessImageSourceGuard,
    TestManagedSandboxMachineCacheSourceGuard,
};
#[cfg(target_os = "macos")]
use ctx_core::ids::SessionId;
#[cfg(target_os = "macos")]
use ctx_core::ids::TaskId;
use ctx_core::ids::{WorkspaceId, WorktreeId};
#[cfg(target_os = "macos")]
use ctx_core::models::ExecutionEnvironment;
use ctx_sandbox_container_runtime::SandboxCommandMode;
use ctx_sandbox_contract::CTX_CONTAINER_WORKSPACE_ROOT;
use ctx_settings_model::{ContainerMountMode, ContainerNetworkMode};
#[cfg(target_os = "macos")]
use ctx_store::StoreManager;
use ctx_workspace_container::workspace_container_name;
use ctx_workspace_container::{
    apply_container_network_policy, build_mounts, rewrite_daemon_url_for_avf_guest,
    should_use_keep_id_userns, WorkspaceContainer,
};
use sha2::{Digest, Sha256};
#[cfg(target_os = "macos")]
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio::time::{sleep, Duration};

mod avf_fixtures;
mod avf_workspace;
mod fixtures;
mod machine_cache;
mod machine_recovery;
mod managed_images;
mod network_cleanup;
mod readiness_messages;
mod reclaim;
mod recovery_tests;
mod runtime_prepare;
mod sandbox_cli;
