use std::path::Path;

use ctx_core::provider_ids::CODEX_PROVIDER_ID;
use ctx_provider_accounts::{
    self as provider_accounts, AmpAccountsResponse, ClaudeAccountsResponse, CodexAccountsResponse,
    CopilotAccountsResponse, CursorAccountsResponse, GeminiAccountsResponse, KimiAccountsResponse,
    MistralAccountsResponse, ProviderAccountRouteError, QwenAccountsResponse,
};
use ctx_provider_runtime::ProviderRuntime;

use crate::daemon::providers::restarts;

use super::super::{CodexAccountsSnapshot, ProviderAccountMutationError};
use super::error::{
    codex_set_active_error, ensure_known_account, internal_error, provider_account_mutation_error,
};

const CLAUDE_PROVIDER_ID: &str = "claude-crp";
const GEMINI_PROVIDER_ID: &str = "gemini";
const QWEN_PROVIDER_ID: &str = "qwen";
const AMP_PROVIDER_ID: &str = "amp";
const MISTRAL_PROVIDER_ID: &str = "mistral";
const KIMI_PROVIDER_ID: &str = "kimi";
const COPILOT_PROVIDER_ID: &str = "copilot";
const CURSOR_PROVIDER_ID: &str = "cursor";

#[derive(Clone, Copy)]
pub(in crate::daemon::providers::accounts::routes) struct ProviderAccountRouteDeps<'a> {
    data_root: &'a Path,
    providers: &'a ProviderRuntime,
}

impl<'a> ProviderAccountRouteDeps<'a> {
    pub(in crate::daemon::providers::accounts::routes) fn new(
        data_root: &'a Path,
        providers: &'a ProviderRuntime,
    ) -> Self {
        Self {
            data_root,
            providers,
        }
    }
}

pub(in crate::daemon::providers::accounts::routes) async fn codex_accounts_response(
    deps: ProviderAccountRouteDeps<'_>,
) -> Result<CodexAccountsResponse, ProviderAccountRouteError> {
    load_codex_accounts_snapshot(deps)
        .await
        .map(codex_accounts_route_response)
        .map_err(internal_error)
}

pub(in crate::daemon::providers::accounts::routes) async fn import_host_codex_auth_response(
    deps: ProviderAccountRouteDeps<'_>,
    label: Option<String>,
) -> Result<CodexAccountsResponse, ProviderAccountRouteError> {
    import_host_codex_auth(deps, label)
        .await
        .map_err(provider_account_mutation_error)?;
    codex_accounts_response(deps).await
}

pub(in crate::daemon::providers::accounts::routes) async fn set_active_codex_account_response(
    deps: ProviderAccountRouteDeps<'_>,
    account_id: Option<String>,
) -> Result<CodexAccountsResponse, ProviderAccountRouteError> {
    if account_id.is_some() {
        let registry = provider_accounts::load_codex_registry(deps.data_root)
            .await
            .map_err(internal_error)?;
        ensure_known_account(&account_id, &registry.accounts, |account| &account.id)?;
    }
    set_active_codex_account(deps, account_id)
        .await
        .map(codex_accounts_route_response)
        .map_err(codex_set_active_error)
}

pub(in crate::daemon::providers::accounts::routes) async fn delete_codex_account_response(
    deps: ProviderAccountRouteDeps<'_>,
    account_id: &str,
) -> Result<CodexAccountsResponse, ProviderAccountRouteError> {
    remove_codex_account(deps, account_id)
        .await
        .map(codex_accounts_route_response)
        .map_err(provider_account_mutation_error)
}

fn codex_accounts_route_response(snapshot: CodexAccountsSnapshot) -> CodexAccountsResponse {
    CodexAccountsResponse::new(
        snapshot.active_account_id,
        snapshot.accounts,
        snapshot.logins,
    )
}

async fn load_codex_accounts_snapshot(
    deps: ProviderAccountRouteDeps<'_>,
) -> anyhow::Result<CodexAccountsSnapshot> {
    let registry = provider_accounts::load_codex_registry(deps.data_root).await?;
    let logins = deps
        .providers
        .with_codex_login_sessions(|map| map.values().cloned().collect())
        .await;
    Ok(CodexAccountsSnapshot {
        active_account_id: registry.active_account_id,
        accounts: registry.accounts,
        logins,
    })
}

