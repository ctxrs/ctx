use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::Utc;
use ctx_provider_accounts as provider_accounts;
use ctx_provider_runtime::ProviderRuntime;

pub struct CodexAccountsSnapshot {
    pub active_account_id: Option<String>,
    pub accounts: Vec<provider_accounts::CodexAccountEntry>,
    pub logins: Vec<provider_accounts::CodexLoginStatus>,
}

pub struct PreparedCodexLoginStart {
    pub account_id: String,
    pub label: String,
    pub account_dir: PathBuf,
    pub codex_bin: String,
}

pub async fn probe_host_codex_auth_candidate() -> provider_accounts::CodexHostImportProbe {
    provider_accounts::probe_host_codex_auth_candidate().await
}

pub async fn prepare_codex_login_start(
    data_root: &Path,
    label: Option<String>,
) -> anyhow::Result<PreparedCodexLoginStart> {
    let account_id = uuid::Uuid::new_v4().to_string();
    let label = provider_accounts::normalize_label(label, &account_id);
    let account_dir = provider_accounts::ensure_codex_account_dir(data_root, &account_id)
        .await
        .with_context(|| format!("creating codex account directory for {account_id}"))?;

    let prep_result = async {
        let (cfg, managed_config_error) = ctx_provider_runtime::provider_launch::config::load_managed_agent_server_config_with_error(
            data_root,
        )
        .await;
        if let Some(error) = managed_config_error {
            anyhow::bail!(error);
        }
        let codex_bin = ctx_managed_installs::require_codex_cli_command_path_for_target(
            &cfg,
            Some(ctx_provider_install::install_state::InstallTarget::Host),
        )
        .context("resolving managed Codex CLI command")?;
        Ok(codex_bin)
    }
    .await;

    match prep_result {
        Ok(codex_bin) => Ok(PreparedCodexLoginStart {
            account_id,
            label,
            account_dir,
            codex_bin,
        }),
        Err(err) => {
            let _ = tokio::fs::remove_dir_all(&account_dir).await;
            Err(err)
        }
    }
}

pub async fn persist_successful_codex_login(
    data_root: &Path,
    providers: &ProviderRuntime,
    account_id: &str,
    label: String,
    email: Option<String>,
    plan_type: Option<String>,
) -> anyhow::Result<()> {
    let entry = provider_accounts::CodexAccountEntry {
        id: account_id.to_string(),
        label,
        kind: provider_accounts::CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
        email,
        provider_account_id: None,
        plan_type,
        created_at: Utc::now(),
        last_used_at: Some(Utc::now()),
        secret_ref: None,
        endpoint_profile: provider_accounts::CodexEndpointProfile::default(),
    };
    provider_accounts::upsert_codex_account(data_root, entry)
        .await
        .with_context(|| format!("persisting codex account {account_id}"))?;

    let persist_result = async {
        let ingested =
            provider_accounts::ingest_codex_account_auth_to_secret_store(data_root, account_id)
                .await
                .with_context(|| format!("ingesting codex auth for account {account_id}"))?;
        if !ingested {
            anyhow::bail!("missing persisted codex auth file for account {account_id}");
        }
        provider_accounts::remove_codex_account_home_auth_if_present(data_root, account_id)
            .await
            .with_context(|| {
                format!("removing account-home codex auth for account {account_id}")
            })?;
        provider_accounts::set_active_codex_account(data_root, Some(account_id.to_string()))
            .await
            .with_context(|| format!("setting active codex account {account_id}"))?;
        crate::daemon::providers::restarts::restart_provider_for_auth_change_with_runtime(
            providers,
            ctx_core::provider_ids::CODEX_PROVIDER_ID,
            "codex auth updated",
        )
        .await
        .context("restarting codex providers after auth change")?;
        Ok(())
    }
    .await;

    if let Err(err) = persist_result {
        let _ = provider_accounts::remove_codex_account(data_root, account_id).await;
        let _ = provider_accounts::cleanup_codex_account_broker_home(data_root, account_id).await;
        return Err(err);
    }

    Ok(())
}
