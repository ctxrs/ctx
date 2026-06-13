use super::*;

pub(super) fn shared_vm_boot_root(data_root: &Path) -> PathBuf {
    shared_vm_root(data_root).join(SHARED_VM_BOOT_DIR)
}

pub(super) fn shared_vm_boot_kernel_path(data_root: &Path) -> PathBuf {
    shared_vm_boot_root(data_root).join(SHARED_VM_BOOT_KERNEL_FILE)
}

pub(super) fn shared_vm_disk_root(data_root: &Path) -> PathBuf {
    shared_vm_root(data_root).join(SHARED_VM_DISK_DIR)
}

pub(super) fn shared_vm_rootfs_path(data_root: &Path) -> PathBuf {
    shared_vm_disk_root(data_root).join(SHARED_VM_ROOTFS_FILE)
}

pub(super) fn shared_vm_data_disk_path(data_root: &Path) -> PathBuf {
    shared_vm_disk_root(data_root).join(SHARED_VM_DATA_DISK_FILE)
}

pub(super) fn shared_vm_guest_agent_helper_path(runtime_root: &Path) -> PathBuf {
    runtime_root
        .join("helpers")
        .join(AVF_LINUX_GUEST_AGENT_HELPER)
}

pub(super) fn shared_vm_egress_proxy_helper_path(runtime_root: &Path) -> PathBuf {
    runtime_root
        .join("helpers")
        .join(AVF_LINUX_EGRESS_PROXY_HELPER)
}

pub(super) fn shared_vm_container_stack_helper_path(runtime_root: &Path) -> PathBuf {
    runtime_root
        .join("helpers")
        .join(AVF_LINUX_CONTAINER_STACK_FILE)
}

pub(super) fn shared_vm_root(data_root: &Path) -> PathBuf {
    data_root
        .join("managed")
        .join("vms")
        .join("avf-linux")
        .join(std::env::consts::OS)
        .join(std::env::consts::ARCH)
        .join("shared")
}

pub(super) fn shared_vm_logs_root(data_root: &Path) -> PathBuf {
    shared_vm_root(data_root).join("logs")
}

pub(super) fn shared_vm_state_path(data_root: &Path) -> PathBuf {
    shared_vm_root(data_root).join(SHARED_VM_STATE_FILE)
}

pub(super) fn shared_vm_log_path(data_root: &Path) -> PathBuf {
    shared_vm_logs_root(data_root).join(SHARED_VM_LOG_FILE)
}

pub(super) fn shared_vm_host_private_root(data_root: &Path) -> PathBuf {
    data_root
        .parent()
        .unwrap_or(data_root)
        .join(".ctx-avf-host-private")
        .join(short_hash(data_root))
}

pub(super) fn shared_vm_saved_state_path(data_root: &Path) -> PathBuf {
    shared_vm_host_private_root(data_root).join(SHARED_VM_SAVED_STATE_FILE)
}

pub(super) fn shared_vm_machine_identifier_path(data_root: &Path) -> PathBuf {
    shared_vm_root(data_root).join(SHARED_VM_MACHINE_IDENTIFIER_FILE)
}

pub(super) fn shared_vm_mac_address_path(data_root: &Path) -> PathBuf {
    shared_vm_root(data_root).join(SHARED_VM_MAC_ADDRESS_FILE)
}

pub(super) fn shared_vm_shutdown_request_path(data_root: &Path) -> PathBuf {
    shared_vm_root(data_root).join(SHARED_VM_SHUTDOWN_REQUEST_FILE)
}

pub(super) fn shared_vm_memory_pressure_request_path(data_root: &Path) -> PathBuf {
    shared_vm_root(data_root).join(SHARED_VM_MEMORY_PRESSURE_REQUEST_FILE)
}

pub(super) fn shared_vm_start_lock_path(data_root: &Path) -> PathBuf {
    shared_vm_root(data_root).join(SHARED_VM_START_LOCK_FILE)
}

pub(super) fn shared_vm_guest_console_log_path(data_root: &Path) -> PathBuf {
    shared_vm_logs_root(data_root).join(SHARED_VM_GUEST_CONSOLE_LOG_FILE)
}

pub(super) fn shared_vm_guest_control_ready_path(data_root: &Path) -> PathBuf {
    shared_vm_root(data_root).join(SHARED_VM_GUEST_CONTROL_READY_FILE)
}

pub(super) fn shared_vm_guest_control_failed_path(data_root: &Path) -> PathBuf {
    shared_vm_root(data_root).join(SHARED_VM_GUEST_CONTROL_FAILED_FILE)
}

