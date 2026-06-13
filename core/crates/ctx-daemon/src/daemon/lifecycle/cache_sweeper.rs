use std::sync::Arc;
use std::time::Instant;

use crate::daemon::{CacheSweepConfig, DaemonState};

pub(in crate::daemon) fn spawn_cache_sweeper(state: Arc<DaemonState>) {
    let config = CacheSweepConfig::from_env();
    tokio::spawn(async move {
        let mut shutdown_rx = state.core.shutdown_tx.subscribe();
        loop {
            tokio::select! {
                _ = tokio::time::sleep(config.interval) => {
                    let stats = state.sweep_idle_caches(Instant::now(), config).await;
                    if stats.total_evicted() > 0 {
                        tracing::info!(
                            session_head_evicted = stats.session_head_evicted,
                            session_meta_evicted = stats.session_meta_evicted,
                            schedulers_evicted = stats.schedulers_evicted,
                            broadcasters_evicted = stats.broadcasters_evicted,
                            session_event_heads_evicted = stats.session_event_heads_evicted,
                            file_completions_evicted = stats.file_completions_evicted,
                            workspace_file_completions_evicted =
                                stats.workspace_file_completions_evicted,
                            git_status_evicted = stats.git_status_evicted,
                            workspace_snapshot_evicted = stats.workspace_snapshot_evicted,
                            workspace_heads_evicted = stats.workspace_heads_evicted,
                            worktree_bootstrap_evicted = stats.worktree_bootstrap_evicted,
                            workspace_stores_evicted = stats.workspace_stores_evicted,
                            "cache sweep completed"
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
