use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderOptions {
    pub provider_id: String,
    pub workspace_id: String,
    #[serde(default)]
    pub installed: Option<bool>,
    #[serde(default)]
    pub probe_ok: Option<bool>,
    #[serde(default)]
    pub probe_error: Option<String>,
    #[serde(default)]
    pub supports_load: bool,
    #[serde(default)]
    pub auth_required: bool,
    #[serde(default)]
    pub auth_methods: Option<Value>,
    #[serde(default)]
    pub modes: Option<Value>,
    #[serde(default)]
    pub models: Option<Value>,
    #[serde(default)]
    pub verify: Option<ProviderAuthCheck>,
    #[serde(default)]
    pub probed_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderAuthCheck {
    pub provider_id: String,
    pub workspace_id: String,
    pub status: String,
    #[serde(default)]
    pub auth_required: Option<bool>,
    #[serde(default)]
    pub auth_methods: Option<Value>,
    #[serde(default)]
    pub checked_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallStartResponse {
    pub provider_id: String,
    pub install_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TitleGenerationLocalRuntimeStatus {
    pub version: String,
    pub installed: bool,
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TitleGenerationLocalModelStatus {
    pub model_id: String,
    pub file_name: String,
    pub installed: bool,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub size_bytes: Option<u64>,
    #[serde(default)]
    pub installed_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TitleGenerationLocalStatus {
    pub ready: bool,
    pub runtime: TitleGenerationLocalRuntimeStatus,
    pub model: TitleGenerationLocalModelStatus,
    #[serde(default)]
    pub install_id: Option<String>,
    #[serde(default)]
    pub install_running: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TitleGenerationLocalInstallResponse {
    pub install_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallEventLevel {
    Info,
    Warning,
    Error,
    Success,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallProgressEvent {
    pub install_id: String,
    pub provider_id: String,
    pub at: String,
    pub stage: String,
    pub message: String,
    pub level: InstallEventLevel,
    #[serde(default)]
    pub bytes: Option<u64>,
    #[serde(default)]
    pub total_bytes: Option<u64>,
    #[serde(default)]
    pub attempt: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallStateKind {
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallInfo {
    pub install_id: String,
    pub provider_id: String,
    pub state: InstallStateKind,
    pub started_at: String,
    #[serde(default)]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub last_event: Option<InstallProgressEvent>,
}
