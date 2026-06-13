mod avf_linux_exec_protocol;
#[path = "avf_linux_helper/build_identity.rs"]
mod build_identity;
#[path = "avf_linux_helper/cli.rs"]
mod cli;
#[path = "avf_linux_helper/cloud_init.rs"]
mod cloud_init;
#[path = "avf_linux_helper/control.rs"]
mod control;
#[path = "avf_linux_helper/guest_exec.rs"]
mod guest_exec;
#[path = "avf_linux_helper/guest_worktree.rs"]
mod guest_worktree;
#[path = "avf_linux_helper/paths.rs"]
mod paths;
#[path = "avf_linux_helper/probe.rs"]
mod probe;
#[path = "avf_linux_helper/real_vm_runtime.rs"]
mod real_vm_runtime;
#[path = "avf_linux_helper/runtime_artifacts.rs"]
mod runtime_artifacts;
#[path = "avf_linux_helper/shared_vm_lifecycle.rs"]
mod shared_vm_lifecycle;
#[path = "avf_linux_helper/simulated_exec.rs"]
mod simulated_exec;
#[path = "avf_linux_helper/state.rs"]
mod state;
#[cfg(test)]
#[path = "avf_linux_helper/tests.rs"]
mod tests;
#[path = "avf_linux_helper/virtualization.rs"]
mod virtualization;

use std::collections::HashMap;
use std::fs;
use std::fs::File;
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
#[cfg(target_os = "macos")]
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
#[cfg(target_os = "macos")]
use block2::RcBlock;
#[cfg(target_os = "macos")]
use dispatch2::{DispatchQueue, DispatchQueueAttr};
use flate2::read::GzDecoder;
#[cfg(target_os = "macos")]
use objc2::rc::Retained;
#[cfg(target_os = "macos")]
use objc2::{AnyThread, ClassType};
#[cfg(target_os = "macos")]
use objc2_foundation::{NSArray, NSData, NSError, NSString, NSURL};
#[cfg(target_os = "macos")]
use objc2_virtualization::{
    VZDirectorySharingDeviceConfiguration, VZDiskImageCachingMode,
    VZDiskImageStorageDeviceAttachment, VZDiskImageSynchronizationMode, VZFileSerialPortAttachment,
    VZGenericMachineIdentifier, VZGenericPlatformConfiguration, VZLinuxBootLoader, VZMACAddress,
    VZMemoryBalloonDeviceConfiguration, VZNATNetworkDeviceAttachment, VZNetworkDeviceConfiguration,
    VZSerialPortConfiguration, VZSharedDirectory, VZSingleDirectoryShare,
    VZSocketDeviceConfiguration, VZStorageDeviceConfiguration, VZVirtioBlockDeviceConfiguration,
    VZVirtioConsoleDeviceSerialPortConfiguration, VZVirtioFileSystemDeviceConfiguration,
    VZVirtioNetworkDeviceConfiguration, VZVirtioSocketDeviceConfiguration,
    VZVirtioTraditionalMemoryBalloonDeviceConfiguration, VZVirtualMachine,
    VZVirtualMachineConfiguration, VZVirtualMachineState,
};
use portable_pty::{CommandBuilder as PtyCommandBuilder, NativePtySystem, PtySize, PtySystem};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use self::avf_linux_exec_protocol::{
    read_exec_frame, write_exec_frame, AvfLinuxExecError, AvfLinuxExecExit, AvfLinuxExecFrame,
    AvfLinuxExecRequest, AvfLinuxExecResize,
};
use self::build_identity::*;
use self::cloud_init::*;
use self::control::*;
use self::guest_exec::*;
use self::guest_worktree::*;
use self::paths::*;
use self::probe::*;
use self::real_vm_runtime::*;
#[cfg(test)]
use self::real_vm_runtime::{
    replay_shared_vm_controller_safety_trace, shared_vm_controller_safety_trace_canonical_json,
    SharedVmControllerSafetyHostPressureState, SharedVmControllerSafetyPressureState,
    SharedVmControllerSafetyReplayDecision, SharedVmControllerSafetyReplayPhase,
    SharedVmControllerSafetyReplayState, SharedVmControllerSafetyReplayStep,
};
use self::runtime_artifacts::*;
use self::shared_vm_lifecycle::*;
use self::simulated_exec::*;
use self::state::*;
use self::virtualization::*;

