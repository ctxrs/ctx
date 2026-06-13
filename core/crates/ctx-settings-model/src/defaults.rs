use crate::*;

pub fn ensure_settings_defaults(settings: &mut Settings) {
    if settings.resource_governance.is_none() {
        settings.resource_governance = Some(ResourceGovernanceSettings::default());
    }
    if settings.provider_guard.is_none() {
        settings.provider_guard = Some(ProviderGuardSettings::default());
    }
    if settings.tool_limits.is_none() {
        settings.tool_limits = Some(ToolLimitsSettings::default());
    }
    if settings.provider_restart.is_none() {
        settings.provider_restart = Some(ProviderRestartSettings::default());
    }
    if settings.sandboxing.is_none() {
        settings.sandboxing = Some(SandboxingSettings::default());
    }
    if settings.storage.is_none() {
        settings.storage = Some(StorageSettings::default());
    }
    if settings.execution.is_none() {
        settings.execution = Some(ExecutionSettings::default());
    }
    if settings.network_profiles.is_none() {
        settings.network_profiles = Some(NetworkProfilesSettings::default());
    }
}
