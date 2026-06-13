use super::snapshot::{ProviderCacheStats, SessionCacheStats, WorkspaceCacheStats};
use crate::daemon::DaemonState;

mod providers;
mod sessions;
mod workspaces;

use providers::collect_provider_cache_stats;
use sessions::collect_session_cache_stats;
use workspaces::collect_workspace_cache_stats;

pub(super) struct MemleakDebugCacheStats {
    pub(super) sessions: SessionCacheStats,
    pub(super) workspaces: WorkspaceCacheStats,
    pub(super) providers: ProviderCacheStats,
}

pub(super) async fn collect_memleak_debug_cache_stats(
    state: &DaemonState,
) -> MemleakDebugCacheStats {
    let sessions = collect_session_cache_stats(state).await;
    let workspaces = collect_workspace_cache_stats(state).await;
    let providers = collect_provider_cache_stats(state).await;
    MemleakDebugCacheStats {
        sessions,
        workspaces,
        providers,
    }
}