async fn import_host_codex_auth(
    deps: ProviderAccountRouteDeps<'_>,
    label: Option<String>,
) -> Result<(), ProviderAccountMutationError> {
    provider_accounts::import_host_codex_auth_to_secret_store(deps.data_root, label)
        .await
        .map_err(ProviderAccountMutationError::BadRequest)?;
    restart_provider_for_auth_change(deps, CODEX_PROVIDER_ID, "codex auth updated").await
}

async fn set_active_codex_account(
    deps: ProviderAccountRouteDeps<'_>,
    account_id: Option<String>,
) -> Result<CodexAccountsSnapshot, ProviderAccountMutationError> {
    provider_accounts::set_active_codex_account(deps.data_root, account_id)
        .await
        .map_err(ProviderAccountMutationError::BadRequest)?;
    restart_provider_for_auth_change(deps, CODEX_PROVIDER_ID, "codex auth updated").await?;
    load_codex_accounts_snapshot(deps)
        .await
        .map_err(ProviderAccountMutationError::Internal)
}

async fn remove_codex_account(
    deps: ProviderAccountRouteDeps<'_>,
    account_id: &str,
) -> Result<CodexAccountsSnapshot, ProviderAccountMutationError> {
    let previous_active =
        provider_accounts::begin_codex_account_deletion(deps.data_root, account_id)
            .await
            .map_err(ProviderAccountMutationError::Delete)?;
    let stop_result =
        stop_provider_for_auth_removal(deps, CODEX_PROVIDER_ID, "codex auth account removed").await;
    if let Err(err) = stop_result {
        if let Err(rollback_err) = provider_accounts::abort_codex_account_deletion(
            deps.data_root,
            account_id,
            previous_active,
        )
        .await
        {
            tracing::warn!(
                account_id,
                error = %rollback_err,
                "failed to roll back Codex account deletion marker after provider stop failure"
            );
        }
        return Err(ProviderAccountMutationError::Internal(err));
    }
    provider_accounts::cleanup_codex_account_broker_home(deps.data_root, account_id)
        .await
        .map_err(ProviderAccountMutationError::Delete)?;
    let registry = provider_accounts::remove_codex_account(deps.data_root, account_id)
        .await
        .map_err(ProviderAccountMutationError::Delete)?;
    if let Err(err) =
        provider_accounts::finish_codex_account_deletion(deps.data_root, account_id).await
    {
        tracing::warn!(
            account_id,
            error = %err,
            "failed to remove Codex account deletion marker after account deletion"
        );
    }
    let logins = deps
        .providers
        .with_codex_login_sessions(|map| {
            map.remove(account_id);
            map.values().cloned().collect()
        })
        .await;
    Ok(CodexAccountsSnapshot {
        active_account_id: registry.active_account_id,
        accounts: registry.accounts,
        logins,
    })
}

pub(in crate::daemon::providers::accounts::routes) async fn amp_accounts_response(
    deps: ProviderAccountRouteDeps<'_>,
) -> Result<AmpAccountsResponse, ProviderAccountRouteError> {
    provider_accounts::ensure_amp_registry_from_runtime_auth(deps.data_root)
        .await
        .map(AmpAccountsResponse::from)
        .map_err(internal_error)
}

pub(in crate::daemon::providers::accounts::routes) async fn upsert_amp_account_response(
    deps: ProviderAccountRouteDeps<'_>,
    label: Option<String>,
    email: Option<String>,
) -> Result<AmpAccountsResponse, ProviderAccountRouteError> {
    provider_accounts::upsert_amp_account(deps.data_root, label, email)
        .await
        .map_err(ProviderAccountMutationError::BadRequest)
        .map_err(provider_account_mutation_error)?;
    restart_provider_for_auth_change(deps, AMP_PROVIDER_ID, "amp auth updated")
        .await
        .map_err(provider_account_mutation_error)?;
    amp_accounts_response(deps).await
}

