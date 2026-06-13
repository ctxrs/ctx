use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use ctx_core::provider_ids::CODEX_PROVIDER_ID;
use ctx_managed_installs as installer;
use ctx_provider_accounts as provider_accounts;
use ctx_provider_install::install_state::InstallTarget;
use serde::Serialize;
use tokio::sync::broadcast;

use crate::ProviderRuntime;

#[path = "provider_usage/oauth.rs"]
mod oauth;
#[path = "provider_usage/rpc.rs"]
mod rpc;
#[cfg(test)]
#[path = "provider_usage/tests.rs"]
mod tests;

pub trait ProviderUsageHost: Send + Sync + 'static {
    fn data_root(&self) -> &Path;
    fn provider_runtime(&self) -> &ProviderRuntime;
    fn subscribe_shutdown(&self) -> broadcast::Receiver<()>;
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderUsageSnapshot {
    pub provider_id: String,
    pub source: String,
    pub fetched_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(60);

async fn cache_usage_error<H>(state: &H, provider_id: &str, error: String)
where
    H: ProviderUsageHost,
{
    let snapshot = ProviderUsageSnapshot {
        provider_id: provider_id.to_string(),
        source: "error".to_string(),
        fetched_at: Utc::now(),
        payload: None,
        error: Some(error),
    };
    state
        .provider_runtime()
        .with_provider_usage_cache(|cache| {
            cache.insert(provider_id.to_string(), snapshot);
        })
        .await;
}

pub fn spawn_provider_usage_poller<H>(state: std::sync::Arc<H>)
where
    H: ProviderUsageHost,
{
    let mut shutdown_rx = state.subscribe_shutdown();
    let poll_interval = usage_poll_interval_from_env().unwrap_or(DEFAULT_POLL_INTERVAL);
    if poll_interval.is_zero() {
        return;
    }
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(poll_interval);
        loop {
            tokio::select! {
                _ = shutdown_rx.recv() => break,
                _ = ticker.tick() => {
                    if let Err(err) = refresh_provider_usage(state.as_ref()).await {
                        tracing::warn!("provider usage poll failed: {err:#}");
                    }
                }
            }
        }
    });
}

pub async fn refresh_provider_usage<H>(state: &H) -> Result<()>
where
    H: ProviderUsageHost,
{
    let result: Result<()> = async {
        let mut env =
            provider_accounts::codex_usage_env_for_active_account(state.data_root()).await?;
        let cfg = installer::load_agent_server_config(state.data_root())
            .await
            .context("loading agent server config")?;
        installer::ensure_codex_cli_command_env_for_target(
            &mut env,
            &cfg,
            CODEX_PROVIDER_ID,
            Some(InstallTarget::Host),
        )?;
        refresh_provider_usage_for(state, CODEX_PROVIDER_ID, env).await?;
        Ok(())
    }
    .await;
    match result {
        Ok(()) => Ok(()),
        Err(err) => {
            cache_usage_error(state, CODEX_PROVIDER_ID, err.to_string()).await;
            Err(err)
        }
    }
}

pub async fn refresh_provider_usage_for<H>(
    state: &H,
    provider_id: &str,
    env: HashMap<String, String>,
) -> Result<ProviderUsageSnapshot>
where
    H: ProviderUsageHost,
{
    let snapshot = match provider_id {
        CODEX_PROVIDER_ID => fetch_codex_usage(env).await?,
        _ => {
            return Ok(ProviderUsageSnapshot {
                provider_id: provider_id.to_string(),
                source: "unsupported".to_string(),
                fetched_at: Utc::now(),
                payload: None,
                error: Some("usage not supported for provider".to_string()),
            });
        }
    };
    state
        .provider_runtime()
        .with_provider_usage_cache(|cache| {
            cache.insert(provider_id.to_string(), snapshot.clone());
        })
        .await;
    Ok(snapshot)
}

pub async fn fetch_codex_usage_snapshot(
    env: HashMap<String, String>,
) -> Result<ProviderUsageSnapshot> {
    fetch_codex_usage(env).await
}

async fn fetch_codex_usage(env: HashMap<String, String>) -> Result<ProviderUsageSnapshot> {
    let auth_kind = match oauth::codex_usage_auth_kind(&env).await {
        Ok(auth_kind) => auth_kind,
        Err(err) => {
            return Ok(ProviderUsageSnapshot {
                provider_id: CODEX_PROVIDER_ID.to_string(),
                source: "error".to_string(),
                fetched_at: Utc::now(),
                payload: None,
                error: Some(err.to_string()),
            });
        }
    };
    match auth_kind {
        oauth::CodexUsageAuthKind::OAuth => match oauth::fetch_codex_usage_oauth(&env).await {
            Ok(payload) => Ok(ProviderUsageSnapshot {
                provider_id: CODEX_PROVIDER_ID.to_string(),
                source: "oauth".to_string(),
                fetched_at: Utc::now(),
                payload: Some(payload),
                error: None,
            }),
            Err(err) => Ok(ProviderUsageSnapshot {
                provider_id: CODEX_PROVIDER_ID.to_string(),
                source: "error".to_string(),
                fetched_at: Utc::now(),
                payload: None,
                error: Some(format!(
                    "{err}; usage polling will not refresh Codex OAuth tokens outside the active Codex auth authority"
                )),
            }),
        },
        oauth::CodexUsageAuthKind::ApiKey => match rpc::fetch_codex_usage_rpc(&env).await {
            Ok(payload) => Ok(ProviderUsageSnapshot {
                provider_id: CODEX_PROVIDER_ID.to_string(),
                source: "rpc".to_string(),
                fetched_at: Utc::now(),
                payload: Some(payload),
                error: None,
            }),
            Err(err) => Ok(ProviderUsageSnapshot {
                provider_id: CODEX_PROVIDER_ID.to_string(),
                source: "error".to_string(),
                fetched_at: Utc::now(),
                payload: None,
                error: Some(err.to_string()),
            }),
        },
    }
}

fn usage_poll_interval_from_env() -> Option<Duration> {
    let raw = std::env::var("CTX_PROVIDER_USAGE_INTERVAL_MS").ok()?;
    let ms: u64 = raw.trim().parse().ok()?;
    Some(Duration::from_millis(ms))
}
