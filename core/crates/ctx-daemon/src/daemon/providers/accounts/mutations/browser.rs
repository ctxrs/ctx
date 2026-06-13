use std::path::Path;

use ctx_provider_accounts as provider_accounts;
use ctx_provider_runtime::ProviderRuntime;

use super::super::{ProviderAccountLoginMutation, ProviderAccountMutationError};
use crate::daemon::providers::restarts;

pub async fn upsert_amp_account_for_login(
    data_root: &Path,
    providers: &ProviderRuntime,
    label: Option<String>,
    email: Option<String>,
) -> Result<ProviderAccountLoginMutation, ProviderAccountMutationError> {
    let registry = provider_accounts::upsert_amp_account(data_root, label, email)
        .await
        .map_err(ProviderAccountMutationError::BadRequest)?;
    let restart_result = restarts::restart_provider_for_auth_change_with_runtime(
        providers,
        "amp",
        "amp auth updated",
    )
    .await;
    Ok(ProviderAccountLoginMutation::from_restart_result(
        registry.active_account_id,
        restart_result,
    ))
}

pub async fn upsert_mistral_account_for_login(
    data_root: &Path,
    providers: &ProviderRuntime,
    label: Option<String>,
    email: Option<String>,
) -> Result<ProviderAccountLoginMutation, ProviderAccountMutationError> {
    let registry = provider_accounts::upsert_mistral_account(data_root, label, email)
        .await
        .map_err(ProviderAccountMutationError::BadRequest)?;
    let restart_result = restarts::restart_provider_for_auth_change_with_runtime(
        providers,
        "mistral",
        "mistral auth updated",
    )
    .await;
    Ok(ProviderAccountLoginMutation::from_restart_result(
        registry.active_account_id,
        restart_result,
    ))
}

pub async fn add_gemini_account_for_login(
    data_root: &Path,
    providers: &ProviderRuntime,
    label: Option<String>,
    oauth_creds_json: String,
    google_accounts_json: Option<String>,
    email: Option<String>,
) -> Result<ProviderAccountLoginMutation, ProviderAccountMutationError> {
    let registry = provider_accounts::add_gemini_account(
        data_root,
        label,
        oauth_creds_json,
        google_accounts_json,
        email,
    )
    .await
    .map_err(ProviderAccountMutationError::BadRequest)?;
    let restart_result = restarts::restart_provider_for_auth_change_with_runtime(
        providers,
        "gemini",
        "gemini auth updated",
    )
    .await;
    Ok(ProviderAccountLoginMutation::from_restart_result(
        registry.active_account_id,
        restart_result,
    ))
}

pub async fn add_qwen_account_for_login(
    data_root: &Path,
    providers: &ProviderRuntime,
    label: Option<String>,
    oauth_creds_json: String,
    email: Option<String>,
) -> Result<ProviderAccountLoginMutation, ProviderAccountMutationError> {
    let registry = provider_accounts::add_qwen_account(data_root, label, oauth_creds_json, email)
        .await
        .map_err(ProviderAccountMutationError::BadRequest)?;
    let restart_result = restarts::restart_provider_for_auth_change_with_runtime(
        providers,
        "qwen",
        "qwen auth updated",
    )
    .await;
    Ok(ProviderAccountLoginMutation::from_restart_result(
        registry.active_account_id,
        restart_result,
    ))
}