pub(in crate::daemon::providers::accounts::routes) async fn set_active_amp_account_response(
    deps: ProviderAccountRouteDeps<'_>,
    account_id: Option<String>,
) -> Result<AmpAccountsResponse, ProviderAccountRouteError> {
    if account_id.is_some() {
        let registry = provider_accounts::load_amp_registry(deps.data_root)
            .await
            .map_err(internal_error)?;
        ensure_known_account(&account_id, &registry.accounts, |account| &account.id)?;
    }
    provider_accounts::set_active_amp_account(deps.data_root, account_id)
        .await
        .map_err(ProviderAccountMutationError::BadRequest)
        .map_err(provider_account_mutation_error)?;
    restart_provider_for_auth_change(deps, AMP_PROVIDER_ID, "amp auth updated")
        .await
        .map_err(provider_account_mutation_error)?;
    amp_accounts_response(deps).await
}

pub(in crate::daemon::providers::accounts::routes) async fn delete_amp_account_response(
    deps: ProviderAccountRouteDeps<'_>,
    account_id: &str,
) -> Result<AmpAccountsResponse, ProviderAccountRouteError> {
    provider_accounts::remove_amp_account(deps.data_root, account_id)
        .await
        .map_err(ProviderAccountMutationError::Delete)
        .map_err(provider_account_mutation_error)?;
    restart_provider_for_auth_change(deps, AMP_PROVIDER_ID, "amp auth updated")
        .await
        .map_err(provider_account_mutation_error)?;
    amp_accounts_response(deps).await
}

pub(in crate::daemon::providers::accounts::routes) async fn claude_accounts_response(
    deps: ProviderAccountRouteDeps<'_>,
) -> Result<ClaudeAccountsResponse, ProviderAccountRouteError> {
    provider_accounts::load_claude_registry(deps.data_root)
        .await
        .map(ClaudeAccountsResponse::from)
        .map_err(internal_error)
}

pub(in crate::daemon::providers::accounts::routes) async fn add_claude_account_response(
    deps: ProviderAccountRouteDeps<'_>,
    label: Option<String>,
    setup_token: String,
) -> Result<ClaudeAccountsResponse, ProviderAccountRouteError> {
    provider_accounts::add_claude_account(deps.data_root, label, setup_token)
        .await
        .map_err(ProviderAccountMutationError::BadRequest)
        .map_err(provider_account_mutation_error)?;
    restart_provider_for_auth_change(deps, CLAUDE_PROVIDER_ID, "claude auth updated")
        .await
        .map_err(provider_account_mutation_error)?;
    claude_accounts_response(deps).await
}

pub(in crate::daemon::providers::accounts::routes) async fn set_active_claude_account_response(
    deps: ProviderAccountRouteDeps<'_>,
    account_id: Option<String>,
) -> Result<ClaudeAccountsResponse, ProviderAccountRouteError> {
    if account_id.is_some() {
        let registry = provider_accounts::load_claude_registry(deps.data_root)
            .await
            .map_err(internal_error)?;
        ensure_known_account(&account_id, &registry.accounts, |account| &account.id)?;
    }
    provider_accounts::set_active_claude_account(deps.data_root, account_id)
        .await
        .map_err(ProviderAccountMutationError::BadRequest)
        .map_err(provider_account_mutation_error)?;
    restart_provider_for_auth_change(deps, CLAUDE_PROVIDER_ID, "claude auth updated")
        .await
        .map_err(provider_account_mutation_error)?;
    claude_accounts_response(deps).await
}

pub(in crate::daemon::providers::accounts::routes) async fn delete_claude_account_response(
    deps: ProviderAccountRouteDeps<'_>,
    account_id: &str,
) -> Result<ClaudeAccountsResponse, ProviderAccountRouteError> {
    provider_accounts::remove_claude_account(deps.data_root, account_id)
        .await
        .map_err(ProviderAccountMutationError::Delete)
        .map_err(provider_account_mutation_error)?;
    restart_provider_for_auth_change(deps, CLAUDE_PROVIDER_ID, "claude auth updated")
        .await
        .map_err(provider_account_mutation_error)?;
    claude_accounts_response(deps).await
}

