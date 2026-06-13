use crate::daemon::DaemonState;

use super::SessionCacheStats;

pub(super) async fn collect_session_cache_stats(state: &DaemonState) -> SessionCacheStats {
    state.sessions.cache_debug_stats().await
}
