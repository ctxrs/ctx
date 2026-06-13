use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    #[default]
    Host,
    #[serde(rename = "sandbox", alias = "container")]
    Sandbox,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ContainerRuntimeKind {
    #[default]
    NativeContainer,
    SharedVmContainer,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ContainerMountMode {
    #[default]
    DiskIsolated,
    #[serde(other)]
    Legacy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ContainerNetworkMode {
    #[default]
    LlmOnly,
    Allowlist,
    All,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ContainerMachineMemoryProfile {
    #[default]
    Economy,
    Balanced,
    Performance,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContainerMachineSettings {
    #[serde(default)]
    pub memory_profile: ContainerMachineMemoryProfile,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_memory_mb: Option<u32>,
    #[serde(
        default = "default_container_machine_idle_shutdown_seconds",
        deserialize_with = "deserialize_container_machine_idle_shutdown_seconds"
    )]
    pub idle_shutdown_seconds: u64,
    #[serde(default = "default_container_machine_host_pressure_swap_threshold_mb")]
    pub host_pressure_swap_threshold_mb: u32,
}

impl Default for ContainerMachineSettings {
    fn default() -> Self {
        Self {
            memory_profile: ContainerMachineMemoryProfile::Economy,
            custom_memory_mb: None,
            idle_shutdown_seconds: default_container_machine_idle_shutdown_seconds(),
            host_pressure_swap_threshold_mb:
                default_container_machine_host_pressure_swap_threshold_mb(),
        }
    }
}

pub const MIN_CONTAINER_MACHINE_IDLE_SHUTDOWN_SECONDS: u64 = 60;

pub const fn default_container_machine_idle_shutdown_seconds() -> u64 {
    60 * 60
}

fn deserialize_container_machine_idle_shutdown_seconds<'de, D>(
    deserializer: D,
) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = u64::deserialize(deserializer)?;
    Ok(normalize_container_machine_idle_shutdown_seconds(value))
}

pub const fn default_container_machine_host_pressure_swap_threshold_mb() -> u32 {
    1024
}

pub const fn normalize_container_machine_idle_shutdown_seconds(value: u64) -> u64 {
    if value < MIN_CONTAINER_MACHINE_IDLE_SHUTDOWN_SECONDS {
        MIN_CONTAINER_MACHINE_IDLE_SHUTDOWN_SECONDS
    } else {
        value
    }
}

pub fn normalize_container_machine_settings(machine: &mut ContainerMachineSettings) {
    machine.idle_shutdown_seconds =
        normalize_container_machine_idle_shutdown_seconds(machine.idle_shutdown_seconds);
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerExecutionSettings {
    pub runtime: ContainerRuntimeKind,
    pub mount_mode: ContainerMountMode,
    pub network_mode: ContainerNetworkMode,
    #[serde(default)]
    pub allowlist: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default)]
    pub machine: ContainerMachineSettings,
}

pub fn default_container_runtime_kind() -> ContainerRuntimeKind {
    #[cfg(target_os = "macos")]
    {
        ContainerRuntimeKind::SharedVmContainer
    }
    #[cfg(not(target_os = "macos"))]
    {
        ContainerRuntimeKind::NativeContainer
    }
}

pub fn default_container_mount_mode_for_runtime(
    runtime: ContainerRuntimeKind,
) -> ContainerMountMode {
    match runtime {
        ContainerRuntimeKind::SharedVmContainer => ContainerMountMode::DiskIsolated,
        ContainerRuntimeKind::NativeContainer => ContainerMountMode::DiskIsolated,
    }
}

pub fn normalize_container_execution_settings(settings: &mut ContainerExecutionSettings) {
    if !matches!(settings.mount_mode, ContainerMountMode::DiskIsolated) {
        settings.mount_mode = ContainerMountMode::DiskIsolated;
    }
    normalize_container_machine_settings(&mut settings.machine);
}

impl Default for ContainerExecutionSettings {
    fn default() -> Self {
        let runtime = default_container_runtime_kind();
        let mount_mode = default_container_mount_mode_for_runtime(runtime.clone());
        Self {
            runtime,
            mount_mode,
            network_mode: ContainerNetworkMode::LlmOnly,
            allowlist: Vec::new(),
            image: None,
            machine: ContainerMachineSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionSettings {
    pub mode: ExecutionMode,
    #[serde(default)]
    pub container: ContainerExecutionSettings,
}

impl Default for ExecutionSettings {
    fn default() -> Self {
        Self {
            mode: ExecutionMode::Host,
            container: ContainerExecutionSettings::default(),
        }
    }
}