pub(in crate::daemon::providers::accounts::routes) async fn copilot_accounts_response(
    deps: ProviderAccountRouteDeps<'_>,
) -> Result<CopilotAccountsResponse, ProviderAccountRouteError> {
    provider_accounts::load_copilot_registry(deps.data_root)
        .await
        .map(CopilotAccountsResponse::from)
        .map_err(internal_error)
}

pub(in crate::daemon::providers::accounts::routes) async fn add_copilot_account_response(
    deps: ProviderAccountRouteDeps<'_>,
    label: Option<String>,
    token: String,
    email: Option<String>,
) -> Result<CopilotAccountsResponse, ProviderAccountRouteError> {
    provider_accounts::add_copilot_account(deps.data_root, label, token, email)
        .await
        .map_err(ProviderAccountMutationError::BadRequest)
        .map_err(provider_account_mutation_error)?;
    restart_provider_for_auth_change(deps, COPILOT_PROVIDER_ID, "copilot auth updated")
        .await
        .map_err(provider_account_mutation_error)?;
    copilot_accounts_response(deps).await
}

pub(in crate::daemon::providers::accounts::routes) async fn set_active_copilot_account_response(
    deps: ProviderAccountRouteDeps<'_>,
    account_id: Option<String>,
) -> Result<CopilotAccountsResponse, ProviderAccountRouteError> {
    if account_id.is_some() {
        let registry = provider_accounts::load_copilot_registry(deps.data_root)
            .await
            .map_err(internal_error)?;
        ensure_known_account(&account_id, &registry.accounts, |account| &account.id)?;
    }
    provider_accounts::set_active_copilot_account(deps.data_root, account_id)
        .await
        .map_err(ProviderAccountMutationError::BadRequest)
        .map_err(provider_account_mutation_error)?;
    restart_provider_for_auth_change(deps, COPILOT_PROVIDER_ID, "copilot auth updated")
        .await
        .map_err(provider_account_mutation_error)?;
    copilot_accounts_response(deps).await
}

pub(in crate::daemon::providers::accounts::routes) async fn delete_copilot_account_response(
    deps: ProviderAccountRouteDeps<'_>,
    account_id: &str,
) -> Result<CopilotAccountsResponse, ProviderAccountRouteError> {
    provider_accounts::remove_copilot_account(deps.data_root, account_id)
        .await
        .map_err(ProviderAccountMutationError::Delete)
        .map_err(provider_account_mutation_error)?;
    restart_provider_for_auth_change(deps, COPILOT_PROVIDER_ID, "copilot auth updated")
        .await
        .map_err(provider_account_mutation_error)?;
    copilot_accounts_response(deps).await
}

pub(in crate::daemon::providers::accounts::routes) async fn cursor_accounts_response(
    deps: ProviderAccountRouteDeps<'_>,
) -> Result<CursorAccountsResponse, ProviderAccountRouteError> {
    provider_accounts::load_cursor_registry(deps.data_root)
        .await
        .map(CursorAccountsResponse::from)
        .map_err(internal_error)
}

pub(in crate::daemon::providers::accounts::routes) async fn add_cursor_account_response(
    deps: ProviderAccountRouteDeps<'_>,
    label: Option<String>,
    token: String,
    email: Option<String>,
) -> Result<CursorAccountsResponse, ProviderAccountRouteError> {
    provider_accounts::add_cursor_account(deps.data_root, label, token, email)
        .await
        .map_err(ProviderAccountMutationError::BadRequest)
        .map_err(provider_account_mutation_error)?;
    restart_provider_for_auth_change(deps, CURSOR_PROVIDER_ID, "cursor auth updated")
        .await
        .map_err(provider_account_mutation_error)?;
    cursor_accounts_response(deps).await
}

pub(in crate::daemon::providers::accounts::routes) async fn set_active_cursor_account_response(
    deps: ProviderAccountRouteDeps<'_>,
    account_id: Option<String>,
) -> Result<CursorAccountsResponse, ProviderAccountRouteError> {
    if account_id.is_some() {
        let registry = provider_accounts::load_cursor_registry(deps.data_root)
            .await
            .map_err(internal_error)?;
        ensure_known_account(&account_id, &registry.accounts, |account| &account.id)?;
    }
    provider_accounts::set_active_cursor_account(deps.data_root, account_id)
        .await
        .map_err(ProviderAccountMutationError::BadRequest)
        .map_err(provider_account_mutation_error)?;
    restart_provider_for_auth_change(deps, CURSOR_PROVIDER_ID, "cursor auth updated")
        .await
        .map_err(provider_account_mutation_error)?;
    cursor_accounts_response(deps).await
}

