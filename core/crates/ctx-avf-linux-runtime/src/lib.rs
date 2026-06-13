use std::path::Path;

use anyhow::{Context, Result};
use sha2::Digest;
use tokio::io::AsyncReadExt;

mod avf_linux_vm;
mod lifecycle_manager;
mod shared_vm_orchestrator;
mod substrate;
#[cfg(test)]
mod test_support;

pub use avf_linux_vm::{
    build_guest_exec_command, ensure_guest_worktree_from_host_copy,
    ensure_shared_vm_ready_with_observer, ensure_workspace_vm_ready_with_observer, helper_path,
    prefetch_runtime_with_observer, probe_helper, run_guest_exec_capture, runtime_available,
    runtime_state, runtime_target_label, shared_vm_is_launch_ready, start_workspace_vm,
    stop_shared_vm, workspace_vm_data_root, workspace_vm_state, AvfLinuxGuestRuntime,
    AvfLinuxGuestWorktree, AvfLinuxGuestWorktreeStatus, AvfLinuxHelperProbe, AvfLinuxRuntimeLayout,
    AvfLinuxRuntimeLayoutStatus, AvfLinuxSharedVmLifecycleState, AvfLinuxSharedVmStartOutcome,
    AvfLinuxSharedVmState, AvfLinuxSharedVmStopOutcome, AvfLinuxSharedVmTransitionStatus,
    AVF_LINUX_GUEST_RUNTIME_DIR_ENV, AVF_LINUX_HELPER_PATH_ENV,
};
#[cfg(any(test, feature = "test-support"))]
pub use avf_linux_vm::{
    override_managed_avf_linux_runtime_source_for_test, TestManagedAvfLinuxRuntimeSourceGuard,
};
pub use lifecycle_manager::{SharedSubstrateLifecycleManager, SubstrateLifecycleRecord};
pub use shared_vm_orchestrator::SharedVmLifecycleOrchestrator;
pub use substrate::{
    SubstrateShutdownOutcome, SubstrateShutdownReason, SubstrateStartupOutcome,
    SubstrateStartupReason, SubstrateStartupSelection,
};

pub(crate) use ctx_harness_setup::{observe_log, observe_phase};
pub use ctx_harness_setup::{
    HarnessSetupDownloadStatus, HarnessSetupLogLevel, HarnessSetupObserver, HarnessSetupPhase,
    HarnessSetupProgressUpdate,
};
pub use ctx_sandbox_container_runtime::{
    default_container_image, CTX_HARNESS_SANDBOX_CLI_PATH_ENV,
};
pub use ctx_sandbox_contract::{
    ContainerExecutionSettings, ContainerRuntimeKind, UbuntuSandboxSubstrate,
};

pub(crate) async fn sha256_hex_file(path: &Path) -> Result<String> {
    let mut file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = [0_u8; 8192];
    loop {
        let read = file
            .read(&mut buf)
            .await
            .with_context(|| format!("reading {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}
