use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use ctx_resource_utilization::memleak_debug::{
    append_memleak_debug_log, read_memleak_debug_process_stats, MemleakDebugConfig,
};
use tokio::time::MissedTickBehavior;

use crate::daemon::DaemonState;
use ctx_observability::logs;

mod cache_stats;
mod snapshot;

use self::cache_stats::collect_memleak_debug_cache_stats;
use self::snapshot::MemleakDebugSnapshot;

pub fn spawn_memleak_debug(state: Arc<DaemonState>) {
    let config = MemleakDebugConfig::from_env();
    if !config.enabled {
        return;
    }
    let mut shutdown_rx = state.core.shutdown_tx.subscribe();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(config.interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = shutdown_rx.recv() => break,
                _ = ticker.tick() => {
                    if let Err(err) = sample_once(&state).await {
                        tracing::warn!("memleak debug sample failed: {err:#}");
                    }
                }
            }
        }
    });
}

async fn sample_once(state: &Arc<DaemonState>) -> Result<()> {
    let cache_stats = collect_memleak_debug_cache_stats(state).await;
    let active_snapshot = state.workspaces.workspace_active_snapshot.stats().await;
    let terminals = state.transport.terminals.stats().await;
    let perf_telemetry = state.telemetry.perf_telemetry.stats();
    let web_sessions = state.transport.web_sessions.stats().await;
    let harness_runtime = state.execution.harness.stats().await;
    let stores = state.core.stores.stats().await;
    let process = read_memleak_debug_process_stats();

    let snapshot = MemleakDebugSnapshot {
        occurred_at: Utc::now(),
        rss_bytes: process.rss_bytes,
        thread_count: process.thread_count,
        sessions: cache_stats.sessions,
        workspaces: cache_stats.workspaces,
        providers: cache_stats.providers,
        active_snapshot,
        terminals,
        perf_telemetry,
        web_sessions,
        harness_runtime,
        stores,
        glibc: process.glibc,
        jemalloc: process.jemalloc,
    };

    let logs_dir = logs::logs_dir(&state.core.data_root);
    append_memleak_debug_log(&logs_dir, &snapshot).await?;
    Ok(())
}