const HELPER_PROTOCOL_VERSION: u32 = 1;
const HELPER_PROTOCOL_SCHEMA: &str = "ctx.avf_linux_helper.v1";
const SHARED_VM_STATE_FILE: &str = "shared-vm-state.json";
const SHARED_VM_LOG_FILE: &str = "shared-vm.log";
const SHARED_VM_CONTROL_SOCKET_FILE: &str = "shared-vm-control.sock";
const SHARED_VM_GUEST_AGENT_SOCKET_FILE: &str = "shared-vm-guest-agent.sock";
const SHARED_VM_KERNEL_CMDLINE_FILE: &str = "kernel-cmdline";
const SHARED_VM_SAVED_STATE_FILE: &str = "saved-machine-state.vzvmsave";
const SHARED_VM_SHUTDOWN_REQUEST_FILE: &str = "shutdown-request";
const SHARED_VM_MEMORY_PRESSURE_REQUEST_FILE: &str = "memory-pressure-request";
const SHARED_VM_START_LOCK_FILE: &str = "start.lock";
const GUEST_WORKTREES_DIR: &str = "worktrees";
const GUEST_WORKTREE_METADATA_FILE: &str = "worktree.json";
const GUEST_WORKTREE_SHADOW_DIR: &str = "shadow-root";
const GUEST_WORKTREES_ROOT: &str = "/ctx/ws/worktrees";
const GUEST_WORKSPACE_HOMES_ROOT: &str = "/ctx/home";
const GUEST_WORKSPACE_CACHE_ROOT: &str = "/ctx/cache";
const GUEST_WORKSPACE_TMP_ROOT: &str = "/ctx/tmp";
const GUEST_WORKSPACE_USER_PREFIX: &str = "ctx-ws-";
const AVF_LINUX_GUEST_AGENT_HELPER: &str = "guest-agent";
const AVF_LINUX_EGRESS_PROXY_HELPER: &str = "egress-proxy";
const AVF_LINUX_CONTAINER_STACK_FILE: &str = "container-stack.tar.gz";
const SHARED_VM_CLOUD_INIT_DIR: &str = "cloud-init";
const SHARED_VM_CLOUD_INIT_META_DATA_FILE: &str = "meta-data";
const SHARED_VM_CLOUD_INIT_USER_DATA_FILE: &str = "user-data";
const SHARED_VM_CLOUD_INIT_NETWORK_CONFIG_FILE: &str = "network-config";
const SHARED_VM_CLOUD_INIT_IMAGE_FILE: &str = "cidata.iso";
const SHARED_VM_GUEST_AGENT_SERVICE_NAME: &str = "ctx-avf-linux-guest-agent.service";
const SHARED_VM_ROOTFS_LABEL: &str = "cloudimg-rootfs";
const SHARED_VM_BOOT_DIR: &str = "boot";
const SHARED_VM_BOOT_KERNEL_FILE: &str = "kernel";
const SHARED_VM_DISK_DIR: &str = "disk";
const SHARED_VM_ROOTFS_FILE: &str = "rootfs.raw";
const SHARED_VM_DATA_DISK_FILE: &str = "data.raw";
const SHARED_VM_MACHINE_IDENTIFIER_FILE: &str = "machine-identifier.bin";
const SHARED_VM_MAC_ADDRESS_FILE: &str = "mac-address.txt";
const SHARED_VM_GUEST_CONSOLE_LOG_FILE: &str = "guest-console.log";
const SHARED_VM_GUEST_CONTROL_READY_FILE: &str = "guest-control-ready";
const SHARED_VM_GUEST_CONTROL_FAILED_FILE: &str = "guest-control-failed";
const SHARED_VM_GUEST_AGENT_LOG_FILE: &str = "guest-agent.log";
const SHARED_VM_DATA_ROOT_SHARE_TAG: &str = "ctx-data-root";
const SHARED_VM_GUEST_HOST_DATA_ROOT: &str = ctx_sandbox_contract::SHARED_VM_GUEST_HOST_DATA_ROOT;
const SHARED_VM_HOST_DATA_SERVICE_NAME: &str = "ctx-avf-host-data.service";
const SHARED_VM_DATA_DISK_LABEL: &str = "ctx-avf-data";
const SHARED_VM_DATA_DISK_SERVICE_NAME: &str = "ctx-avf-data-disk.service";
const SHARED_VM_DATA_DISK_INSTALL_PATH: &str = "/usr/local/lib/ctx/ctx-avf-data-disk.sh";
const SHARED_VM_GUEST_AGENT_LAUNCHER_PATH: &str =
    "/usr/local/lib/ctx/ctx-avf-linux-guest-agent-launch.sh";
