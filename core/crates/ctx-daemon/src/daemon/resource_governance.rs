use anyhow::Result;
use tokio::sync::Mutex;

use ctx_provider_runtime::ProviderRuntime;
use ctx_resource_utilization::resource_governance::{
    apply_limits, compute_effective_limits, public_settings, status_for, ResourceGovernanceRuntime,
};
use ctx_resource_utilization::ResourceSampler;
use ctx_transport_runtime::terminals::TerminalManager;

use crate::daemon::DaemonState;
use ctx_settings_model::{
    PublicResourceGovernanceSettings, ResourceGovernanceStatusState, Settings,
};

pub async fn apply_settings(state: &DaemonState, settings: &Settings) -> Result<()> {
    apply_settings_parts(
        state.telemetry.resource_sampler.as_ref(),
        state.telemetry.resource_governance.as_ref(),
        state.providers.as_ref(),
        state.transport.terminals.as_ref(),
        settings,
    )
    .await
}

pub async fn apply_settings_parts(
    resource_sampler: &Mutex<ResourceSampler>,
    resource_governance: &Mutex<ResourceGovernanceRuntime>,
    providers: &ProviderRuntime,
    terminals: &TerminalManager,
    settings: &Settings,
) -> Result<()> {
    let cfg = settings.resource_governance.clone().unwrap_or_default();
    let (system, _disks, _cache_age_ms) = {
        let mut sampler = resource_sampler.lock().await;
        sampler.system_snapshot()
    };
    let cpu_count = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let effective = compute_effective_limits(&cfg, &system, cpu_count);

    let mut runtime = if let Some(limits) = effective.as_ref() {
        let has_running_children = has_running_children(providers, terminals).await;
        apply_limits(std::process::id(), limits, has_running_children).await
    } else {
        ResourceGovernanceRuntime::default()
    };

    if !cfg.enabled {
        runtime.last_state = ResourceGovernanceStatusState::Disabled;
        runtime.last_message = None;
        runtime.last_applied = None;
        runtime.requires_restart = false;
    }

    let mut guard = resource_governance.lock().await;
    *guard = runtime;
    Ok(())
}

pub async fn build_public_settings(
    state: &DaemonState,
    settings: &Settings,
) -> Option<PublicResourceGovernanceSettings> {
    build_public_settings_parts(
        state.telemetry.resource_sampler.as_ref(),
        state.telemetry.resource_governance.as_ref(),
        settings,
    )
    .await
}

pub async fn build_public_settings_parts(
    resource_sampler: &Mutex<ResourceSampler>,
    resource_governance: &Mutex<ResourceGovernanceRuntime>,
    settings: &Settings,
) -> Option<PublicResourceGovernanceSettings> {
    let cfg = settings.resource_governance.as_ref()?;
    let (system, _disks, _cache_age_ms) = {
        let mut sampler = resource_sampler.lock().await;
        sampler.system_snapshot()
    };
    let cpu_count = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let effective = compute_effective_limits(cfg, &system, cpu_count);
    let runtime = resource_governance.lock().await.clone();
    let status = status_for(cfg.enabled, effective.as_ref(), &runtime);
    Some(public_settings(cfg, effective.as_ref(), status))
}

async fn has_running_children(providers: &ProviderRuntime, terminals: &TerminalManager) -> bool {
    providers.has_running_provider_processes().await || terminals.has_running().await
}