pub(in crate::daemon::providers::accounts::routes) async fn delete_cursor_account_response(
    deps: ProviderAccountRouteDeps<'_>,
    account_id: &str,
) -> Result<CursorAccountsResponse, ProviderAccountRouteError> {
    provider_accounts::remove_cursor_account(deps.data_root, account_id)
        .await
        .map_err(ProviderAccountMutationError::Delete)
        .map_err(provider_account_mutation_error)?;
    restart_provider_for_auth_change(deps, CURSOR_PROVIDER_ID, "cursor auth updated")
        .await
        .map_err(provider_account_mutation_error)?;
    cursor_accounts_response(deps).await
}

pub(in crate::daemon::providers::accounts::routes) async fn gemini_accounts_response(
    deps: ProviderAccountRouteDeps<'_>,
) -> Result<GeminiAccountsResponse, ProviderAccountRouteError> {
    provider_accounts::load_gemini_registry(deps.data_root)
        .await
        .map(GeminiAccountsResponse::from)
        .map_err(internal_error)
}

pub(in crate::daemon::providers::accounts::routes) async fn add_gemini_account_response(
    deps: ProviderAccountRouteDeps<'_>,
    label: Option<String>,
    oauth_creds_json: String,
    google_accounts_json: Option<String>,
    email: Option<String>,
) -> Result<GeminiAccountsResponse, ProviderAccountRouteError> {
    provider_accounts::add_gemini_account(
        deps.data_root,
        label,
        oauth_creds_json,
        google_accounts_json,
        email,
    )
    .await
    .map_err(ProviderAccountMutationError::BadRequest)
    .map_err(provider_account_mutation_error)?;
    restart_provider_for_auth_change(deps, GEMINI_PROVIDER_ID, "gemini auth updated")
        .await
        .map_err(provider_account_mutation_error)?;
    gemini_accounts_response(deps).await
}

pub(in crate::daemon::providers::accounts::routes) async fn set_active_gemini_account_response(
    deps: ProviderAccountRouteDeps<'_>,
    account_id: Option<String>,
) -> Result<GeminiAccountsResponse, ProviderAccountRouteError> {
    if account_id.is_some() {
        let registry = provider_accounts::load_gemini_registry(deps.data_root)
            .await
            .map_err(internal_error)?;
        ensure_known_account(&account_id, &registry.accounts, |account| &account.id)?;
    }
    provider_accounts::set_active_gemini_account(deps.data_root, account_id)
        .await
        .map_err(ProviderAccountMutationError::BadRequest)
        .map_err(provider_account_mutation_error)?;
    restart_provider_for_auth_change(deps, GEMINI_PROVIDER_ID, "gemini auth updated")
        .await
        .map_err(provider_account_mutation_error)?;
    gemini_accounts_response(deps).await
}

pub(in crate::daemon::providers::accounts::routes) async fn delete_gemini_account_response(
    deps: ProviderAccountRouteDeps<'_>,
    account_id: &str,
) -> Result<GeminiAccountsResponse, ProviderAccountRouteError> {
    provider_accounts::remove_gemini_account(deps.data_root, account_id)
        .await
        .map_err(ProviderAccountMutationError::Delete)
        .map_err(provider_account_mutation_error)?;
    restart_provider_for_auth_change(deps, GEMINI_PROVIDER_ID, "gemini auth updated")
        .await
        .map_err(provider_account_mutation_error)?;
    gemini_accounts_response(deps).await
}

pub(in crate::daemon::providers::accounts::routes) async fn kimi_accounts_response(
    deps: ProviderAccountRouteDeps<'_>,
) -> Result<KimiAccountsResponse, ProviderAccountRouteError> {
    provider_accounts::load_kimi_registry(deps.data_root)
        .await
        .map(KimiAccountsResponse::from)
        .map_err(internal_error)
}

