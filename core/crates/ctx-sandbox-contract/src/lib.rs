mod binding;
mod execution;
mod layout;
mod substrate;

pub use binding::{
    sandbox_execution_settings_from_binding, SANDBOX_BINDING_EXECUTION_SETTINGS_SCHEMA_V1,
};
pub use execution::{
    default_container_machine_host_pressure_swap_threshold_mb,
    default_container_machine_idle_shutdown_seconds, default_container_mount_mode_for_runtime,
    default_container_runtime_kind, normalize_container_execution_settings,
    normalize_container_machine_idle_shutdown_seconds, normalize_container_machine_settings,
    ContainerExecutionSettings, ContainerMachineMemoryProfile, ContainerMachineSettings,
    ContainerMountMode, ContainerNetworkMode, ContainerRuntimeKind, ExecutionMode,
    ExecutionSettings, MIN_CONTAINER_MACHINE_IDLE_SHUTDOWN_SECONDS,
};
pub use layout::{
    container_worktree_root, live_workspace_root_for_mode, live_worktree_root_for_mode,
    map_host_or_live_path_to_live_roots, sandbox_workspace_root, sandbox_worktree_root,
    shared_vm_guest_host_share_path, shared_vm_guest_host_share_root, CTX_CONTAINER_WORKSPACE_ROOT,
    SHARED_VM_GUEST_HOST_DATA_ROOT,
};
pub use substrate::{guest_identity_label, UbuntuSandboxSubstrate};
