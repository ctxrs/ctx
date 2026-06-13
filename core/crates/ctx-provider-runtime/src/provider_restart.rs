use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use tokio::sync::{broadcast, Mutex};

pub use crate::resource_governance::ResourceGovernanceMode;
use crate::resource_governance::{ProviderMemorySample, SystemSnapshot};
use crate::ProviderRuntime;

const DEFAULT_INTERVAL_MS: u64 = 5_000;
const DEFAULT_GRACE_PERIOD_MS: u64 = 300_000;
const DEFAULT_MIN_MEMORY_MB: u64 = 1024;
const DEFAULT_MEMORY_FRACTION: f64 = 0.6;

#[derive(Debug, Clone, Default)]
pub struct ProviderRestartConfig {
    pub enabled: bool,
    pub mode: Option<ResourceGovernanceMode>,
    pub memory_high_mb: Option<u32>,
    pub memory_max_mb: Option<u32>,
    pub interval_ms: Option<u64>,
    pub grace_period_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRestartLimits {
    pub memory_high_mb: u32,
    pub memory_max_mb: u32,
    pub interval: Duration,
    pub grace_period: Duration,
}

#[derive(Debug, Clone, Default)]
pub struct ProviderRestartRuntime {
    pub enabled: bool,
    pub last_applied: Option<ProviderRestartLimits>,
    pub last_message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProviderRestartEvent {
    pub sample: ProviderMemorySample,
    pub kind: &'static str,
    pub stage: &'static str,
    pub limits: ProviderRestartLimits,
    pub system: SystemSnapshot,
    pub restart_at_ms: Option<u64>,
}

#[derive(Debug, Clone)]
struct OverLimitState {
    first_seen: Instant,
    last_seen: Instant,
    last_memory_bytes: u64,
    last_tool_memory_bytes: u64,
    pid: u32,
}

#[async_trait::async_trait]
pub trait ProviderRestartHost: Send + Sync + 'static {
    fn provider_restart_runtime(&self) -> &Mutex<ProviderRestartRuntime>;
    fn subscribe_shutdown(&self) -> broadcast::Receiver<()>;
    async fn system_snapshot(&self) -> SystemSnapshot;
    async fn provider_memory_snapshot(&self) -> Vec<ProviderMemorySample>;
    async fn restart_provider(&self, provider_id: &str, pid: u32);
    async fn on_provider_restart_notice(state: &Arc<Self>, event: ProviderRestartEvent);
}

impl ProviderRuntime {
    pub fn provider_restart_runtime(&self) -> &Mutex<ProviderRestartRuntime> {
        &self.restart
    }
}

pub fn compute_effective_limits(
    settings: &ProviderRestartConfig,
    system: &SystemSnapshot,
) -> Option<ProviderRestartLimits> {
    if !settings.enabled {
        return None;
    }

    let total_mb = (system.memory_total_bytes / (1024 * 1024)).max(1);
    let mode = settings.mode.unwrap_or(ResourceGovernanceMode::Auto);
    let mut memory_max_mb = match mode {
        ResourceGovernanceMode::Auto => {
            ((total_mb as f64) * DEFAULT_MEMORY_FRACTION).round() as u64
        }
        ResourceGovernanceMode::Custom => settings.memory_max_mb.unwrap_or(0) as u64,
    };
    if memory_max_mb == 0 {
        memory_max_mb = ((total_mb as f64) * DEFAULT_MEMORY_FRACTION).round() as u64;
    }
    memory_max_mb = memory_max_mb.max(DEFAULT_MIN_MEMORY_MB);

    let mut memory_high_mb = match mode {
        ResourceGovernanceMode::Auto => ((memory_max_mb as f64) * 0.9).round() as u64,
        ResourceGovernanceMode::Custom => settings.memory_high_mb.unwrap_or(0) as u64,
    };
    if memory_high_mb == 0 {
        memory_high_mb = ((memory_max_mb as f64) * 0.9).round() as u64;
    }
    if memory_high_mb > memory_max_mb {
        memory_high_mb = memory_max_mb;
    }

    let interval_ms = settings.interval_ms.unwrap_or(DEFAULT_INTERVAL_MS).max(100);
    let grace_ms = settings.grace_period_ms.unwrap_or(DEFAULT_GRACE_PERIOD_MS);

    Some(ProviderRestartLimits {
        memory_high_mb: memory_high_mb as u32,
        memory_max_mb: memory_max_mb as u32,
        interval: Duration::from_millis(interval_ms),
        grace_period: Duration::from_millis(grace_ms),
    })
}

pub async fn apply_settings<H>(state: &H, settings: &ProviderRestartConfig) -> Result<()>
where
    H: ProviderRestartHost,
{
    let system = state.system_snapshot().await;
    apply_settings_to_runtime(state.provider_restart_runtime(), settings, &system).await
}

pub async fn apply_settings_to_runtime(
    provider_restart_runtime: &Mutex<ProviderRestartRuntime>,
    settings: &ProviderRestartConfig,
    system: &SystemSnapshot,
) -> Result<()> {
    let effective = compute_effective_limits(settings, system);

    let runtime = ProviderRestartRuntime {
        enabled: settings.enabled,
        last_applied: effective,
        last_message: None,
    };

    let mut guard = provider_restart_runtime.lock().await;
    *guard = runtime;
    Ok(())
}

pub fn spawn_provider_restart<H>(state: Arc<H>)
where
    H: ProviderRestartHost,
{
    tokio::spawn(async move {
        let mut shutdown_rx = state.subscribe_shutdown();
        let mut over_high: HashMap<String, OverLimitState> = HashMap::new();

        loop {
            let (enabled, limits) = {
                let runtime = state.provider_restart_runtime().lock().await;
                (runtime.enabled, runtime.last_applied.clone())
            };

            if !enabled || limits.is_none() {
                over_high.clear();
                tokio::select! {
                    _ = shutdown_rx.recv() => break,
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {},
                }
                continue;
            }

            if let Some(limits) = limits.as_ref() {
                if let Err(err) = restart_once(&state, limits, &mut over_high).await {
                    tracing::warn!("provider restart tick failed: {err:#}");
                }
            }

            let interval = limits
                .as_ref()
                .map(|l| l.interval)
                .unwrap_or(Duration::from_millis(DEFAULT_INTERVAL_MS));
            tokio::select! {
                _ = shutdown_rx.recv() => break,
                _ = tokio::time::sleep(interval) => {},
            }
        }
    });
}

async fn restart_once<H>(
    state: &Arc<H>,
    limits: &ProviderRestartLimits,
    over_high: &mut HashMap<String, OverLimitState>,
) -> Result<()>
where
    H: ProviderRestartHost,
{
    let system = state.system_snapshot().await;
    let samples = state.provider_memory_snapshot().await;
    handle_limits(state, limits, &system, &samples, over_high).await?;
    Ok(())
}

async fn handle_limits<H>(
    state: &Arc<H>,
    limits: &ProviderRestartLimits,
    system: &SystemSnapshot,
    samples: &[ProviderMemorySample],
    over_high: &mut HashMap<String, OverLimitState>,
) -> Result<()>
where
    H: ProviderRestartHost,
{
    let mut seen: HashSet<String> = HashSet::new();
    let high_bytes = mb_to_bytes(limits.memory_high_mb);

    for sample in samples {
        let provider_id = sample.provider_id.clone();
        seen.insert(provider_id.clone());

        if sample.memory_bytes < high_bytes {
            over_high.remove(&provider_id);
            continue;
        }

        let now = Instant::now();
        let entry = over_high.entry(provider_id.clone());
        let mut is_new = false;
        match entry {
            std::collections::hash_map::Entry::Vacant(vacant) => {
                is_new = true;
                vacant.insert(OverLimitState {
                    first_seen: now,
                    last_seen: now,
                    last_memory_bytes: sample.memory_bytes,
                    last_tool_memory_bytes: sample.tool_memory_bytes,
                    pid: sample.pid,
                });
            }
            std::collections::hash_map::Entry::Occupied(mut occupied) => {
                let entry = occupied.get_mut();
                if entry.pid != sample.pid {
                    is_new = true;
                    *entry = OverLimitState {
                        first_seen: now,
                        last_seen: now,
                        last_memory_bytes: sample.memory_bytes,
                        last_tool_memory_bytes: sample.tool_memory_bytes,
                        pid: sample.pid,
                    };
                } else {
                    entry.last_seen = now;
                    entry.last_memory_bytes = sample.memory_bytes;
                    entry.last_tool_memory_bytes = sample.tool_memory_bytes;
                }
            }
        }

        if is_new {
            let restart_at_ms =
                unix_ms_now().saturating_add(limits.grace_period.as_millis() as u64);
            H::on_provider_restart_notice(
                state,
                ProviderRestartEvent {
                    sample: sample.clone(),
                    kind: "provider_restart_warning",
                    stage: "high",
                    limits: limits.clone(),
                    system: system.clone(),
                    restart_at_ms: Some(restart_at_ms),
                },
            )
            .await;
        }

        let Some(entry) = over_high.get(&provider_id) else {
            tracing::warn!(
                provider_id,
                "over-limit state missing after update; skipping restart check"
            );
            continue;
        };
        if now.duration_since(entry.first_seen) >= limits.grace_period {
            H::on_provider_restart_notice(
                state,
                ProviderRestartEvent {
                    sample: sample.clone(),
                    kind: "provider_restart",
                    stage: "restart",
                    limits: limits.clone(),
                    system: system.clone(),
                    restart_at_ms: None,
                },
            )
            .await;
            state.restart_provider(&provider_id, sample.pid).await;
            over_high.remove(&provider_id);
        }
    }

    over_high.retain(|provider_id, _| seen.contains(provider_id));
    Ok(())
}

fn mb_to_bytes(value: u32) -> u64 {
    value as u64 * 1024 * 1024
}

fn unix_ms_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn apply_settings_to_runtime_updates_restart_limits() {
        let runtime = Mutex::new(ProviderRestartRuntime::default());
        let system = SystemSnapshot {
            memory_total_bytes: 8 * 1024 * 1024 * 1024,
            memory_used_bytes: 1024 * 1024 * 1024,
        };
        let config = ProviderRestartConfig {
            enabled: true,
            mode: Some(ResourceGovernanceMode::Custom),
            memory_high_mb: Some(384),
            memory_max_mb: Some(768),
            interval_ms: Some(300),
            grace_period_ms: Some(1_500),
        };

        apply_settings_to_runtime(&runtime, &config, &system)
            .await
            .expect("apply restart settings");

        let snapshot = runtime.lock().await.clone();
        let limits = snapshot.last_applied.expect("restart limits");
        assert!(snapshot.enabled);
        assert_eq!(limits.memory_high_mb, 384);
        assert_eq!(limits.memory_max_mb, DEFAULT_MIN_MEMORY_MB as u32);
        assert_eq!(limits.interval, Duration::from_millis(300));
        assert_eq!(limits.grace_period, Duration::from_millis(1_500));
    }
}