const SHARED_VM_CONTAINERD_SERVICE_NAME: &str = "containerd.service";
const SHARED_VM_BUILDKIT_SERVICE_NAME: &str = "buildkit.service";
const SHARED_VM_GUEST_WRITABLE_ROOT: &str = "/ctx";
const SHARED_VM_GUEST_WORKTREES_ROOT: &str = "/ctx/ws/worktrees";
const SHARED_VM_GUEST_HOME_ROOT: &str = "/ctx/home";
const SHARED_VM_GUEST_CACHE_ROOT: &str = "/ctx/cache";
const SHARED_VM_GUEST_TMP_ROOT: &str = "/ctx/tmp";
const SHARED_VM_GUEST_ROOT_HOME: &str = "/ctx/home/root";
const SHARED_VM_GUEST_ROOT_XDG_CONFIG_ROOT: &str = "/ctx/cache/xdg/config";
const SHARED_VM_GUEST_ROOT_XDG_DATA_ROOT: &str = "/ctx/cache/xdg/data";
const SHARED_VM_GUEST_ROOT_XDG_CACHE_ROOT: &str = "/ctx/cache/xdg/cache";
const SHARED_VM_GUEST_ROOT_XDG_RUNTIME_ROOT: &str = "/ctx/tmp/xdg-runtime-root";
const SHARED_VM_GUEST_LOG_ROOT: &str = "/ctx/system/log";
const SHARED_VM_GUEST_CONTAINERD_ROOT: &str = "/ctx/system/containerd";
const SHARED_VM_GUEST_BUILDKIT_ROOT: &str = "/ctx/system/buildkit";
const SHARED_VM_GUEST_NERDCTL_ROOT: &str = "/ctx/system/nerdctl";
const SHARED_VM_GUEST_CNI_CONFIG_ROOT: &str = "/ctx/system/cni/net.d";
const SHARED_VM_GUEST_CNI_STATE_ROOT: &str = "/ctx/system/cni/lib";
const SHARED_VM_PAYLOADS_DIR: &str = "payloads";
const SHARED_VM_GUEST_CONTAINER_STACK_INSTALL_PATH: &str =
    "/usr/local/lib/ctx/ctx-avf-install-container-stack.sh";
const SHARED_VM_GUEST_CONTAINER_STACK_MARKER_PATH: &str =
    "/usr/local/lib/ctx/container-stack.sha256";
const SHARED_VM_GUEST_POLICY_INSTALL_PATH: &str = "/usr/local/lib/ctx/ctx-avf-guest-policy.sh";
const SHARED_VM_GUEST_POLICY_MASKED_UNITS: &[&str] = &[
    "apt-daily.service",
    "apt-daily.timer",
    "apt-daily-upgrade.service",
    "apt-daily-upgrade.timer",
    "boot.mount",
    "boot-efi.mount",
    "unattended-upgrades.service",
    "snapd.service",
    "snapd.socket",
    "snapd.seeded.service",
    "fwupd-refresh.timer",
];
const SHARED_VM_GUEST_NERDCTL_BIN: &str = "/usr/local/bin/nerdctl";
const SHARED_VM_GUEST_BUILDKITCTL_BIN: &str = "/usr/local/bin/buildctl";
const SHARED_VM_GUEST_BUILDKIT_SOCKET: &str = "unix:///run/buildkit/buildkitd.sock";
const SHARED_VM_CPU_COUNT_ENV: &str = "CTX_AVF_LINUX_CPU_COUNT";
const SHARED_VM_MEMORY_CEILING_BYTES_ENV: &str = "CTX_AVF_LINUX_MEMORY_CEILING_BYTES";
const SHARED_VM_HOST_MEMORY_RESERVE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const SHARED_VM_MIN_DEFAULT_MEMORY_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const SHARED_VM_INITIAL_DATA_DISK_BYTES: u64 = 12 * 1024 * 1024 * 1024;
const SHARED_VM_HOST_DISK_RESERVE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const SHARED_VM_DATA_DISK_GROWTH_THRESHOLD_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const SHARED_VM_DATA_DISK_CRITICAL_FREE_BYTES: u64 = 512 * 1024 * 1024;
const SHARED_VM_DATA_DISK_GROWTH_STEP_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const SHARED_VM_MEMORY_BALLOON_STEP_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const SHARED_VM_GUEST_MEMORY_GROW_THRESHOLD_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const SHARED_VM_HOST_MEMORY_EMERGENCY_BYTES: u64 = 1024 * 1024 * 1024;
const SHARED_VM_MEMORY_WATCHDOG_CONFIRMATION_POLLS: u32 = 2;
#[cfg(target_os = "macos")]
const SHARED_VM_MEMORY_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
#[cfg(target_os = "macos")]
const SHARED_VM_MEMORY_WATCHDOG_POLL_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(2);
const SHARED_VM_MEMORY_WATCHDOG_EXIT_GRACE: Duration = Duration::from_secs(8);
const SHARED_VM_START_LOCK_POLL_INTERVAL: Duration = Duration::from_millis(250);
const SHARED_VM_READINESS_PHASE_TIMEOUT_SECONDS: u64 = 10;
const SHARED_VM_READINESS_GUEST_EXEC_IO_TIMEOUT: Duration =
    Duration::from_secs(SHARED_VM_READINESS_PHASE_TIMEOUT_SECONDS + 5);
