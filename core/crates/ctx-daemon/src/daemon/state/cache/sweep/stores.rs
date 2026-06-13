use super::super::{CacheSweepConfig, CacheSweepStats, DaemonState};

impl DaemonState {
    pub(super) async fn sweep_workspace_stores(
        &self,
        config: CacheSweepConfig,
        stats: &mut CacheSweepStats,
    ) {
        let active_workspaces = self.protected_workspace_store_ids().await;
        stats.workspace_stores_evicted = self
            .core
            .stores
            .evict_idle_workspaces(config.workspace_ttl, &active_workspaces)
            .await;
    }
}