pub(in crate::daemon::providers::accounts::routes) async fn add_kimi_account_response(
    deps: ProviderAccountRouteDeps<'_>,
    label: Option<String>,
    provider: Option<String>,
    credentials_json: String,
    config_toml: Option<String>,
    email: Option<String>,
) -> Result<KimiAccountsResponse, ProviderAccountRouteError> {
    provider_accounts::add_kimi_account(
        deps.data_root,
        label,
        provider,
        credentials_json,
        config_toml,
        email,
    )
    .await
    .map_err(ProviderAccountMutationError::BadRequest)
    .map_err(provider_account_mutation_error)?;
    restart_provider_for_auth_change(deps, KIMI_PROVIDER_ID, "kimi auth updated")
        .await
        .map_err(provider_account_mutation_error)?;
    kimi_accounts_response(deps).await
}

pub(in crate::daemon::providers::accounts::routes) async fn set_active_kimi_account_response(
    deps: ProviderAccountRouteDeps<'_>,
    account_id: Option<String>,
) -> Result<KimiAccountsResponse, ProviderAccountRouteError> {
    if account_id.is_some() {
        let registry = provider_accounts::load_kimi_registry(deps.data_root)
            .await
            .map_err(internal_error)?;
        ensure_known_account(&account_id, &registry.accounts, |account| &account.id)?;
    }
    provider_accounts::set_active_kimi_account(deps.data_root, account_id)
        .await
        .map_err(ProviderAccountMutationError::BadRequest)
        .map_err(provider_account_mutation_error)?;
    restart_provider_for_auth_change(deps, KIMI_PROVIDER_ID, "kimi auth updated")
        .await
        .map_err(provider_account_mutation_error)?;
    kimi_accounts_response(deps).await
}

pub(in crate::daemon::providers::accounts::routes) async fn delete_kimi_account_response(
    deps: ProviderAccountRouteDeps<'_>,
    account_id: &str,
) -> Result<KimiAccountsResponse, ProviderAccountRouteError> {
    provider_accounts::remove_kimi_account(deps.data_root, account_id)
        .await
        .map_err(ProviderAccountMutationError::Delete)
        .map_err(provider_account_mutation_error)?;
    restart_provider_for_auth_change(deps, KIMI_PROVIDER_ID, "kimi auth updated")
        .await
        .map_err(provider_account_mutation_error)?;
    kimi_accounts_response(deps).await
}

pub(in crate::daemon::providers::accounts::routes) async fn mistral_accounts_response(
    deps: ProviderAccountRouteDeps<'_>,
) -> Result<MistralAccountsResponse, ProviderAccountRouteError> {
    provider_accounts::load_mistral_registry(deps.data_root)
        .await
        .map(MistralAccountsResponse::from)
        .map_err(internal_error)
}

pub(in crate::daemon::providers::accounts::routes) async fn upsert_mistral_account_response(
    deps: ProviderAccountRouteDeps<'_>,
    label: Option<String>,
    email: Option<String>,
) -> Result<MistralAccountsResponse, ProviderAccountRouteError> {
    provider_accounts::upsert_mistral_account(deps.data_root, label, email)
        .await
        .map_err(ProviderAccountMutationError::BadRequest)
        .map_err(provider_account_mutation_error)?;
    restart_provider_for_auth_change(deps, MISTRAL_PROVIDER_ID, "mistral auth updated")
        .await
        .map_err(provider_account_mutation_error)?;
    mistral_accounts_response(deps).await
}

pub(in crate::daemon::providers::accounts::routes) async fn set_active_mistral_account_response(
    deps: ProviderAccountRouteDeps<'_>,
    account_id: Option<String>,
) -> Result<MistralAccountsResponse, ProviderAccountRouteError> {
    if account_id.is_some() {
        let registry = provider_accounts::load_mistral_registry(deps.data_root)
            .await
            .map_err(internal_error)?;
        ensure_known_account(&account_id, &registry.accounts, |account| &account.id)?;
    }
    provider_accounts::set_active_mistral_account(deps.data_root, account_id)
        .await
        .map_err(ProviderAccountMutationError::BadRequest)
        .map_err(provider_account_mutation_error)?;
    restart_provider_for_auth_change(deps, MISTRAL_PROVIDER_ID, "mistral auth updated")
        .await
        .map_err(provider_account_mutation_error)?;
    mistral_accounts_response(deps).await
}

