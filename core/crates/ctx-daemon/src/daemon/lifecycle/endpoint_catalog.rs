use std::sync::Arc;
use std::time::Duration;

use crate::daemon::DaemonState;

const DEFAULT_ENDPOINT_MODEL_SWEEP_INTERVAL: Duration = Duration::from_secs(60 * 60 * 6);

fn endpoint_model_sweep_interval() -> Duration {
    std::env::var("CTX_ENDPOINT_MODEL_SWEEP_SECS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_ENDPOINT_MODEL_SWEEP_INTERVAL)
}

pub(in crate::daemon) fn spawn_endpoint_model_catalog_sweeper(state: Arc<DaemonState>) {
    let interval = endpoint_model_sweep_interval();
    tokio::spawn(async move {
        let mut shutdown_rx = state.core.shutdown_tx.subscribe();
        loop {
            tokio::select! {
                _ = tokio::time::sleep(interval) => {
                    let result = state
                        .providers
                        .refresh_stale_selected_endpoint_model_catalogs(&state.core.data_root)
                        .await;

                    if result.has_activity() {
                        tracing::info!(
                            refreshed_endpoint_catalogs = result.refreshed,
                            failed_endpoint_catalog_refreshes = result.failed,
                            "endpoint model catalog sweep completed"
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
