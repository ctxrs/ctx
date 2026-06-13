use serde::Deserialize;

use super::{
    normalize_container_machine_settings, ContainerMachineSettings, ContainerNetworkMode,
    DictationProvider, ExecutionMode, NetworkProfilesSettings, ProviderControlMode,
    ResourceGovernanceMode, Settings, TitleGenerationLocalSettings, TitleGenerationMode,
};

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateSettingsReq {
    #[serde(default)]
    pub dictation: Option<UpdateDictationSettingsReq>,
    #[serde(default)]
    pub telemetry: Option<UpdateTelemetrySettingsReq>,
    #[serde(default)]
    pub title_generation: Option<UpdateTitleGenerationSettingsReq>,
    #[serde(default)]
    pub oracle: Option<UpdateOracleSettingsReq>,
    #[serde(default)]
    pub resource_governance: Option<UpdateResourceGovernanceSettingsReq>,
    #[serde(default)]
    pub provider_guard: Option<UpdateProviderGuardSettingsReq>,
    #[serde(default)]
    pub tool_limits: Option<UpdateToolLimitsSettingsReq>,
    #[serde(default)]
    pub provider_restart: Option<UpdateProviderRestartSettingsReq>,
    #[serde(default)]
    pub subagents: Option<UpdateSubagentSettingsReq>,
    #[serde(default)]
    pub sandboxing: Option<UpdateSandboxingSettingsReq>,
    #[serde(default)]
    pub execution: Option<UpdateExecutionSettingsReq>,
    #[serde(default)]
    pub network_profiles: Option<NetworkProfilesSettings>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateDictationSettingsReq {
    pub enabled: bool,
    pub provider: DictationProvider,
    #[serde(default)]
    pub livekit: Option<UpdateLiveKitDictationSettingsReq>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateLiveKitDictationSettingsReq {
    pub base_url: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub api_secret: Option<String>,
    pub model: String,
    pub language: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateTelemetrySettingsReq {
    pub enabled: bool,
    pub endpoint: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateTitleGenerationSettingsReq {
    #[serde(default)]
    pub mode: TitleGenerationMode,
    #[serde(default)]
    pub remote: UpdateTitleGenerationRemoteSettingsReq,
    #[serde(default)]
    pub local: TitleGenerationLocalSettings,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct UpdateTitleGenerationRemoteSettingsReq {
    pub base_url: String,
    #[serde(default)]
    pub api_key: Option<String>,
    pub model: String,
    #[serde(default)]
    pub use_json: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateOracleSettingsReq {
    pub enabled: bool,
    pub base_url: String,
    #[serde(default)]
    pub api_key: Option<String>,
    pub model: String,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateResourceGovernanceSettingsReq {
    pub enabled: bool,
    pub mode: ResourceGovernanceMode,
    #[serde(default)]
    pub cpu_quota_pct: Option<u32>,
    #[serde(default)]
    pub memory_high_mb: Option<u32>,
    #[serde(default)]
    pub memory_max_mb: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateProviderGuardSettingsReq {
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
pub struct UpdateToolLimitsSettingsReq {
    pub enabled: bool,
    pub mode: ResourceGovernanceMode,
    #[serde(default)]
    pub memory_high_mb: Option<u32>,
    #[serde(default)]
    pub memory_max_mb: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateProviderRestartSettingsReq {
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
pub struct UpdateSubagentSettingsReq {
    #[serde(default)]
    pub max_per_call: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateSandboxingSettingsReq {
    pub provider_control_mode: ProviderControlMode,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateExecutionSettingsReq {
    pub mode: ExecutionMode,
    #[serde(default)]
    pub container: UpdateContainerExecutionSettingsReq,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct UpdateContainerExecutionSettingsReq {
    pub network_mode: ContainerNetworkMode,
    #[serde(default)]
    pub allowlist: Vec<String>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub machine: ContainerMachineSettings,
}

pub fn apply_update(mut current: Settings, req: UpdateSettingsReq) -> Settings {
    if let Some(d) = req.dictation {
        let mut next = current.dictation.unwrap_or_default();
        next.enabled = d.enabled;
        next.provider = d.provider;
        if let Some(lk) = d.livekit {
            let mut cur_lk = next.livekit.unwrap_or_default();
            cur_lk.base_url = lk.base_url;
            if let Some(api_key) = lk.api_key {
                if !api_key.trim().is_empty() {
                    cur_lk.api_key = api_key;
                }
            }
            if let Some(secret) = lk.api_secret {
                if !secret.trim().is_empty() {
                    cur_lk.api_secret = Some(secret);
                }
            }
            cur_lk.model = lk.model;
            cur_lk.language = lk.language;
            next.livekit = Some(cur_lk);
        }
        current.dictation = Some(next);
    }
    if let Some(t) = req.telemetry {
        let mut next = current.telemetry.unwrap_or_default();
        next.enabled = t.enabled;
        if !t.endpoint.trim().is_empty() {
            next.endpoint = t.endpoint;
        }
        current.telemetry = Some(next);
    }
    if let Some(t) = req.title_generation {
        let mut next = current.title_generation.unwrap_or_default();
        next.mode = t.mode;
        next.remote.base_url = t.remote.base_url;
        if let Some(api_key) = t.remote.api_key {
            if !api_key.trim().is_empty() {
                next.remote.api_key = api_key;
            }
        }
        next.remote.model = t.remote.model;
        next.remote.use_json = t.remote.use_json;
        next.local = t.local;
        current.title_generation = Some(next);
    }
    if let Some(o) = req.oracle {
        let mut next = current.oracle.unwrap_or_default();
        next.enabled = o.enabled;
        next.base_url = o.base_url;
        if let Some(api_key) = o.api_key {
            next.api_key = api_key;
        }
        next.model = o.model;
        next.reasoning_effort = o.reasoning_effort;
        next.max_output_tokens = o.max_output_tokens;
        next.timeout_ms = o.timeout_ms;
        current.oracle = Some(next);
    }
    if let Some(r) = req.resource_governance {
        let mut next = current.resource_governance.unwrap_or_default();
        next.enabled = r.enabled;
        next.mode = r.mode;
        next.cpu_quota_pct = r.cpu_quota_pct;
        next.memory_high_mb = r.memory_high_mb;
        next.memory_max_mb = r.memory_max_mb;
        current.resource_governance = Some(next);
    }
    if let Some(g) = req.provider_guard {
        let mut next = current.provider_guard.unwrap_or_default();
        next.enabled = g.enabled;
        next.mode = g.mode;
        next.memory_high_mb = g.memory_high_mb;
        next.memory_max_mb = g.memory_max_mb;
        next.interval_ms = g.interval_ms;
        next.grace_period_ms = g.grace_period_ms;
        current.provider_guard = Some(next);
    }
    if let Some(t) = req.tool_limits {
        let mut next = current.tool_limits.unwrap_or_default();
        next.enabled = t.enabled;
        next.mode = t.mode;
        next.memory_high_mb = t.memory_high_mb;
        next.memory_max_mb = t.memory_max_mb;
        current.tool_limits = Some(next);
    }
    if let Some(r) = req.provider_restart {
        let mut next = current.provider_restart.unwrap_or_default();
        next.enabled = r.enabled;
        next.mode = r.mode;
        next.memory_high_mb = r.memory_high_mb;
        next.memory_max_mb = r.memory_max_mb;
        next.interval_ms = r.interval_ms;
        next.grace_period_ms = r.grace_period_ms;
        current.provider_restart = Some(next);
    }
    if let Some(s) = req.subagents {
        let mut next = current.subagents.unwrap_or_default();
        next.max_per_call = s.max_per_call;
        current.subagents = Some(next);
    }
    if let Some(s) = req.sandboxing {
        let mut next = current.sandboxing.unwrap_or_default();
        next.provider_control_mode = s.provider_control_mode;
        current.sandboxing = Some(next);
    }
    if let Some(e) = req.execution {
        let mut next = current.execution.unwrap_or_default();
        next.mode = e.mode;
        next.container.network_mode = e.container.network_mode;
        next.container.allowlist = e.container.allowlist;
        next.container.image = e.container.image;
        next.container.machine = e.container.machine;
        normalize_container_machine_settings(&mut next.container.machine);
        current.execution = Some(next);
    }
    if let Some(p) = req.network_profiles {
        current.network_profiles = Some(p);
    }
    current
}
