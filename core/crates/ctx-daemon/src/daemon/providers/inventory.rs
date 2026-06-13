use ctx_observability::logs;
use ctx_provider_runtime::ProviderRuntimeHost;

pub struct ProviderMatrixRefreshSummary {
    pub provider_count: usize,
    pub generated_at: Option<String>,
    pub source: String,
    pub degraded: bool,
    pub last_error: Option<String>,
}

pub async fn refresh_provider_inventory<H>(
    state: &H,
) -> anyhow::Result<ProviderMatrixRefreshSummary>
where
    H: ProviderRuntimeHost + ctx_managed_installs::ManagedInstallHost,
{
    let outcome = state
        .provider_runtime()
        .refresh_provider_matrix_from_local_sources(ProviderRuntimeHost::data_root(state))
        .await;
    ctx_managed_installs::refresh_provider_statuses(state).await?;

    Ok(ProviderMatrixRefreshSummary {
        provider_count: outcome.matrix.providers.len(),
        generated_at: outcome.matrix.generated_at,
        source: outcome.source.as_str().to_string(),
        degraded: outcome.degraded,
        last_error: outcome
            .last_error
            .map(|value| logs::redact_sensitive(&value)),
    })
}
