use super::super::{CacheSweepStats, DaemonState};

impl DaemonState {
    pub(super) async fn emit_cache_sweep_stats(&self, stats: &CacheSweepStats) {
        self.emit_cache_evicted("session_head", stats.session_head_evicted)
            .await;
        self.emit_cache_evicted("session_meta", stats.session_meta_evicted)
            .await;
        self.emit_cache_evicted("scheduler", stats.schedulers_evicted)
            .await;
        self.emit_cache_evicted("broadcaster", stats.broadcasters_evicted)
            .await;
        self.emit_cache_evicted("session_event_head", stats.session_event_heads_evicted)
            .await;
        self.emit_cache_evicted("file_completions", stats.file_completions_evicted)
            .await;
        self.emit_cache_evicted(
            "workspace_file_completions",
            stats.workspace_file_completions_evicted,
        )
        .await;
        self.emit_cache_evicted("git_status", stats.git_status_evicted)
            .await;
        self.emit_cache_evicted(
            "workspace_active_snapshot",
            stats.workspace_snapshot_evicted,
        )
        .await;
        self.emit_cache_evicted("workspace_active_heads", stats.workspace_heads_evicted)
            .await;
        self.emit_cache_evicted("worktree_bootstrap", stats.worktree_bootstrap_evicted)
            .await;
        self.emit_cache_evicted("workspace_store", stats.workspace_stores_evicted)
            .await;
    }
}
