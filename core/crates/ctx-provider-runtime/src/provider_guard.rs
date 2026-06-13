use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use sysinfo::{Pid, Signal, System};
use tokio::sync::{broadcast, Mutex};

pub use crate::resource_governance::{
    ProviderMemorySample, ResourceGovernanceMode, SystemSnapshot,
};
use crate::ProviderRuntime;

const DEFAULT_INTERVAL_MS: u64 = 5_000;
const DEFAULT_GRACE_PERIOD_MS: u64 = 300_000;
const DEFAULT_MIN_MEMORY_MB: u64 = 1024;
const DEFAULT_MEMORY_FRACTION: f64 = 0.6;

#[derive(Debug, Clone, Default)]
pub struct ProviderGuardConfig {
    pub enabled: bool,
    pub mode: Option<ResourceGovernanceMode>,
    pub memory_high_mb: Option<u32>,
    pub memory_max_mb: Option<u32>,
    pub interval_ms: Option<u64>,
    pub grace_period_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderGuardLimits {
    pub memory_high_mb: u32,
    pub memory_max_mb: u32,
    pub interval: Duration,
    pub grace_period: Duration,
}

#[derive(Debug, Clone, Default)]
pub struct ProviderGuardRuntime {
    pub enabled: bool,
    pub last_applied: Option<ProviderGuardLimits>,
    pub last_message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProviderGuardEvent {
    pub sample: ProviderMemorySample,
    pub kind: &'static str,
    pub stage: &'static str,
    pub limits: ProviderGuardLimits,
    pub system: SystemSnapshot,
    pub kill_at_ms: Option<u64>,
}

#[derive(Debug, Clone)]
struct OverLimitState {
    first_seen: Instant,
    last_seen: Instant,
    last_memory_bytes: u64,
}

#[async_trait::async_trait]
pub trait ProviderGuardHost: Send + Sync + 'static {
    fn provider_guard_runtime(&self) -> &Mutex<ProviderGuardRuntime>;
    fn subscribe_shutdown(&self) -> broadcast::Receiver<()>;
    async fn system_snapshot(&self) -> SystemSnapshot;
    async fn provider_memory_snapshot(&self) -> Vec<ProviderMemorySample>;
    async fn on_provider_guard_event(state: &Arc<Self>, event: ProviderGuardEvent);
}

impl ProviderRuntime {
    pub fn provider_guard_runtime(&self) -> &Mutex<ProviderGuardRuntime> {
        &self.guard
    }
}

pub fn compute_effective_limits(
    settings: &ProviderGuardConfig,
    system: &SystemSnapshot,
) -> Option<ProviderGuardLimits> {
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

    Some(ProviderGuardLimits {
        memory_high_mb: memory_high_mb as u32,
        memory_max_mb: memory_max_mb as u32,
        interval: Duration::from_millis(interval_ms),
        grace_period: Duration::from_millis(grace_ms),
    })
}

pub async fn apply_settings<H>(state: &H, settings: &ProviderGuardConfig) -> Result<()>
where
    H: ProviderGuardHost,
{
    let system = state.system_snapshot().await;
    apply_settings_to_runtime(state.provider_guard_runtime(), settings, &system).await
}

pub async fn apply_settings_to_runtime(
    provider_guard_runtime: &Mutex<ProviderGuardRuntime>,
    settings: &ProviderGuardConfig,
    system: &SystemSnapshot,
) -> Result<()> {
    let effective = compute_effective_limits(settings, system);

    let runtime = ProviderGuardRuntime {
        enabled: settings.enabled,
        last_applied: effective,
        last_message: None,
    };

    let mut guard = provider_guard_runtime.lock().await;
    *guard = runtime;
    Ok(())
}

pub fn spawn_provider_guard<H>(state: Arc<H>)
where
    H: ProviderGuardHost,
{
    tokio::spawn(async move {
        let mut shutdown_rx = state.subscribe_shutdown();
        let mut warned_high: HashSet<u32> = HashSet::new();
        let mut over_max: HashMap<u32, OverLimitState> = HashMap::new();

        loop {
            let (enabled, limits) = {
                let runtime = state.provider_guard_runtime().lock().await;
                (runtime.enabled, runtime.last_applied.clone())
            };

            if !enabled || limits.is_none() {
                warned_high.clear();
                over_max.clear();
                tokio::select! {
                    _ = shutdown_rx.recv() => break,
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {},
                }
                continue;
            }

            if let Some(limits) = limits.as_ref() {
                if let Err(err) = guard_once(&state, limits, &mut warned_high, &mut over_max).await
                {
                    tracing::warn!("provider guard tick failed: {err:#}");
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

async fn guard_once<H>(
    state: &Arc<H>,
    limits: &ProviderGuardLimits,
    warned_high: &mut HashSet<u32>,
    over_max: &mut HashMap<u32, OverLimitState>,
) -> Result<()>
where
    H: ProviderGuardHost,
{
    let system = state.system_snapshot().await;
    let provider_memory = state.provider_memory_snapshot().await;

    handle_limits(
        state,
        limits,
        &system,
        &provider_memory,
        warned_high,
        over_max,
    )
    .await?;
    Ok(())
}

async fn handle_limits<H>(
    state: &Arc<H>,
    limits: &ProviderGuardLimits,
    system: &SystemSnapshot,
    provider_memory: &[ProviderMemorySample],
    warned_high: &mut HashSet<u32>,
    over_max: &mut HashMap<u32, OverLimitState>,
) -> Result<()>
where
    H: ProviderGuardHost,
{
    let mut seen: HashSet<u32> = HashSet::new();
    let high_bytes = mb_to_bytes(limits.memory_high_mb);
    let max_bytes = mb_to_bytes(limits.memory_max_mb);

    for sample in provider_memory.iter() {
        let pid = sample.pid;
        seen.insert(pid);

        if sample.memory_bytes >= high_bytes && !warned_high.contains(&pid) {
            warned_high.insert(pid);
            H::on_provider_guard_event(
                state,
                ProviderGuardEvent {
                    sample: sample.clone(),
                    kind: "provider_guard_warning",
                    stage: "high",
                    limits: limits.clone(),
                    system: system.clone(),
                    kill_at_ms: None,
                },
            )
            .await;
        } else if sample.memory_bytes < high_bytes {
            warned_high.remove(&pid);
        }

        if sample.memory_bytes >= max_bytes {
            let now = Instant::now();
            let is_new = !over_max.contains_key(&pid);
            let entry = over_max.entry(pid).or_insert_with(|| OverLimitState {
                first_seen: now,
                last_seen: now,
                last_memory_bytes: sample.memory_bytes,
            });
            entry.last_seen = now;
            entry.last_memory_bytes = sample.memory_bytes;

            if is_new {
                let kill_at_ms =
                    unix_ms_now().saturating_add(limits.grace_period.as_millis() as u64);
                H::on_provider_guard_event(
                    state,
                    ProviderGuardEvent {
                        sample: sample.clone(),
                        kind: "provider_guard_warning",
                        stage: "max",
                        limits: limits.clone(),
                        system: system.clone(),
                        kill_at_ms: Some(kill_at_ms),
                    },
                )
                .await;
            }

            if now.duration_since(entry.first_seen) >= limits.grace_period {
                H::on_provider_guard_event(
                    state,
                    ProviderGuardEvent {
                        sample: sample.clone(),
                        kind: "provider_guard_kill",
                        stage: "kill",
                        limits: limits.clone(),
                        system: system.clone(),
                        kill_at_ms: None,
                    },
                )
                .await;
                let killed = signal_pids(&[sample.pid], Signal::Kill);
                if killed == 0 {
                    tracing::warn!(
                        provider_id = %sample.label,
                        pid = sample.pid,
                        "provider guard failed to kill process"
                    );
                }
                over_max.remove(&pid);
            }
        } else {
            over_max.remove(&pid);
        }
    }

    warned_high.retain(|pid| seen.contains(pid));
    over_max.retain(|pid, _| seen.contains(pid));
    Ok(())
}

fn signal_pids(pids: &[u32], signal: Signal) -> usize {
    let mut system = System::new();
    system.refresh_processes();
    let mut killed = 0usize;
    for pid in pids {
        if let Some(process) = system.process(Pid::from_u32(*pid)) {
            if process.kill_with(signal).unwrap_or(false) {
                killed += 1;
            }
        }
    }
    killed
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
    async fn apply_settings_to_runtime_updates_guard_limits() {
        let runtime = Mutex::new(ProviderGuardRuntime::default());
        let system = SystemSnapshot {
            memory_total_bytes: 8 * 1024 * 1024 * 1024,
            memory_used_bytes: 1024 * 1024 * 1024,
        };
        let config = ProviderGuardConfig {
            enabled: true,
            mode: Some(ResourceGovernanceMode::Custom),
            memory_high_mb: Some(512),
            memory_max_mb: Some(768),
            interval_ms: Some(250),
            grace_period_ms: Some(1_000),
        };

        apply_settings_to_runtime(&runtime, &config, &system)
            .await
            .expect("apply guard settings");

        let snapshot = runtime.lock().await.clone();
        let limits = snapshot.last_applied.expect("guard limits");
        assert!(snapshot.enabled);
        assert_eq!(limits.memory_high_mb, 512);
        assert_eq!(limits.memory_max_mb, DEFAULT_MIN_MEMORY_MB as u32);
        assert_eq!(limits.interval, Duration::from_millis(250));
        assert_eq!(limits.grace_period, Duration::from_millis(1_000));
    }
}
