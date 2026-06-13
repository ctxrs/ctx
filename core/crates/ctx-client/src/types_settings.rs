use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContainerRuntimeKind {
    NativeContainer,
    SharedVmContainer,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderControlMode {
    Full,
    HarnessNative,
    CtxEnforced,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    Host,
    #[serde(rename = "sandbox", alias = "container")]
    Sandbox,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContainerMountMode {
    DiskIsolated,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContainerNetworkMode {
    LlmOnly,
    Allowlist,
    All,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContainerMachineMemoryProfile {
    Economy,
    Balanced,
    Performance,
    Custom,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PublicSettings {
    #[serde(default)]
    pub dictation: Option<PublicDictationSettings>,
    #[serde(default)]
    pub telemetry: Option<PublicTelemetrySettings>,
    #[serde(default)]
    pub title_generation: Option<PublicTitleGenerationSettings>,
    #[serde(default)]
    pub resource_governance: Option<PublicResourceGovernanceSettings>,
    #[serde(default)]
    pub tool_limits: Option<PublicToolLimitsSettings>,
    #[serde(default)]
    pub provider_restart: Option<PublicProviderRestartSettings>,
    #[serde(default)]
    pub subagents: Option<PublicSubagentSettings>,
    #[serde(default)]
    pub oracle: Option<PublicOracleSettings>,
    #[serde(default)]
    pub provider_guard: Option<PublicProviderGuardSettings>,
    #[serde(default)]
    pub sandboxing: Option<PublicSandboxingSettings>,
    #[serde(default)]
    pub execution: Option<PublicExecutionSettings>,
    #[serde(default)]
    pub network_profiles: Option<PublicNetworkProfilesSettings>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PublicDictationSettings {
    pub enabled: bool,
    pub provider: DictationProvider,
    #[serde(default)]
    pub livekit: Option<PublicLiveKitDictationSettings>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PublicLiveKitDictationSettings {
    pub base_url: String,
    pub api_key_set: bool,
    pub api_secret_set: bool,
    pub model: String,
    pub language: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PublicTelemetrySettings {
    pub enabled: bool,
    pub endpoint: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PublicOracleSettings {
    pub enabled: bool,
    pub base_url: String,
    pub api_key_set: bool,
    pub model: String,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PublicProviderGuardSettings {
    pub enabled: bool,
    pub mode: ResourceGovernanceMode,
    #[serde(default)]
    pub memory_high_mb: Option<u32>,
    #[serde(default)]
    pub memory_max_mb: Option<u32>,
    #[serde(default)]
    pub interval_ms: Option<u64>,
    #[serde(default)]
    pub grace_period_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PublicSandboxingSettings {
    pub provider_control_mode: ProviderControlMode,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PublicExecutionSettings {
    pub mode: ExecutionMode,
    pub container: PublicContainerExecutionSettings,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PublicContainerExecutionSettings {
    pub network_mode: ContainerNetworkMode,
    #[serde(default)]
    pub allowlist: Vec<String>,
    #[serde(default)]
    pub image: Option<String>,
    pub machine: PublicContainerMachineSettings,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PublicContainerMachineSettings {
    pub memory_profile: ContainerMachineMemoryProfile,
    #[serde(default)]
    pub custom_memory_mb: Option<u32>,
    pub idle_shutdown_seconds: u64,
    pub host_pressure_swap_threshold_mb: u32,
    pub target_memory_mb: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PublicNetworkProfilesSettings {
    pub agent_default: NetworkProfile,
    pub merge_queue: NetworkProfile,
    pub worktree_setup: NetworkProfile,
    pub user_shell: NetworkProfile,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NetworkProfile {
    pub mode: ContainerNetworkMode,
    #[serde(default)]
    pub allowlist: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TitleGenerationMode {
    Remote,
    Local,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PublicTitleGenerationRemoteSettings {
    pub base_url: String,
    pub api_key_set: bool,
    pub model: String,
    pub use_json: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateTitleGenerationRemoteSettingsRequest {
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    pub model: String,
    pub use_json: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TitleGenerationLocalSettings {
    pub model_id: String,
    pub use_json: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PublicTitleGenerationSettings {
    pub mode: TitleGenerationMode,
    pub remote: PublicTitleGenerationRemoteSettings,
    pub local: TitleGenerationLocalSettings,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PublicSubagentSettings {
    #[serde(default)]
    pub max_per_call: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DictationProvider {
    Disabled,
    #[serde(rename = "livekit_inference")]
    LiveKitInference,
    #[serde(rename = "tauri_stt")]
    TauriStt,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResourceGovernanceMode {
    Auto,
    Custom,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResourceGovernanceStatusState {
    Disabled,
    Applied,
    Pending,
    Unsupported,
    Error,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PublicResourceGovernanceStatus {
    pub state: ResourceGovernanceStatusState,
    pub can_apply_now: bool,
    pub requires_restart: bool,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PublicResourceGovernanceLimits {
    pub cpu_quota_pct: u32,
    pub memory_high_mb: u32,
    pub memory_max_mb: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PublicResourceGovernanceSettings {
    pub enabled: bool,
    pub mode: ResourceGovernanceMode,
    #[serde(default)]
    pub cpu_quota_pct: Option<u32>,
    #[serde(default)]
    pub memory_high_mb: Option<u32>,
    #[serde(default)]
    pub memory_max_mb: Option<u32>,
    #[serde(default)]
    pub effective: Option<PublicResourceGovernanceLimits>,
    #[serde(default)]
    pub status: Option<PublicResourceGovernanceStatus>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PublicToolLimitsLimits {
    pub memory_high_mb: u32,
    pub memory_max_mb: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PublicToolLimitsSettings {
    pub enabled: bool,
    pub mode: ResourceGovernanceMode,
    #[serde(default)]
    pub memory_high_mb: Option<u32>,
    #[serde(default)]
    pub memory_max_mb: Option<u32>,
    #[serde(default)]
    pub effective: Option<PublicToolLimitsLimits>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PublicProviderRestartSettings {
    pub enabled: bool,
    pub mode: ResourceGovernanceMode,
    #[serde(default)]
    pub memory_high_mb: Option<u32>,
    #[serde(default)]
    pub memory_max_mb: Option<u32>,
    #[serde(default)]
    pub interval_ms: Option<u64>,
    #[serde(default)]
    pub grace_period_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateSettingsRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dictation: Option<UpdateDictationSettingsRequest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<UpdateTelemetrySettingsRequest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_generation: Option<UpdateTitleGenerationSettingsRequest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_governance: Option<UpdateResourceGovernanceSettingsRequest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_guard: Option<UpdateProviderGuardSettingsRequest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_limits: Option<UpdateToolLimitsSettingsRequest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_restart: Option<UpdateProviderRestartSettingsRequest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagents: Option<UpdateSubagentSettingsRequest>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateDictationSettingsRequest {
    pub enabled: bool,
    pub provider: DictationProvider,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub livekit: Option<UpdateLiveKitDictationSettingsRequest>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateLiveKitDictationSettingsRequest {
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_secret: Option<String>,
    pub model: String,
    pub language: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateTelemetrySettingsRequest {
    pub enabled: bool,
    pub endpoint: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateTitleGenerationSettingsRequest {
    pub mode: TitleGenerationMode,
    pub remote: UpdateTitleGenerationRemoteSettingsRequest,
    pub local: TitleGenerationLocalSettings,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateResourceGovernanceSettingsRequest {
    pub enabled: bool,
    pub mode: ResourceGovernanceMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_quota_pct: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_high_mb: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_max_mb: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateProviderGuardSettingsRequest {
    pub enabled: bool,
    pub mode: ResourceGovernanceMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_high_mb: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_max_mb: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grace_period_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateToolLimitsSettingsRequest {
    pub enabled: bool,
    pub mode: ResourceGovernanceMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_high_mb: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_max_mb: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateProviderRestartSettingsRequest {
    pub enabled: bool,
    pub mode: ResourceGovernanceMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_high_mb: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_max_mb: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grace_period_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateSubagentSettingsRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_per_call: Option<u32>,
}
