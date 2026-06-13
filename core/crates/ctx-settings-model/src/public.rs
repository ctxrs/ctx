use serde::Serialize;
use sysinfo::System;

use super::{
    default_telemetry_endpoint, normalize_container_machine_idle_shutdown_seconds,
    ContainerMachineMemoryProfile, ContainerMachineSettings, ContainerNetworkMode,
    DictationProvider, ExecutionMode, NetworkProfile, ProviderControlMode, ResourceGovernanceMode,
    Settings, TitleGenerationLocalSettings, TitleGenerationMode,
};

#[derive(Debug, Clone, Serialize)]
pub struct PublicSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dictation: Option<PublicDictationSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<PublicTelemetrySettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_generation: Option<PublicTitleGenerationSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oracle: Option<PublicOracleSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_governance: Option<PublicResourceGovernanceSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_guard: Option<PublicProviderGuardSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_limits: Option<PublicToolLimitsSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_restart: Option<PublicProviderRestartSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagents: Option<PublicSubagentSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandboxing: Option<PublicSandboxingSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<PublicExecutionSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network_profiles: Option<PublicNetworkProfilesSettings>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicDictationSettings {
    pub enabled: bool,
    pub provider: DictationProvider,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub livekit: Option<PublicLiveKitDictationSettings>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicLiveKitDictationSettings {
    pub base_url: String,
    pub api_key_set: bool,
    pub api_secret_set: bool,
    pub model: String,
    pub language: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicTelemetrySettings {
    pub enabled: bool,
    pub endpoint: String,
    pub source: PublicSettingsSource,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PublicSettingsSource {
    Default,
    Configured,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicTitleGenerationSettings {
    pub mode: TitleGenerationMode,
    pub remote: PublicTitleGenerationRemoteSettings,
    pub local: TitleGenerationLocalSettings,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicTitleGenerationRemoteSettings {
    pub base_url: String,
    pub api_key_set: bool,
    pub model: String,
    pub use_json: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicOracleSettings {
    pub enabled: bool,
    pub base_url: String,
    pub api_key_set: bool,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicSandboxingSettings {
    pub provider_control_mode: ProviderControlMode,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicExecutionSettings {
    pub mode: ExecutionMode,
    pub container: PublicContainerExecutionSettings,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicNetworkProfilesSettings {
    pub agent_default: NetworkProfile,
    pub merge_queue: NetworkProfile,
    pub worktree_setup: NetworkProfile,
    pub user_shell: NetworkProfile,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicContainerExecutionSettings {
    pub network_mode: ContainerNetworkMode,
    pub allowlist: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    pub machine: PublicContainerMachineSettings,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicContainerMachineSettings {
    pub memory_profile: ContainerMachineMemoryProfile,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_memory_mb: Option<u32>,
    pub idle_shutdown_seconds: u64,
    pub host_pressure_swap_threshold_mb: u32,
    pub target_memory_mb: u32,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResourceGovernanceStatusState {
    Disabled,
    Applied,
    Pending,
    Unsupported,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicResourceGovernanceStatus {
    pub state: ResourceGovernanceStatusState,
    pub can_apply_now: bool,
    pub requires_restart: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicResourceGovernanceLimits {
    pub cpu_quota_pct: u32,
    pub memory_high_mb: u32,
    pub memory_max_mb: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicResourceGovernanceSettings {
    pub enabled: bool,
    pub mode: ResourceGovernanceMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_quota_pct: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_high_mb: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_max_mb: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective: Option<PublicResourceGovernanceLimits>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<PublicResourceGovernanceStatus>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicProviderGuardSettings {
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
pub struct PublicToolLimitsLimits {
    pub memory_high_mb: u32,
    pub memory_max_mb: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicToolLimitsSettings {
    pub enabled: bool,
    pub mode: ResourceGovernanceMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_high_mb: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_max_mb: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective: Option<PublicToolLimitsLimits>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicProviderRestartSettings {
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
pub struct PublicSubagentSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_per_call: Option<u32>,
}

const DEFAULT_PRESET_HOST_MEMORY_MB: u32 = 32 * 1024;
const SANDBOX_VM_MEMORY_PRESET_FLOOR_MB: u32 = 4096;
const SANDBOX_VM_MEMORY_ECONOMY_CAP_MB: u32 = 8192;
const SANDBOX_VM_MEMORY_BALANCED_CAP_MB: u32 = 16 * 1024;
const SANDBOX_VM_MEMORY_PERFORMANCE_CAP_MB: u32 = 32 * 1024;
const MI_B: u64 = 1024 * 1024;

fn detected_host_memory_mb() -> Option<u32> {
    #[cfg(test)]
    if let Ok(raw) = std::env::var("CTX_TEST_HOST_MEMORY_MB") {
        if let Ok(value) = raw.parse::<u32>() {
            if value > 0 {
                return Some(value);
            }
        }
    }

    let mut system = System::new();
    system.refresh_memory();
    let total_bytes = system.total_memory();
    let total_mb = total_bytes / MI_B;
    if total_mb == 0 {
        return None;
    }
    u32::try_from(total_mb).ok()
}

fn preset_memory_mb(total_memory_mb: u32, numerator: u32, denominator: u32, cap_mb: u32) -> u32 {
    total_memory_mb
        .saturating_mul(numerator)
        .checked_div(denominator)
        .unwrap_or(SANDBOX_VM_MEMORY_PRESET_FLOOR_MB)
        .clamp(SANDBOX_VM_MEMORY_PRESET_FLOOR_MB, cap_mb)
}

fn resolved_machine_memory_mb(machine: &ContainerMachineSettings) -> u32 {
    let host_memory_mb = detected_host_memory_mb().unwrap_or(DEFAULT_PRESET_HOST_MEMORY_MB);
    match machine.memory_profile {
        ContainerMachineMemoryProfile::Economy => {
            preset_memory_mb(host_memory_mb, 1, 8, SANDBOX_VM_MEMORY_ECONOMY_CAP_MB)
        }
        ContainerMachineMemoryProfile::Balanced => {
            preset_memory_mb(host_memory_mb, 1, 4, SANDBOX_VM_MEMORY_BALANCED_CAP_MB)
        }
        ContainerMachineMemoryProfile::Performance => {
            preset_memory_mb(host_memory_mb, 1, 2, SANDBOX_VM_MEMORY_PERFORMANCE_CAP_MB)
        }
        ContainerMachineMemoryProfile::Custom => machine
            .custom_memory_mb
            .unwrap_or_else(|| {
                preset_memory_mb(host_memory_mb, 1, 4, SANDBOX_VM_MEMORY_BALANCED_CAP_MB)
            })
            .max(1024),
    }
}

fn to_public_container_machine_settings(
    machine: &ContainerMachineSettings,
) -> PublicContainerMachineSettings {
    PublicContainerMachineSettings {
        memory_profile: machine.memory_profile.clone(),
        custom_memory_mb: machine.custom_memory_mb,
        idle_shutdown_seconds: normalize_container_machine_idle_shutdown_seconds(
            machine.idle_shutdown_seconds,
        ),
        host_pressure_swap_threshold_mb: machine.host_pressure_swap_threshold_mb,
        target_memory_mb: resolved_machine_memory_mb(machine),
    }
}

pub(super) fn to_public(settings: &Settings) -> PublicSettings {
    let dictation = settings
        .dictation
        .as_ref()
        .map(|d| PublicDictationSettings {
            enabled: d.enabled,
            provider: d.provider.clone(),
            livekit: d.livekit.as_ref().map(|lk| PublicLiveKitDictationSettings {
                base_url: lk.base_url.clone(),
                api_key_set: !lk.api_key.trim().is_empty(),
                api_secret_set: lk.api_secret.as_ref().is_some_and(|s| !s.trim().is_empty()),
                model: lk.model.clone(),
                language: lk.language.clone(),
            }),
        });
    let telemetry = Some(match settings.telemetry.as_ref() {
        Some(t) => PublicTelemetrySettings {
            enabled: t.enabled,
            endpoint: t.endpoint.clone(),
            source: PublicSettingsSource::Configured,
        },
        None => PublicTelemetrySettings {
            enabled: true,
            endpoint: default_telemetry_endpoint(),
            source: PublicSettingsSource::Default,
        },
    });
    let title_generation =
        settings
            .title_generation
            .as_ref()
            .map(|t| PublicTitleGenerationSettings {
                mode: t.mode.clone(),
                remote: PublicTitleGenerationRemoteSettings {
                    base_url: t.remote.base_url.clone(),
                    api_key_set: !t.remote.api_key.trim().is_empty(),
                    model: t.remote.model.clone(),
                    use_json: t.remote.use_json,
                },
                local: t.local.clone(),
            });
    let oracle = settings.oracle.as_ref().map(|o| PublicOracleSettings {
        enabled: o.enabled,
        base_url: o.base_url.clone(),
        api_key_set: !o.api_key.trim().is_empty(),
        model: o.model.clone(),
        reasoning_effort: o.reasoning_effort.clone(),
        max_output_tokens: o.max_output_tokens,
        timeout_ms: o.timeout_ms,
    });
    let resource_governance =
        settings
            .resource_governance
            .as_ref()
            .map(|r| PublicResourceGovernanceSettings {
                enabled: r.enabled,
                mode: r.mode.clone(),
                cpu_quota_pct: r.cpu_quota_pct,
                memory_high_mb: r.memory_high_mb,
                memory_max_mb: r.memory_max_mb,
                effective: None,
                status: None,
            });
    let provider_guard = settings
        .provider_guard
        .as_ref()
        .map(|g| PublicProviderGuardSettings {
            enabled: g.enabled,
            mode: g.mode.clone(),
            memory_high_mb: g.memory_high_mb,
            memory_max_mb: g.memory_max_mb,
            interval_ms: g.interval_ms,
            grace_period_ms: g.grace_period_ms,
        });
    let tool_limits = settings
        .tool_limits
        .as_ref()
        .map(|t| PublicToolLimitsSettings {
            enabled: t.enabled,
            mode: t.mode.clone(),
            memory_high_mb: t.memory_high_mb,
            memory_max_mb: t.memory_max_mb,
            effective: None,
        });
    let provider_restart =
        settings
            .provider_restart
            .as_ref()
            .map(|p| PublicProviderRestartSettings {
                enabled: p.enabled,
                mode: p.mode.clone(),
                memory_high_mb: p.memory_high_mb,
                memory_max_mb: p.memory_max_mb,
                interval_ms: p.interval_ms,
                grace_period_ms: p.grace_period_ms,
            });
    let subagents = settings.subagents.as_ref().map(|s| PublicSubagentSettings {
        max_per_call: s.max_per_call,
    });
    let sandboxing = settings
        .sandboxing
        .as_ref()
        .map(|s| PublicSandboxingSettings {
            provider_control_mode: s.provider_control_mode.clone(),
        });
    let effective_execution = settings.execution.clone().unwrap_or_default();
    let execution = Some(PublicExecutionSettings {
        mode: effective_execution.mode,
        container: PublicContainerExecutionSettings {
            network_mode: effective_execution.container.network_mode,
            allowlist: effective_execution.container.allowlist,
            image: effective_execution.container.image,
            machine: to_public_container_machine_settings(&effective_execution.container.machine),
        },
    });
    let network_profiles =
        settings
            .network_profiles
            .as_ref()
            .map(|p| PublicNetworkProfilesSettings {
                agent_default: p.agent_default.clone(),
                merge_queue: p.merge_queue.clone(),
                worktree_setup: p.worktree_setup.clone(),
                user_shell: p.user_shell.clone(),
            });
    PublicSettings {
        dictation,
        telemetry,
        title_generation,
        oracle,
        resource_governance,
        provider_guard,
        tool_limits,
        provider_restart,
        subagents,
        sandboxing,
        execution,
        network_profiles,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContainerExecutionSettings, ExecutionSettings, TelemetrySettings};

    static RUNTIME_ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct EnvVarGuard {
        key: &'static str,
        prev: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let prev = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, prev }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(prev) = self.prev.take() {
                std::env::set_var(self.key, prev);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[test]
    fn default_public_machine_memory_uses_automatic_economy_target() {
        let _serial = RUNTIME_ENV_TEST_LOCK.lock().expect("runtime env lock");
        let _host_memory = EnvVarGuard::set("CTX_TEST_HOST_MEMORY_MB", "49152");
        let settings = Settings {
            execution: Some(ExecutionSettings {
                container: ContainerExecutionSettings::default(),
                ..ExecutionSettings::default()
            }),
            ..Settings::default()
        };

        let public = to_public(&settings);
        let machine = public.execution.expect("execution").container.machine;
        assert_eq!(
            machine.memory_profile,
            ContainerMachineMemoryProfile::Economy
        );
        assert_eq!(machine.target_memory_mb, 6144);
    }

    #[test]
    fn public_machine_memory_prefers_explicit_custom_value() {
        let _serial = RUNTIME_ENV_TEST_LOCK.lock().expect("runtime env lock");
        let _host_memory = EnvVarGuard::set("CTX_TEST_HOST_MEMORY_MB", "49152");
        let settings = Settings {
            execution: Some(ExecutionSettings {
                container: ContainerExecutionSettings {
                    machine: ContainerMachineSettings {
                        memory_profile: ContainerMachineMemoryProfile::Custom,
                        custom_memory_mb: Some(28672),
                        ..ContainerMachineSettings::default()
                    },
                    ..ContainerExecutionSettings::default()
                },
                ..ExecutionSettings::default()
            }),
            ..Settings::default()
        };

        let public = to_public(&settings);
        let machine = public.execution.expect("execution").container.machine;
        assert_eq!(machine.target_memory_mb, 28672);
    }

    #[test]
    fn public_machine_memory_uses_runtime_fallback_for_missing_custom_value() {
        let _serial = RUNTIME_ENV_TEST_LOCK.lock().expect("runtime env lock");
        let _host_memory = EnvVarGuard::set("CTX_TEST_HOST_MEMORY_MB", "49152");
        let settings = Settings {
            execution: Some(ExecutionSettings {
                container: ContainerExecutionSettings {
                    machine: ContainerMachineSettings {
                        memory_profile: ContainerMachineMemoryProfile::Custom,
                        custom_memory_mb: None,
                        ..ContainerMachineSettings::default()
                    },
                    ..ContainerExecutionSettings::default()
                },
                ..ExecutionSettings::default()
            }),
            ..Settings::default()
        };

        let public = to_public(&settings);
        let machine = public.execution.expect("execution").container.machine;
        assert_eq!(machine.target_memory_mb, 12288);
    }

    #[test]
    fn public_machine_idle_shutdown_is_clamped_to_minimum() {
        let settings = Settings {
            execution: Some(ExecutionSettings {
                container: ContainerExecutionSettings {
                    machine: ContainerMachineSettings {
                        idle_shutdown_seconds: 5,
                        ..ContainerMachineSettings::default()
                    },
                    ..ContainerExecutionSettings::default()
                },
                ..ExecutionSettings::default()
            }),
            ..Settings::default()
        };

        let public = to_public(&settings);
        let machine = public.execution.expect("execution").container.machine;
        assert_eq!(machine.idle_shutdown_seconds, 60);
    }

    #[test]
    fn public_telemetry_defaults_are_marked_as_default_source() {
        let public = to_public(&Settings::default());
        let telemetry = public.telemetry.expect("telemetry");
        assert!(telemetry.enabled);
        assert_eq!(telemetry.source, PublicSettingsSource::Default);
        assert_eq!(telemetry.endpoint, default_telemetry_endpoint());
    }

    #[test]
    fn public_telemetry_configured_values_are_marked_as_configured_source() {
        let public = to_public(&Settings {
            telemetry: Some(TelemetrySettings {
                enabled: false,
                endpoint: "https://telemetry.example/functions/v1/telemetry".to_string(),
            }),
            ..Settings::default()
        });
        let telemetry = public.telemetry.expect("telemetry");
        assert!(!telemetry.enabled);
        assert_eq!(telemetry.source, PublicSettingsSource::Configured);
        assert_eq!(
            telemetry.endpoint,
            "https://telemetry.example/functions/v1/telemetry"
        );
    }
}
