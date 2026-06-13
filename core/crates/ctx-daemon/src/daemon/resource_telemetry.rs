use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use tokio::time::MissedTickBehavior;

use ctx_resource_utilization::{
    resource_telemetry_log::{
        append_resource_telemetry_log, cleanup_old_resource_telemetry_logs,
        resource_telemetry_cleanup_key,
    },
    resource_utilization_disabled_from_env, trim_resource_processes, ResourceTelemetryConfig,
};

use crate::daemon::DaemonState;
use ctx_observability::logs;

mod event;
mod providers;
mod remote_metrics;

use event::ResourceTelemetryEvent;
use providers::provider_session_counts;
use remote_metrics::export_remote_metrics;

pub fn spawn_resource_telemetry(state: Arc<DaemonState>) {
    if resource_utilization_disabled_from_env() {
        return;
    }
    let cfg = ResourceTelemetryConfig::from_env();
    if !cfg.enabled() {
        return;
    }

    let mut shutdown_rx = state.core.shutdown_tx.subscribe();
    tokio::spawn(async move {
        let mut last_cleanup = None::<String>;
        if let Err(err) = sample_once(&state, &cfg, &mut last_cleanup).await {
            tracing::warn!("resource telemetry sample failed: {err:#}");
        }

        let mut ticker = tokio::time::interval(cfg.interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = shutdown_rx.recv() => break,
                _ = ticker.tick() => {
                    if let Err(err) = sample_once(&state, &cfg, &mut last_cleanup).await {
                        tracing::warn!("resource telemetry sample failed: {err:#}");
                    }
                }
            }
        }
    });
}

async fn sample_once(
    state: &Arc<DaemonState>,
    cfg: &ResourceTelemetryConfig,
    last_cleanup: &mut Option<String>,
) -> Result<()> {
    let provider_processes = state.providers.list_provider_processes().await;
    let (system, cache_age_ms, processes, provider_memory_rollups) = {
        let mut sampler = state.telemetry.resource_sampler.lock().await;
        let (system, _disks, cache_age_ms) = sampler.system_snapshot();
        let processes = sampler.processes_snapshot_light(std::process::id(), &provider_processes);
        let provider_memory_rollups = sampler.provider_memory_rollups(&provider_processes);
        (system, cache_age_ms, processes, provider_memory_rollups)
    };

    let provider_sessions = provider_session_counts(state).await;
    let shared_substrate_lifecycle =
        ctx_harness_runtime::selected_shared_substrate_lifecycle(&state.core.data_root)
            .ok()
            .flatten();
    let processes = trim_resource_processes(processes, cfg.child_limit);
    let event = ResourceTelemetryEvent {
        occurred_at: Utc::now(),
        cache_age_ms,
        system: system.clone(),
        processes: processes.clone(),
        provider_sessions: provider_sessions.clone(),
        provider_memory_rollups,
        shared_substrate_lifecycle: shared_substrate_lifecycle.clone(),
    };

    let logs_dir = logs::logs_dir(&state.core.data_root);
    append_resource_telemetry_log(&logs_dir, event.occurred_at, &event, cfg.local_max_bytes)
        .await?;
    if cfg.local_retention_days > 0 {
        let today = resource_telemetry_cleanup_key(event.occurred_at);
        if last_cleanup.as_deref() != Some(&today) {
            let _ = cleanup_old_resource_telemetry_logs(
                &logs_dir,
                Utc::now(),
                cfg.local_retention_days,
            )
            .await;
            *last_cleanup = Some(today);
        }
    }

    export_remote_metrics(
        &state.telemetry.perf_telemetry,
        &system,
        &processes,
        &provider_sessions,
        shared_substrate_lifecycle.as_ref(),
    );
    Ok(())
}
