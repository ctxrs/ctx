use serde::{Deserialize, Serialize};

pub use ctx_sandbox_contract::{
    default_container_machine_host_pressure_swap_threshold_mb,
    default_container_machine_idle_shutdown_seconds, default_container_mount_mode_for_runtime,
    default_container_runtime_kind, normalize_container_execution_settings,
    normalize_container_machine_idle_shutdown_seconds, normalize_container_machine_settings,
    ContainerExecutionSettings, ContainerMachineMemoryProfile, ContainerMachineSettings,
    ContainerMountMode, ContainerNetworkMode, ContainerRuntimeKind, ExecutionMode,
    ExecutionSettings, MIN_CONTAINER_MACHINE_IDLE_SHUTDOWN_SECONDS,
};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Settings {
    #[serde(default)]
    pub dictation: Option<DictationSettings>,
    #[serde(default)]
    pub telemetry: Option<TelemetrySettings>,
    #[serde(default)]
    pub title_generation: Option<TitleGenerationSettings>,
    #[serde(default)]
    pub oracle: Option<OracleSettings>,
    #[serde(default)]
    pub resource_governance: Option<ResourceGovernanceSettings>,
    #[serde(default)]
    pub provider_guard: Option<ProviderGuardSettings>,
    #[serde(default)]
    pub tool_limits: Option<ToolLimitsSettings>,
    #[serde(default)]
    pub provider_restart: Option<ProviderRestartSettings>,
    #[serde(default)]
    pub subagents: Option<SubagentSettings>,
    #[serde(default)]
    pub sandboxing: Option<SandboxingSettings>,
    #[serde(default)]
    pub storage: Option<StorageSettings>,
    #[serde(default)]
    pub execution: Option<ExecutionSettings>,
    #[serde(default)]
    pub network_profiles: Option<NetworkProfilesSettings>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StorageSettings {
    #[serde(default)]
    pub max_connections: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DictationSettings {
    pub enabled: bool,
    pub provider: DictationProvider,
    #[serde(default)]
    pub livekit: Option<LiveKitDictationSettings>,
}

impl Default for DictationSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            provider: DictationProvider::LiveKitInference,
            livekit: Some(LiveKitDictationSettings::default()),
        }
    }
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveKitDictationSettings {
    pub base_url: String,
    pub api_key: String,
    #[serde(default)]
    pub api_secret: Option<String>,
    pub model: String,
    pub language: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TitleGenerationMode {
    #[default]
    Remote,
    Local,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TitleGenerationRemoteSettings {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    #[serde(default)]
    pub use_json: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TitleGenerationLocalSettings {
    pub model_id: String,
    #[serde(default)]
    pub use_json: bool,
}

impl Default for TitleGenerationLocalSettings {
    fn default() -> Self {
        Self {
            model_id: DEFAULT_TITLE_GENERATION_LOCAL_MODEL_ID.to_string(),
            use_json: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TitleGenerationSettings {
    pub mode: TitleGenerationMode,
    #[serde(default)]
    pub remote: TitleGenerationRemoteSettings,
    #[serde(default)]
    pub local: TitleGenerationLocalSettings,
}

impl Default for TitleGenerationSettings {
    fn default() -> Self {
        Self {
            mode: TitleGenerationMode::Remote,
            remote: TitleGenerationRemoteSettings::default(),
            local: TitleGenerationLocalSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleSettings {
    pub enabled: bool,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

impl Default for OracleSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: String::new(),
            model: "gpt-5.2-pro".to_string(),
            reasoning_effort: Some("high".to_string()),
            max_output_tokens: None,
            timeout_ms: Some(10 * 60 * 1000),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetrySettings {
    pub enabled: bool,
    #[serde(default = "default_telemetry_endpoint")]
    pub endpoint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProviderControlMode {
    #[default]
    Full,
    HarnessNative,
    CtxEnforced,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxingSettings {
    pub provider_control_mode: ProviderControlMode,
}

impl Default for SandboxingSettings {
    fn default() -> Self {
        Self {
            provider_control_mode: ProviderControlMode::Full,
        }
    }
}

pub fn normalize_settings_in_place(settings: &mut Settings) {
    if let Some(sandboxing) = settings.sandboxing.as_mut() {
        // Only `full` is currently a real product setting. The other variants
        // stay in the schema as reserved values for future work, but are not
        // meaningfully supported or user-facing today.
        sandboxing.provider_control_mode = ProviderControlMode::Full;
    }
    if let Some(execution) = settings.execution.as_mut() {
        normalize_container_execution_settings(&mut execution.container);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum NetworkContext {
    AgentDefault,
    MergeQueue,
    WorktreeSetup,
    UserShell,
}

impl NetworkContext {
    pub fn as_str(&self) -> &'static str {
        match self {
            NetworkContext::AgentDefault => "agent_default",
            NetworkContext::MergeQueue => "merge_queue",
            NetworkContext::WorktreeSetup => "worktree_setup",
            NetworkContext::UserShell => "user_shell",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkProfile {
    pub mode: ContainerNetworkMode,
    #[serde(default)]
    pub allowlist: Vec<String>,
}

impl Default for NetworkProfile {
    fn default() -> Self {
        Self {
            mode: ContainerNetworkMode::LlmOnly,
            allowlist: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkProfilesSettings {
    pub agent_default: NetworkProfile,
    pub merge_queue: NetworkProfile,
    pub worktree_setup: NetworkProfile,
    pub user_shell: NetworkProfile,
}

impl Default for NetworkProfilesSettings {
    fn default() -> Self {
        Self {
            agent_default: NetworkProfile::default(),
            merge_queue: NetworkProfile {
                mode: ContainerNetworkMode::All,
                allowlist: Vec::new(),
            },
            worktree_setup: NetworkProfile {
                mode: ContainerNetworkMode::All,
                allowlist: Vec::new(),
            },
            user_shell: NetworkProfile {
                mode: ContainerNetworkMode::All,
                allowlist: Vec::new(),
            },
        }
    }
}

impl NetworkProfilesSettings {
    pub fn profile(&self, context: NetworkContext) -> NetworkProfile {
        match context {
            NetworkContext::AgentDefault => self.agent_default.clone(),
            NetworkContext::MergeQueue => self.merge_queue.clone(),
            NetworkContext::WorktreeSetup => self.worktree_setup.clone(),
            NetworkContext::UserShell => self.user_shell.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceGovernanceMode {
    Auto,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceGovernanceSettings {
    pub enabled: bool,
    pub mode: ResourceGovernanceMode,
    #[serde(default)]
    pub cpu_quota_pct: Option<u32>,
    #[serde(default)]
    pub memory_high_mb: Option<u32>,
    #[serde(default)]
    pub memory_max_mb: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderGuardSettings {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolLimitsSettings {
    pub enabled: bool,
    pub mode: ResourceGovernanceMode,
    #[serde(default)]
    pub memory_high_mb: Option<u32>,
    #[serde(default)]
    pub memory_max_mb: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderRestartSettings {
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SubagentSettings {
    #[serde(default)]
    pub max_per_call: Option<u32>,
}

impl Default for ResourceGovernanceSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: ResourceGovernanceMode::Auto,
            cpu_quota_pct: None,
            memory_high_mb: None,
            memory_max_mb: None,
        }
    }
}

impl Default for ProviderGuardSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: ResourceGovernanceMode::Auto,
            memory_high_mb: None,
            memory_max_mb: None,
            interval_ms: None,
            grace_period_ms: None,
        }
    }
}

impl Default for ToolLimitsSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: ResourceGovernanceMode::Auto,
            memory_high_mb: None,
            memory_max_mb: None,
        }
    }
}

impl Default for ProviderRestartSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: ResourceGovernanceMode::Auto,
            memory_high_mb: None,
            memory_max_mb: None,
            interval_ms: None,
            grace_period_ms: None,
        }
    }
}

impl Default for TelemetrySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            endpoint: default_telemetry_endpoint(),
        }
    }
}

impl Default for LiveKitDictationSettings {
    fn default() -> Self {
        Self {
            base_url: "https://agent-gateway.livekit.cloud/v1".to_string(),
            api_key: String::new(),
            api_secret: None,
            model: "auto".to_string(),
            language: "en".to_string(),
        }
    }
}

pub const DEFAULT_TELEMETRY_BASE_URL: &str = "https://api.ctx.rs/functions/v1";
pub const DEFAULT_TITLE_GENERATION_LOCAL_MODEL_ID: &str = "ggml-org/Qwen3-1.7B-GGUF";

pub fn default_telemetry_endpoint() -> String {
    let base = std::env::var("CTX_TELEMETRY_BASE_URL")
        .unwrap_or_else(|_| DEFAULT_TELEMETRY_BASE_URL.to_string());
    format!("{}/telemetry", base.trim_end_matches('/'))
}
