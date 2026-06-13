use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use ctx_core::ids::{WorkspaceId, WorktreeId};
use ctx_runtime_assets::{
    acquire_managed_artifact_file_lock, download_managed_artifact, extract_archive_to_dir,
    finalize_managed_artifact_download, managed_artifact_lock_path, managed_artifact_partial_path,
    resolve_single_extracted_root,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::sync::Mutex;

mod helper_wrappers;
mod runtime_bootstrap;
mod runtime_install;
#[cfg(test)]
mod tests;

use super::{
    observe_log, observe_phase, sha256_hex_file, ContainerExecutionSettings, HarnessSetupLogLevel,
    HarnessSetupObserver, HarnessSetupPhase,
};
use ctx_harness_setup::{ManagedArtifactDownloadReporter, ManagedDownloadAggregate};

pub use self::helper_wrappers::{
    build_guest_exec_command, helper_path, prepare_guest_worktree, prepare_runtime_layout,
    probe_helper, run_guest_exec_capture, start_workspace_vm, stop_shared_vm,
    workspace_vm_data_root, workspace_vm_state,
};
pub use self::runtime_bootstrap::{
    ensure_guest_worktree_from_host_copy, ensure_shared_vm_ready_with_observer,
    ensure_workspace_vm_ready_with_observer, prefetch_runtime_with_observer, runtime_available,
    runtime_state,
};
pub use self::runtime_install::runtime_target_label;
use self::runtime_install::*;
#[cfg(any(test, feature = "test-support"))]
pub use self::runtime_install::{
    override_managed_avf_linux_runtime_source_for_test, TestManagedAvfLinuxRuntimeSourceGuard,
};

pub const AVF_LINUX_HELPER_PATH_ENV: &str = "CTX_AVF_LINUX_HELPER_PATH";
pub const AVF_LINUX_GUEST_RUNTIME_DIR_ENV: &str = "CTX_AVF_LINUX_GUEST_RUNTIME_DIR";
const AVF_LINUX_GUEST_RUNTIME_ID: &str = "avf-linux-guest";
const AVF_LINUX_RUNTIME_READY_MARKER: &str = ".ctx-managed-ready";
const AVF_LINUX_ROOTFS_LABEL: &str = "Ubuntu guest runtime";
const AVF_LINUX_KERNEL_HELPER: &str = "kernel";
const AVF_LINUX_INITRD_HELPER: &str = "initrd";
const AVF_LINUX_GUEST_AGENT_HELPER: &str = "guest-agent";
const AVF_LINUX_EGRESS_PROXY_HELPER: &str = "egress-proxy";
const AVF_LINUX_CONTAINER_STACK_HELPER: &str = "container-stack";
const AVF_LINUX_CONTAINER_STACK_FILE: &str = "container-stack.tar.gz";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvfLinuxHelperProbe {
    pub protocol_version: u32,
    pub protocol_schema: String,
    pub helper_version: String,
    #[serde(default)]
    pub exact_version: String,
    #[serde(default)]
    pub build_id: String,
    #[serde(default)]
    pub compatibility_token: String,
    pub host_os: String,
    pub host_arch: String,
    pub supported: bool,
    pub save_restore_supported: bool,
    pub rosetta_supported: bool,
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AvfLinuxSharedVmLifecycleState {
    Missing,
    Stopped,
    Starting,
    Running,
    Error,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AvfLinuxRuntimeLayoutStatus {
    Prepared,
    AlreadyPresent,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AvfLinuxSharedVmTransitionStatus {
    Scaffolded,
    Ready,
    Stopped,
    AlreadyStopped,
    Missing,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AvfLinuxSharedVmStartOutcome {
    AlreadyRunning,
    ColdBoot,
    Restored,
    ColdBootAfterRestoreFailure,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AvfLinuxSharedVmStopOutcome {
    SavedStateWritten,
    ColdStop,
    ColdStopAfterSaveFailure,
    ColdStopSaveUnsupported,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvfLinuxRuntimeLayout {
    pub protocol_version: u32,
    pub protocol_schema: String,
    pub vm_root: PathBuf,
    pub logs_root: PathBuf,
    pub state_path: PathBuf,
    pub layout_status: AvfLinuxRuntimeLayoutStatus,
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvfLinuxSharedVmState {
    pub protocol_version: u32,
    pub protocol_schema: String,
    pub state: AvfLinuxSharedVmLifecycleState,
    pub vm_root: PathBuf,
    pub logs_root: PathBuf,
    pub state_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_state_path: Option<PathBuf>,
    #[serde(default)]
    pub saved_state_exists: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_root: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rootfs_image: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initrd_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_shape_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_saved_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_stopped_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition_status: Option<AvfLinuxSharedVmTransitionStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_start_outcome: Option<AvfLinuxSharedVmStartOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_stop_outcome: Option<AvfLinuxSharedVmStopOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_restore_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_save_error: Option<String>,
    #[serde(default)]
    pub simulated: bool,
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AvfLinuxGuestWorktreeStatus {
    Prepared,
    AlreadyPresent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvfLinuxGuestWorktree {
    pub protocol_version: u32,
    pub protocol_schema: String,
    pub workspace_id: String,
    pub worktree_id: String,
    pub guest_root: PathBuf,
    pub host_shadow_root: PathBuf,
    pub metadata_path: PathBuf,
    pub status: AvfLinuxGuestWorktreeStatus,
    #[serde(default)]
    pub simulated: bool,
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct AvfLinuxGuestRuntime {
    pub runtime_root: PathBuf,
    pub rootfs_image: PathBuf,
    pub kernel_path: PathBuf,
    pub initrd_path: PathBuf,
    pub guest_agent_path: Option<PathBuf>,
    pub egress_proxy_path: Option<PathBuf>,
    pub container_stack_path: PathBuf,
    pub version: String,
    pub managed: bool,
}

pub fn shared_vm_is_launch_ready(state: &AvfLinuxSharedVmState) -> bool {
    matches!(state.state, AvfLinuxSharedVmLifecycleState::Running)
        && matches!(
            state.transition_status,
            Some(AvfLinuxSharedVmTransitionStatus::Ready)
        )
}

pub fn shared_vm_start_in_progress(state: &AvfLinuxSharedVmState) -> bool {
    matches!(
        state.state,
        AvfLinuxSharedVmLifecycleState::Starting | AvfLinuxSharedVmLifecycleState::Running
    ) && !shared_vm_is_launch_ready(state)
}
