use std::path::Path;

use ctx_provider_accounts as provider_accounts;
use ctx_provider_runtime::ProviderRuntime;

use super::super::{ProviderAccountLoginMutation, ProviderAccountMutationError};
use crate::daemon::providers::restarts;

pub async fn add_claude_account_for_login(
    data_root: &Path,
    providers: &ProviderRuntime,
    label: Option<String>,
    setup_token: String,
) -> Result<ProviderAccountLoginMutation, ProviderAccountMutationError> {
    let registry = provider_accounts::add_claude_account(data_root, label, setup_token)
        .await
        .map_err(ProviderAccountMutationError::BadRequest)?;
    let restart_result = restarts::restart_provider_for_auth_change_with_runtime(
        providers,
        "claude-crp",
        "claude auth updated",
    )
    .await;
    Ok(ProviderAccountLoginMutation::from_restart_result(
        registry.active_account_id,
        restart_result,
    ))
}

pub async fn add_cursor_oauth_account_for_login(
    data_root: &Path,
    providers: &ProviderRuntime,
    label: Option<String>,
    auth_token: String,
    refresh_token: Option<String>,
    email: Option<String>,
) -> Result<ProviderAccountLoginMutation, ProviderAccountMutationError> {
    let registry = provider_accounts::add_cursor_oauth_account(
        data_root,
        label,
        auth_token,
        refresh_token,
        email,
    )
    .await
    .map_err(ProviderAccountMutationError::BadRequest)?;
    let restart_result = restarts::restart_provider_for_auth_change_with_runtime(
        providers,
        "cursor",
        "cursor auth updated",
    )
    .await;
    Ok(ProviderAccountLoginMutation::from_restart_result(
        registry.active_account_id,
        restart_result,
    ))
}

pub async fn add_kimi_oauth_account_for_login(
    data_root: &Path,
    providers: &ProviderRuntime,
    label: Option<String>,
    credentials_json: String,
    email: Option<String>,
) -> Result<ProviderAccountLoginMutation, ProviderAccountMutationError> {
    let registry =
        provider_accounts::add_kimi_oauth_account(data_root, label, credentials_json, email)
            .await
            .map_err(ProviderAccountMutationError::BadRequest)?;
    let restart_result = restarts::restart_provider_for_auth_change_with_runtime(
        providers,
        "kimi",
        "kimi auth updated",
    )
    .await;
    Ok(ProviderAccountLoginMutation::from_restart_result(
        registry.active_account_id,
        restart_result,
    ))
}
