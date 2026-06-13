use crate::daemon::DaemonState;

use super::WorkspaceCacheStats;

pub(super) async fn collect_workspace_cache_stats(state: &DaemonState) -> WorkspaceCacheStats {
    state.workspaces.cache_debug_stats().await
}
