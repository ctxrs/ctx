use std::time::Instant;

use super::{CacheSweepConfig, CacheSweepStats, DaemonState};

mod sessions;
mod stores;
mod telemetry;
mod workspaces;

impl DaemonState {
    pub async fn sweep_idle_caches(
        &self,
        now: Instant,
        config: CacheSweepConfig,
    ) -> CacheSweepStats {
        let mut stats = CacheSweepStats::default();
        let running_sessions = self.running_sessions_snapshot().await;

        self.sweep_session_caches(now, config, &running_sessions, &mut stats)
            .await;
        self.sweep_workspace_runtime_caches(now, config, &mut stats)
            .await;
        self.sweep_workspace_stores(config, &mut stats).await;
        self.emit_cache_sweep_stats(&stats).await;

        stats
    }
}