pub(super) fn shared_vm_guest_agent_log_path(data_root: &Path) -> PathBuf {
    shared_vm_logs_root(data_root).join(SHARED_VM_GUEST_AGENT_LOG_FILE)
}

pub(super) fn shared_vm_guest_host_share_path(
    data_root: &Path,
    host_path: &Path,
) -> Result<PathBuf> {
    ctx_sandbox_contract::shared_vm_guest_host_share_path(data_root, host_path).with_context(|| {
        format!(
            "host path {} did not live under shared data root {}",
            host_path.display(),
            data_root.display()
        )
    })
}

pub(super) fn shared_vm_kernel_cmdline_path(runtime_root: &Path) -> PathBuf {
    runtime_root
        .join("helpers")
        .join(SHARED_VM_KERNEL_CMDLINE_FILE)
}

pub(super) fn shared_vm_cloud_init_root(data_root: &Path) -> PathBuf {
    shared_vm_root(data_root).join(SHARED_VM_CLOUD_INIT_DIR)
}

pub(super) fn shared_vm_cloud_init_meta_data_path(data_root: &Path) -> PathBuf {
    shared_vm_cloud_init_root(data_root).join(SHARED_VM_CLOUD_INIT_META_DATA_FILE)
}

pub(super) fn shared_vm_cloud_init_user_data_path(data_root: &Path) -> PathBuf {
    shared_vm_cloud_init_root(data_root).join(SHARED_VM_CLOUD_INIT_USER_DATA_FILE)
}

pub(super) fn shared_vm_cloud_init_network_config_path(data_root: &Path) -> PathBuf {
    shared_vm_cloud_init_root(data_root).join(SHARED_VM_CLOUD_INIT_NETWORK_CONFIG_FILE)
}

pub(super) fn shared_vm_cloud_init_image_path(data_root: &Path) -> PathBuf {
    shared_vm_cloud_init_root(data_root).join(SHARED_VM_CLOUD_INIT_IMAGE_FILE)
}

pub(super) fn shared_vm_payloads_root(data_root: &Path) -> PathBuf {
    shared_vm_root(data_root).join(SHARED_VM_PAYLOADS_DIR)
}

pub(super) fn shared_vm_container_stack_payload_path(data_root: &Path) -> PathBuf {
    shared_vm_payloads_root(data_root).join(AVF_LINUX_CONTAINER_STACK_FILE)
}

pub(super) fn shared_vm_control_socket_path(data_root: &Path) -> PathBuf {
    shared_vm_control_socket_root().join(format!(
        "{}-{}",
        short_hash(data_root),
        SHARED_VM_CONTROL_SOCKET_FILE
    ))
}

pub(super) fn shared_vm_guest_agent_socket_path(data_root: &Path) -> PathBuf {
    shared_vm_control_socket_root().join(format!(
        "{}-{}",
        short_hash(data_root),
        SHARED_VM_GUEST_AGENT_SOCKET_FILE
    ))
}

pub(super) fn shared_vm_control_socket_root() -> PathBuf {
    PathBuf::from("/tmp").join(shared_vm_control_socket_root_name())
}

#[cfg(unix)]
fn shared_vm_control_socket_root_name() -> String {
    format!("ctxavf-uid-{}", unsafe { libc::geteuid() })
}

#[cfg(not(unix))]
fn shared_vm_control_socket_root_name() -> &'static str {
    "ctxavf"
}

pub(super) fn short_hash(path: &Path) -> String {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.to_string_lossy().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

pub(super) fn shared_vm_worktrees_root(data_root: &Path) -> PathBuf {
    shared_vm_root(data_root).join(GUEST_WORKTREES_DIR)
}

pub(super) fn shared_vm_worktree_root(
    data_root: &Path,
    workspace_id: &str,
    worktree_id: &str,
) -> PathBuf {
    shared_vm_worktrees_root(data_root)
        .join(workspace_id)
        .join(worktree_id)
}

pub(super) fn shared_vm_worktree_shadow_root(
    data_root: &Path,
    workspace_id: &str,
    worktree_id: &str,
) -> PathBuf {
    shared_vm_worktree_root(data_root, workspace_id, worktree_id).join(GUEST_WORKTREE_SHADOW_DIR)
}

pub(super) fn shared_vm_worktree_metadata_path(
    data_root: &Path,
    workspace_id: &str,
    worktree_id: &str,
) -> PathBuf {
    shared_vm_worktree_root(data_root, workspace_id, worktree_id).join(GUEST_WORKTREE_METADATA_FILE)
}

pub(super) fn guest_worktree_root(worktree_id: &str) -> PathBuf {
    PathBuf::from(GUEST_WORKTREES_ROOT).join(worktree_id)
}