const SHARED_VM_RUNTIME_GUEST_EXEC_IO_TIMEOUT: Duration = Duration::from_secs(120);
const SHARED_VM_GUEST_AGENT_READY_TIMEOUT_SECONDS: u64 = 60;
const SHARED_VM_GUEST_CONTROL_VSOCK_PORT: u32 = 47001;
#[cfg(target_os = "macos")]
const SHARED_VM_CONTROL_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);
#[cfg(target_os = "macos")]
const SHARED_VM_DATA_DISK_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
const GUEST_EXEC_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
#[cfg(target_os = "macos")]
const VM_LIFECYCLE_COMPLETION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
#[cfg(target_os = "macos")]
const VM_SAVE_RESTORE_COMPLETION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
const GUEST_EXEC_CONNECT_RETRY_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(100);
// Real shared-VM guest control transport still truncates streamed stdin around the
// 4 KiB-class frame budget during live `tar -xf -` imports, so keep payload chunks
// well below that empirical limit on both sides of the relay.
const AVF_EXEC_STREAM_FRAME_MAX_PAYLOAD: usize = 1024;
const GUEST_EXEC_TTY_RESIZE_POLL_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(100);
const SHARED_VM_SHUTDOWN_WAIT_TIMEOUT: Duration = Duration::from_secs(135);
const DEFAULT_PTY_COLS: u16 = 80;
const DEFAULT_PTY_ROWS: u16 = 24;
const REQUIRED_SHARED_VM_KERNEL_CMDLINE_BASE_TOKENS: &[&str] =
    &["systemd.mask=systemd-networkd-wait-online.service"];

