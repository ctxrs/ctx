use ctx_provider_runtime::ProviderRuntime;
use ctx_providers::adapters::ProviderRestartMode;

pub(in crate::daemon::providers) async fn invalidate_provider_runtime_state_for_runtime(
    providers: &ProviderRuntime,
    provider_id: &str,
) {
    ctx_provider_runtime::provider_cache::invalidate_provider_probe_caches(providers, provider_id)
        .await;
}

pub(crate) async fn restart_provider_for_auth_change_with_runtime(
    providers: &ProviderRuntime,
    provider_id: &str,
    reason: &str,
) -> anyhow::Result<()> {
    invalidate_provider_runtime_state_for_runtime(providers, provider_id).await;
    providers
        .drain_restart_provider_adapters_for_auth_change(provider_id, reason)
        .await
}

pub(in crate::daemon::providers) async fn stop_provider_for_auth_removal_with_runtime(
    providers: &ProviderRuntime,
    provider_id: &str,
    reason: &str,
) -> anyhow::Result<()> {
    invalidate_provider_runtime_state_for_runtime(providers, provider_id).await;
    let adapters = providers
        .provider_adapter_entries_for_provider(provider_id)
        .await;
    let mut failures = Vec::new();
    for (id, adapter) in adapters {
        if !adapter.supports_restart_mode(ProviderRestartMode::Immediate) {
            failures.push(format!("{id}: provider does not support immediate restart"));
            continue;
        }
        if let Err(err) = adapter
            .restart(reason, ProviderRestartMode::Immediate)
            .await
        {
            failures.push(format!("{id}: {err:#}"));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(
            "provider auth removed but immediate restart failed for {provider_id}: {}",
            failures.join("; ")
        )
    }
}
