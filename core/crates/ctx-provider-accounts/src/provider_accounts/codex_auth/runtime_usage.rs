use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::runtime::{
    broker_oauth_auth_for_projection, codex_env_for_home,
    migrate_owned_runtime_oauth_projection_to_broker_if_needed,
    mirror_account_auth_to_runtime_home, prepare_broker_home_from_legacy_account_auth,
    prepare_broker_home_from_secret, prepare_codex_runtime_auth_with_runtime_root_and_oauth_policy,
    CodexOAuthAccessPolicy,
};
use super::runtime_oauth::{
    codex_oauth_runtime_home, fail_if_codex_oauth_reauth_required,
    project_oauth_authority_to_runtime_home,
};
use super::secret_store::{
    codex_auth_has_refresh_token, ensure_private_dir_allowing_concurrent_create,
    hydrate_codex_account_home_from_secret, load_codex_auth_from_secret_store,
};
use super::*;

pub async fn codex_usage_env_for_active_account(
    data_root: &Path,
) -> Result<HashMap<String, String>> {
    if let Ok(value) = std::env::var("CTX_CODEX_HOME") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            let dir = PathBuf::from(trimmed);
            ctx_fs::permissions::ensure_private_dir(&dir).await?;
            return Ok(codex_env_for_home(&dir));
        }
    }

    let prepared = prepare_codex_runtime_auth_with_runtime_root_and_oauth_policy(
        data_root,
        data_root,
        CodexOAuthAccessPolicy::ProjectCurrentAccessOnly,
    )
    .await?;
    ensure_private_dir_allowing_concurrent_create(&prepared.home).await?;
    Ok(codex_env_for_home(&prepared.home))
}

pub async fn codex_usage_env_for_account(
    data_root: &Path,
    account_id: &str,
) -> Result<HashMap<String, String>> {
    ensure_safe_account_id(account_id)?;
    require_codex_account_exists(data_root, account_id).await?;
    if codex_account_deletion_in_progress(data_root, account_id).await? {
        anyhow::bail!("codex account is being deleted");
    }
    fail_if_codex_oauth_reauth_required(data_root, account_id).await?;

    let registry = load_codex_registry(data_root).await?;
    let entry = registry
        .accounts
        .iter()
        .find(|entry| entry.id == account_id)
        .ok_or_else(|| anyhow::anyhow!("unknown account"))?;
    ensure_codex_endpoint_profile_compatible(&entry.endpoint_profile)?;

    if let Some(secret_ref) = entry.secret_ref.as_deref() {
        let auth = load_codex_auth_from_secret_store(data_root, secret_ref).await?;
        if codex_auth_has_refresh_token(&auth) {
            migrate_owned_runtime_oauth_projection_to_broker_if_needed(data_root, account_id)
                .await?;
            let broker_home =
                prepare_broker_home_from_secret(data_root, account_id, secret_ref).await?;
            let broker_auth = broker_oauth_auth_for_projection(
                data_root,
                account_id,
                &broker_home,
                CodexOAuthAccessPolicy::ProjectCurrentAccessOnly,
            )
            .await?;
            project_oauth_authority_to_runtime_home(data_root, data_root, account_id, &broker_auth)
                .await?;
            let runtime_home = codex_oauth_runtime_home(data_root, account_id)?;
            ctx_fs::permissions::ensure_private_dir(&runtime_home).await?;
            return Ok(codex_env_for_home(&runtime_home));
        }
    }

    if let Some(broker_home) =
        prepare_broker_home_from_legacy_account_auth(data_root, account_id).await?
    {
        let broker_auth = broker_oauth_auth_for_projection(
            data_root,
            account_id,
            &broker_home,
            CodexOAuthAccessPolicy::ProjectCurrentAccessOnly,
        )
        .await?;
        project_oauth_authority_to_runtime_home(data_root, data_root, account_id, &broker_auth)
            .await?;
        let runtime_home = codex_oauth_runtime_home(data_root, account_id)?;
        ctx_fs::permissions::ensure_private_dir(&runtime_home).await?;
        return Ok(codex_env_for_home(&runtime_home));
    }

    if let Some(runtime_home) = mirror_account_auth_to_runtime_home(
        data_root,
        account_id,
        CodexOAuthAccessPolicy::ProjectCurrentAccessOnly,
    )
    .await?
    {
        ctx_fs::permissions::ensure_private_dir(&runtime_home).await?;
        return Ok(codex_env_for_home(&runtime_home));
    }

    hydrate_codex_account_home_from_secret(data_root, account_id).await?;
    let broker_home = codex_broker_home(data_root, account_id);
    ctx_fs::permissions::ensure_private_dir(&broker_home).await?;
    Ok(codex_env_for_home(&broker_home))
}
