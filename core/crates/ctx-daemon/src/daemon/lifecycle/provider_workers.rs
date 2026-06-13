use std::sync::Arc;

use ctx_providers::adapters::ProviderSessionSweepConfig;

use crate::daemon::DaemonState;

pub(in crate::daemon) fn spawn_provider_worker_sweeper(state: Arc<DaemonState>) {
    let config = ProviderSessionSweepConfig::from_env();
    tokio::spawn(async move {
        let mut shutdown_rx = state.core.shutdown_tx.subscribe();
        loop {
            tokio::select! {
                _ = tokio::time::sleep(config.interval) => {
                    let stats = state.providers.sweep_provider_workers_once(config).await;
                    if stats.total_actions() > 0 || stats.skipped_busy > 0 || stats.status_errors > 0 {
                        tracing::info!(
                            reaped = stats.reaped,
                            dead_removed = stats.dead_removed,
                            skipped_busy = stats.skipped_busy,
                            status_errors = stats.status_errors,
                            "provider worker sweep completed"
                        );
                    }
                }
                _ = shutdown_rx.recv() => {
                    break;
                }
            }
        }
    });
}
