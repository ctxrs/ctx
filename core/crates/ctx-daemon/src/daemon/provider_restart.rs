use std::sync::Arc;

use anyhow::Result;
use tokio::sync::{broadcast, Mutex};

use ctx_providers::adapters::ProviderRestartMode;

use crate::daemon::provider_capability_hosts::ProviderLifecycleBackgroundHost;
use ctx_settings_model::{ProviderRestartSettings, ResourceGovernanceMode, Settings};

mod notices;
mod processes;

use notices::notify_sessions;
use processes::signal_pids;

use ctx_provider_runtime::provider_guard::SystemSnapshot;
use ctx_provider_runtime::provider_restart::{
    ProviderRestartConfig, ProviderRestartEvent, ProviderRestartRuntime,
    ResourceGovernanceMode as RestartGovernanceMode,
};
use ctx_provider_runtime::ProviderRuntime;
use ctx_resource_utilization::ResourceSampler;

pub async fn apply_settings_parts(
    providers: &ProviderRuntime,
    resource_sampler: &Mutex<ResourceSampler>,
    settings: &Settings,
) -> Result<()> {
    let cfg = settings.provider_restart.clone().unwrap_or_default();
    let config = map_config(&cfg);
    let system = system_snapshot(resource_sampler).await;
    ctx_provider_runtime::provider_restart::apply_settings_to_runtime(
        providers.provider_restart_runtime(),
        &config,
        &system,
    )
    .await
}

pub(crate) fn spawn_provider_restart(state: Arc<ProviderLifecycleBackgroundHost>) {
    ctx_provider_runtime::provider_restart::spawn_provider_restart(state);
}

#[async_trait::async_trait]
impl ctx_provider_runtime::provider_restart::ProviderRestartHost
    for ProviderLifecycleBackgroundHost
{
    fn provider_restart_runtime(&self) -> &Mutex<ProviderRestartRuntime> {
        self.providers().provider_restart_runtime()
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

    async fn restart_provider(&self, provider_id: &str, pid: u32) {
        let mut needs_kill = true;
        match self
            .providers()
            .restart_provider_adapter_by_id(
                provider_id,
                "provider restart: sustained memory usage",
                ProviderRestartMode::Immediate,
            )
            .await
        {
            ctx_provider_runtime::provider_workers::ProviderAdapterRestartAttempt::Restarted => {
                needs_kill = false
            }
            ctx_provider_runtime::provider_workers::ProviderAdapterRestartAttempt::Failed(err) => {
                tracing::warn!(provider_id, pid, "provider restart hook failed: {err}")
            }
            ctx_provider_runtime::provider_workers::ProviderAdapterRestartAttempt::Missing => {
                tracing::warn!(
                    provider_id,
                    pid,
                    "provider restart failed: provider adapter missing"
                );
            }
        }

        if needs_kill {
            let killed = signal_pids(&[pid], processes::PROCESS_KILL_SIGNAL);
            if killed == 0 {
                tracing::warn!(provider_id, pid, "provider restart failed to kill process");
            }
        }
    }

    async fn on_provider_restart_notice(state: &Arc<Self>, event: ProviderRestartEvent) {
        notify_sessions(state, &event).await;
    }
}

fn map_config(settings: &ProviderRestartSettings) -> ProviderRestartConfig {
    ProviderRestartConfig {
        enabled: settings.enabled,
        mode: Some(match settings.mode {
            ResourceGovernanceMode::Auto => RestartGovernanceMode::Auto,
            ResourceGovernanceMode::Custom => RestartGovernanceMode::Custom,
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
