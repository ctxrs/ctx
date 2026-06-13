#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Arc;

#[cfg(test)]
use anyhow::Context;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
#[cfg(test)]
use url::Url;

use super::errors::ApiErrorResp;
use ctx_daemon::daemon::{
    ProviderAccountsHandle, ProviderAdminHandle, ProviderAuthImportHandle, ProviderBootstrapHandle,
    ProviderHarnessConfigHandle, ProviderStatusHandle, ProviderUsageHandle,
};
#[cfg(test)]
use ctx_daemon::test_support::TestDaemon;
#[cfg(test)]
use ctx_harness_sources as harness_sources;
#[cfg(test)]
use ctx_harness_sources::{HarnessApiShape, HarnessSourceKind};
#[cfg(test)]
use ctx_managed_installs as installer;
use ctx_provider_accounts::route_contract::{
    AmpLoginStatusRouteResponse, ClaudeLoginRouteError, ClaudeLoginRouteErrorKind,
    ClaudeLoginStartRouteRequest, ClaudeLoginStartRouteResponse, ClaudeLoginStatusRouteResponse,
    CodexLoginCompleteRouteRequest, CodexLoginCompleteRouteResponse, CodexLoginRouteError,
    CodexLoginRouteErrorKind, CodexLoginStartRouteRequest, CodexLoginStartRouteResponse,
    CodexLoginStatusRouteResponse, CursorLoginRouteError, CursorLoginRouteErrorKind,
    CursorLoginStartRouteRequest, CursorLoginStartRouteResponse, CursorLoginStatusRouteResponse,
    GeminiLoginStatusRouteResponse, KimiLoginStatusRouteResponse, MistralLoginStatusRouteResponse,
    ProviderLoginRouteError, ProviderLoginRouteErrorKind, ProviderLoginStartRouteRequest,
    ProviderLoginStartRouteResponse, QwenLoginStatusRouteResponse,
};
#[cfg(test)]
use ctx_provider_install::install_state::InstallId;
#[cfg(test)]
use ctx_provider_install::install_state::InstallTarget;
use ctx_providers::adapters::ProviderStatus;

mod accounts;
mod bootstrap;
#[cfg(test)]
mod codex_auth_tests;
mod cursor_login;
mod harness_config;
mod imports;
mod install;
mod login;
mod status;

pub(super) use accounts::{
    delete_amp_account, delete_claude_account, delete_codex_account, delete_copilot_account,
    delete_cursor_account, delete_gemini_account, delete_kimi_account, delete_mistral_account,
    delete_qwen_account, get_codex_accounts_usage, import_host_codex_auth, list_amp_accounts,
    list_claude_accounts, list_codex_accounts, list_copilot_accounts, list_cursor_accounts,
    list_gemini_accounts, list_kimi_accounts, list_mistral_accounts, list_qwen_accounts,
    probe_host_codex_import, set_amp_active_account, set_claude_active_account,
    set_codex_active_account, set_copilot_active_account, set_cursor_active_account,
    set_gemini_active_account, set_kimi_active_account, set_mistral_active_account,
    set_qwen_active_account, upsert_amp_account, upsert_claude_account, upsert_copilot_account,
    upsert_cursor_account, upsert_gemini_account, upsert_kimi_account, upsert_mistral_account,
    upsert_qwen_account,
};
pub(super) use bootstrap::get_workspace_providers_bootstrap;
pub(super) use cursor_login::{get_cursor_login, start_cursor_login};
pub(super) use harness_config::{
    delete_provider_harness_endpoint, get_provider_harness_config,
    refresh_provider_harness_endpoint_models, select_provider_harness_source,
    set_provider_harness_endpoint_manual_models, upsert_provider_harness_endpoint,
};
pub(super) use imports::{
    import_provider_auth_candidates, list_provider_auth_import_candidates,
    list_provider_auth_import_profiles,
};
pub(super) use install::{dev_restart_providers, refresh_provider_matrix};
pub(super) use login::{
    complete_codex_login, get_amp_login, get_claude_login, get_codex_login, get_gemini_login,
    get_kimi_login, get_mistral_login, get_qwen_login, start_amp_login, start_claude_login,
    start_codex_login, start_gemini_login, start_kimi_login, start_mistral_login, start_qwen_login,
};
pub(super) use status::{get_provider, get_provider_usage, list_providers};

#[cfg(test)]
use ctx_provider_runtime::provider_auth::{
    endpoint_selection_is_active, provider_auth_mode, provider_has_active_auth_config,
};
use ctx_provider_runtime::{
    CodexAccountsUsageRouteResponse, ProviderAdminRouteError, ProviderAdminRouteErrorKind,
    ProviderDevRestartRouteRequest, ProviderDevRestartRouteResponse,
    ProviderMatrixRefreshRouteResponse, ProviderStatusRouteQuery, ProviderUsageRouteError,
    ProviderUsageRouteQuery, ProviderUsageRouteSnapshot, SelectProviderHarnessSourceRouteRequest,
};
#[cfg(test)]
use login::{expected_callback_from_auth_url, validate_callback_url};
#[cfg(test)]
mod tests;
