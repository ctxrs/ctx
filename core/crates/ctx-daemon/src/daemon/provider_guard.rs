use std::sync::Arc;

use anyhow::Result;
use tokio::sync::{broadcast, Mutex};

use crate::daemon::provider_capability_hosts::ProviderLifecycleBackgroundHost;
use ctx_settings_model::{ProviderGuardSettings, ResourceGovernanceMode, Settings};

mod events;
mod snapshot;

use ctx_provider_runtime::provider_guard::{
    ProviderGuardConfig, ProviderGuardRuntime, ResourceGovernanceMode as GuardMode, SystemSnapshot,
};
use ctx_provider_runtime::ProviderRuntime;
use ctx_resource_utilization::ResourceSampler;

pub async fn apply_settings_parts(
    providers: &ProviderRuntime,
    resource_sampler: &Mutex<ResourceSampler>,
    settings: &Settings,
) -> Result<()> {
    let cfg = settings.provider_guard.clone().unwrap_or_default();
    let config = map_config(&cfg);
    let system = system_snapshot(resource_sampler).await;
    ctx_provider_runtime::provider_guard::apply_settings_to_runtime(
        providers.provider_guard_runtime(),
        &config,
        &system,
    )
    .await
}

pub(crate) fn spawn_provider_guard(state: Arc<ProviderLifecycleBackgroundHost>) {
    ctx_provider_runtime::provider_guard::spawn_provider_guard(state);
}

#[async_trait::async_trait]
impl ctx_provider_runtime::provider_guard::ProviderGuardHost for ProviderLifecycleBackgroundHost {
    fn provider_guard_runtime(&self) -> &Mutex<ProviderGuardRuntime> {
        self.providers().provider_guard_runtime()
    }

    fn subscribe_shutdown(&self) -> broadcast::Receiver<()> {
        self.shutdown_tx().subscribe()
    }

    async fn system_snapshot(&self) -> ctx_provider_runtime::provider_guard::SystemSnapshot {
        let (system, _disks, _cache_age_ms) = {
            let mut sampler = self.resource_sampler().lock().await;
            sampler.system_snapshot()
        };
        ctx_provider_runtime::provider_guard::SystemSnapshot {
            memory_total_bytes: system.memory_total_bytes,
            memory_used_bytes: system.memory_used_bytes,
        }
    }

    async fn provider_memory_snapshot(
        &self,
    ) -> Vec<ctx_provider_runtime::provider_guard::ProviderMemorySample> {
        let provider_processes = self.providers().list_provider_processes().await;
        let samples = {
            let mut sampler = self.resource_sampler().lock().await;
            sampler.provider_memory_snapshot(&provider_processes)
        };
        samples
            .into_iter()
            .map(
                |sample| ctx_provider_runtime::provider_guard::ProviderMemorySample {
                    provider_id: sample.provider_id,
                    label: sample.label,
                    pid: sample.pid,
                    memory_bytes: sample.memory_bytes,
                    tool_memory_bytes: sample.tool_memory_bytes,
                },
            )
            .collect()
    }

    async fn on_provider_guard_event(
        state: &Arc<Self>,
        event: ctx_provider_runtime::provider_guard::ProviderGuardEvent,
    ) {
        events::handle_provider_guard_event(state, &event).await;
    }
}

fn map_config(settings: &ProviderGuardSettings) -> ProviderGuardConfig {
    ProviderGuardConfig {
        enabled: settings.enabled,
        mode: Some(match settings.mode {
            ResourceGovernanceMode::Auto => GuardMode::Auto,
            ResourceGovernanceMode::Custom => GuardMode::Custom,
        }),
        memory_high_mb: settings.memory_high_mb,
        memory_max_mb: settings.memory_max_mb,
        interval_ms: settings.interval_ms,
        grace_period_ms: settings.grace_period_ms,
    }
}

async fn system_snapshot(resource_sampler: &Mutex<ResourceSampler>) -> SystemSnapshot {
    let (system, _disks, _cache_age_ms) = {
        let mut sampler = resource_sampler.lock().await;
        sampler.system_snapshot()
    };
    SystemSnapshot {
        memory_total_bytes: system.memory_total_bytes,
        memory_used_bytes: system.memory_used_bytes,
    }
}