#[derive(Debug, Clone, Serialize)]
struct AvfLinuxHelperProbe {
    protocol_version: u32,
    protocol_schema: &'static str,
    helper_version: String,
    exact_version: String,
    build_id: String,
    compatibility_token: String,
    host_os: &'static str,
    host_arch: &'static str,
    supported: bool,
    save_restore_supported: bool,
    save_restore_capability_scope: AvfLinuxSaveRestoreCapabilityScope,
    rosetta_supported: bool,
    notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AvfLinuxSharedVmLifecycleState {
    Missing,
    Stopped,
    Starting,
    Running,
    Error,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AvfLinuxRuntimeLayoutStatus {
    Prepared,
    AlreadyPresent,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AvfLinuxSharedVmTransitionStatus {
    Scaffolded,
    Ready,
    Stopped,
    AlreadyStopped,
    Missing,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum AvfLinuxSharedVmStartOutcome {
    AlreadyRunning,
    ColdBoot,
    Restored,
    ColdBootAfterRestoreFailure,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum AvfLinuxSharedVmStopOutcome {
    SavedStateWritten,
    ColdStop,
    ColdStopAfterSaveFailure,
    ColdStopSaveUnsupported,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum AvfLinuxSaveRestoreCapabilityScope {
    HostPrerequisitesOnly,
    Unsupported,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum AvfLinuxGuestWorktreeStatus {
    Prepared,
    AlreadyPresent,
}

#[derive(Debug, Clone, Serialize)]
struct AvfLinuxRuntimeLayout {
    protocol_version: u32,
    protocol_schema: &'static str,
    vm_root: PathBuf,
    logs_root: PathBuf,
    state_path: PathBuf,
    layout_status: AvfLinuxRuntimeLayoutStatus,
    notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct AvfLinuxSharedVmStateResponse {
    protocol_version: u32,
    protocol_schema: &'static str,
    state: AvfLinuxSharedVmLifecycleState,
    vm_root: PathBuf,
    logs_root: PathBuf,
    state_path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    log_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    saved_state_path: Option<PathBuf>,
    saved_state_exists: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime_root: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rootfs_image: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    kernel_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    initrd_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime_shape_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    writable_surface_contract_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_saved_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_stopped_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transition_status: Option<AvfLinuxSharedVmTransitionStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_start_outcome: Option<AvfLinuxSharedVmStartOutcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_stop_outcome: Option<AvfLinuxSharedVmStopOutcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_restore_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_save_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    relay_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    guest_agent_pid: Option<u32>,
    simulated: bool,
    notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedSharedVmState {
    state: AvfLinuxSharedVmLifecycleState,
    #[serde(default)]
    guest_identity: PersistedGuestIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    runtime_root: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rootfs_image: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kernel_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    initrd_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    runtime_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    runtime_shape_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    writable_surface_contract_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_saved_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_stopped_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transition_status: Option<AvfLinuxSharedVmTransitionStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_start_outcome: Option<AvfLinuxSharedVmStartOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_stop_outcome: Option<AvfLinuxSharedVmStopOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_restore_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_save_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    relay_pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    guest_agent_pid: Option<u32>,
    #[serde(default)]
    simulated: bool,
    #[serde(default)]
    notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum PersistedGuestPlatform {
    Linux,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum PersistedIsolationKind {
    Container,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum PersistedGuestRuntime {
    Ubuntu,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
struct PersistedGuestIdentity {
    platform: PersistedGuestPlatform,
    isolation_kind: PersistedIsolationKind,
    runtime: PersistedGuestRuntime,
}

impl Default for PersistedGuestIdentity {
    fn default() -> Self {
        supported_guest_identity()
    }
}

#[derive(Debug, Clone, Serialize)]
struct AvfLinuxGuestWorktreeResponse {
    protocol_version: u32,
    protocol_schema: &'static str,
    workspace_id: String,
    worktree_id: String,
    guest_root: PathBuf,
    guest_user: String,
    host_shadow_root: PathBuf,
    metadata_path: PathBuf,
    status: AvfLinuxGuestWorktreeStatus,
    simulated: bool,
    notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedGuestWorktreeState {
    workspace_id: String,
    worktree_id: String,
    #[serde(default)]
    guest_identity: PersistedGuestIdentity,
    host_workspace_root: PathBuf,
    guest_root: PathBuf,
    #[serde(default)]
    guest_user: String,
    host_shadow_root: PathBuf,
    base_commit_sha: String,
    branch_name: String,
    updated_at: String,
    #[serde(default = "default_persisted_guest_worktree_simulated")]
    simulated: bool,
    #[serde(default)]
    notes: Vec<String>,
}

fn default_persisted_guest_worktree_simulated() -> bool {
    true
}

fn supported_guest_identity() -> PersistedGuestIdentity {
    PersistedGuestIdentity {
        platform: PersistedGuestPlatform::Linux,
        isolation_kind: PersistedIsolationKind::Container,
        runtime: PersistedGuestRuntime::Ubuntu,
    }
}

fn ensure_supported_guest_identity(identity: PersistedGuestIdentity) -> Result<()> {
    if identity != supported_guest_identity() {
        bail!(
            "unsupported persisted guest identity {}; only linux + container + ubuntu is enabled",
            persisted_guest_identity_label(identity)
        );
    }
    Ok(())
}

fn persisted_guest_identity_label(identity: PersistedGuestIdentity) -> String {
    format!(
        "{:?} + {:?} + {:?}",
        identity.platform, identity.isolation_kind, identity.runtime
    )
    .to_ascii_lowercase()
}

fn main() {
    if let Err(err) = cli::run() {
        eprintln!("{err:#}");
        std::process::exit(1);
    }
}

fn now_timestamp_string() -> String {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs().to_string(),
        Err(_) => "0".to_string(),
    }
}