pub(in crate::daemon::providers::accounts::routes) async fn delete_mistral_account_response(
    deps: ProviderAccountRouteDeps<'_>,
    account_id: &str,
) -> Result<MistralAccountsResponse, ProviderAccountRouteError> {
    provider_accounts::remove_mistral_account(deps.data_root, account_id)
        .await
        .map_err(ProviderAccountMutationError::Delete)
        .map_err(provider_account_mutation_error)?;
    restart_provider_for_auth_change(deps, MISTRAL_PROVIDER_ID, "mistral auth updated")
        .await
        .map_err(provider_account_mutation_error)?;
    mistral_accounts_response(deps).await
}

pub(in crate::daemon::providers::accounts::routes) async fn qwen_accounts_response(
    deps: ProviderAccountRouteDeps<'_>,
) -> Result<QwenAccountsResponse, ProviderAccountRouteError> {
    provider_accounts::load_qwen_registry(deps.data_root)
        .await
        .map(QwenAccountsResponse::from)
        .map_err(internal_error)
}

pub(in crate::daemon::providers::accounts::routes) async fn add_qwen_account_response(
    deps: ProviderAccountRouteDeps<'_>,
    label: Option<String>,
    oauth_creds_json: String,
    email: Option<String>,
) -> Result<QwenAccountsResponse, ProviderAccountRouteError> {
    provider_accounts::add_qwen_account(deps.data_root, label, oauth_creds_json, email)
        .await
        .map_err(ProviderAccountMutationError::BadRequest)
        .map_err(provider_account_mutation_error)?;
    restart_provider_for_auth_change(deps, QWEN_PROVIDER_ID, "qwen auth updated")
        .await
        .map_err(provider_account_mutation_error)?;
    qwen_accounts_response(deps).await
}

pub(in crate::daemon::providers::accounts::routes) async fn set_active_qwen_account_response(
    deps: ProviderAccountRouteDeps<'_>,
    account_id: Option<String>,
) -> Result<QwenAccountsResponse, ProviderAccountRouteError> {
    if account_id.is_some() {
        let registry = provider_accounts::load_qwen_registry(deps.data_root)
            .await
            .map_err(internal_error)?;
        ensure_known_account(&account_id, &registry.accounts, |account| &account.id)?;
    }
    provider_accounts::set_active_qwen_account(deps.data_root, account_id)
        .await
        .map_err(ProviderAccountMutationError::BadRequest)
        .map_err(provider_account_mutation_error)?;
    restart_provider_for_auth_change(deps, QWEN_PROVIDER_ID, "qwen auth updated")
        .await
        .map_err(provider_account_mutation_error)?;
    qwen_accounts_response(deps).await
}

pub(in crate::daemon::providers::accounts::routes) async fn delete_qwen_account_response(
    deps: ProviderAccountRouteDeps<'_>,
    account_id: &str,
) -> Result<QwenAccountsResponse, ProviderAccountRouteError> {
    provider_accounts::remove_qwen_account(deps.data_root, account_id)
        .await
        .map_err(ProviderAccountMutationError::Delete)
        .map_err(provider_account_mutation_error)?;
    restart_provider_for_auth_change(deps, QWEN_PROVIDER_ID, "qwen auth updated")
        .await
        .map_err(provider_account_mutation_error)?;
    qwen_accounts_response(deps).await
}

async fn restart_provider_for_auth_change(
    deps: ProviderAccountRouteDeps<'_>,
    provider_id: &str,
    reason: &str,
) -> Result<(), ProviderAccountMutationError> {
    restarts::restart_provider_for_auth_change_with_runtime(deps.providers, provider_id, reason)
        .await
        .map_err(ProviderAccountMutationError::Internal)
}

async fn stop_provider_for_auth_removal(
    deps: ProviderAccountRouteDeps<'_>,
    provider_id: &str,
    reason: &str,
) -> anyhow::Result<()> {
    restarts::stop_provider_for_auth_removal_with_runtime(deps.providers, provider_id, reason).await
}
