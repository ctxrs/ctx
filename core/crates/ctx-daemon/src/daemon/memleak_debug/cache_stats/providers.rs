use crate::daemon::DaemonState;

use super::ProviderCacheStats;

pub(super) async fn collect_provider_cache_stats(state: &DaemonState) -> ProviderCacheStats {
    let stats = state.providers.cache_stats().await;

    ProviderCacheStats {
        adapters: stats.adapters,
        statuses: stats.statuses,
        options_cache: stats.options_cache,
        verify_cache: stats.verify_cache,
        usage_cache: stats.usage_cache,
        installs: stats.installs,
    }
}
