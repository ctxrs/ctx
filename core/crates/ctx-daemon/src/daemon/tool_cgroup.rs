use anyhow::Result;
use ctx_resource_utilization::tool_limits::{
    apply_limits, compute_effective_limits, public_settings, ToolLimitsApplyOutcome,
};
use ctx_resource_utilization::ResourceSampler;
use tokio::sync::Mutex;

#[cfg(target_os = "linux")]
pub const TOOL_SLICE_UNIT: &str = ctx_resource_utilization::tool_limits::TOOL_SLICE_UNIT;

use crate::daemon::DaemonState;
use ctx_settings_model::{PublicToolLimitsSettings, Settings};

pub async fn build_public_settings(
    state: &DaemonState,
    settings: &Settings,
) -> Option<PublicToolLimitsSettings> {
    build_public_settings_parts(state.telemetry.resource_sampler.as_ref(), settings).await
}

pub async fn build_public_settings_parts(
    resource_sampler: &Mutex<ResourceSampler>,
    settings: &Settings,
) -> Option<PublicToolLimitsSettings> {
    let cfg = settings.tool_limits.as_ref()?;
    let (system, _disks, _cache_age_ms) = {
        let mut sampler = resource_sampler.lock().await;
        sampler.system_snapshot()
    };
    let effective = compute_effective_limits(cfg, &system);
    Some(public_settings(cfg, effective.as_ref()))
}

pub async fn apply_settings(state: &DaemonState, settings: &Settings) -> Result<()> {
    apply_settings_parts(state.telemetry.resource_sampler.as_ref(), settings).await
}

pub async fn apply_settings_parts(
    resource_sampler: &Mutex<ResourceSampler>,
    settings: &Settings,
) -> Result<()> {
    let cfg = settings.tool_limits.clone().unwrap_or_default();
    if !cfg.enabled {
        return Ok(());
    }

    let (system, _disks, _cache_age_ms) = {
        let mut sampler = resource_sampler.lock().await;
        sampler.system_snapshot()
    };
    let Some(limits) = compute_effective_limits(&cfg, &system) else {
        return Ok(());
    };

    match apply_limits(&limits).await? {
        ToolLimitsApplyOutcome::Applied => {}
        ToolLimitsApplyOutcome::Unsupported => {
            tracing::warn!("tool cgroup limits are not supported on this host");
        }
    }
    Ok(())
}
