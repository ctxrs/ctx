use std::collections::HashMap;

use ctx_core::ids::WorkspaceId;
use ctx_observability::logs;
use ctx_provider_accounts::{
    self as provider_accounts, AmpAccountsResponse, ClaudeAccountsResponse, CodexAccountsResponse,
    CopilotAccountsResponse, CursorAccountsResponse, GeminiAccountsResponse, KimiAccountsResponse,
    MistralAccountsResponse, QwenAccountsResponse,
};
use ctx_provider_runtime::{provider_bootstrap, provider_status_service as status_service};
use ctx_provider_runtime::{
    ProvidersBootstrapResponse, ProvidersBootstrapRouteError, ProvidersBootstrapRouteRequest,
};
use ctx_providers::adapters::ProviderStatus;
use futures::StreamExt;

use crate::daemon::ProviderBootstrapHandle;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum ProvidersBootstrapErrorKind {
    NotFound,
    Internal,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct ProvidersBootstrapError {
    kind: ProvidersBootstrapErrorKind,
    message: String,
}

impl ProvidersBootstrapError {
    fn not_found(message: impl Into<String>) -> Self {
        Self {
            kind: ProvidersBootstrapErrorKind::NotFound,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            kind: ProvidersBootstrapErrorKind::Internal,
            message: message.into(),
        }
    }

    fn kind(&self) -> ProvidersBootstrapErrorKind {
        self.kind
    }

    fn message(&self) -> &str {
        &self.message
    }
}

impl ProviderBootstrapHandle {
    pub async fn workspace_providers_bootstrap_for_route(
        &self,
        request: ProvidersBootstrapRouteRequest,
    ) -> Result<ProvidersBootstrapResponse, ProvidersBootstrapRouteError> {
        let workspace_id = parse_bootstrap_workspace_id(request.workspace_id())?;
        workspace_providers_bootstrap(self, workspace_id)
            .await
            .map_err(bootstrap_route_error)
    }
}

fn parse_bootstrap_workspace_id(
    workspace_id: &str,
) -> Result<WorkspaceId, ProvidersBootstrapRouteError> {
    uuid::Uuid::parse_str(workspace_id)
        .map(WorkspaceId)
        .map_err(|_| ProvidersBootstrapRouteError::bad_request("invalid workspace id"))
}

fn bootstrap_route_error(error: ProvidersBootstrapError) -> ProvidersBootstrapRouteError {
    match error.kind() {
        ProvidersBootstrapErrorKind::NotFound => {
            ProvidersBootstrapRouteError::not_found(error.message())
        }
        ProvidersBootstrapErrorKind::Internal => {
            ProvidersBootstrapRouteError::internal(error.message())
        }
    }
}

async fn workspace_providers_bootstrap(
    handle: &ProviderBootstrapHandle,
    ws_id: WorkspaceId,
) -> Result<ProvidersBootstrapResponse, ProvidersBootstrapError> {
    load_bootstrap_workspace(handle, ws_id).await?;
    let install_target = handle
        .install_target_for_workspace(ws_id)
        .await
        .map_err(|error| {
            ProvidersBootstrapError::internal(format!(
                "failed to load workspace execution settings: {error:#}"
            ))
        })?;
    let preferred_model_by_provider =
        std::sync::Arc::new(load_preferred_model_by_provider(handle, ws_id).await?);

    let provider_statuses =
        status_service::providers_statuses_response(handle, install_target, true).await;
    let visible_providers = provider_statuses
        .iter()
        .filter(|provider| provider_bootstrap::should_build_bootstrap_options(provider))
        .cloned()
        .collect::<Vec<_>>();

    let per_provider =
        futures::stream::iter(visible_providers.into_iter().map(|provider_status| {
            let preferred_model_by_provider = std::sync::Arc::clone(&preferred_model_by_provider);
            async move {
                let preferred_model_id = preferred_model_by_provider
                    .get(&provider_status.provider_id)
                    .cloned();
                build_bootstrap_options(handle, ws_id, provider_status, preferred_model_id).await
            }
        }))
        .buffer_unordered(provider_bootstrap::visible_provider_count_hint(
            provider_statuses.len(),
        ))
        .collect::<Vec<_>>()
        .await;

    let mut provider_options = HashMap::new();
    let mut provider_harness_config = HashMap::new();
    for (provider_id, options, source_config) in per_provider {
        provider_options.insert(provider_id.clone(), options);
        if let Some(config) = source_config {
            provider_harness_config.insert(provider_id, config);
        }
    }

    let accounts = load_bootstrap_accounts(handle).await?;

    Ok(ProvidersBootstrapResponse::new(
        provider_statuses,
        provider_options,
        provider_harness_config,
        accounts.codex_accounts,
        accounts.claude_accounts,
        accounts.gemini_accounts,
        accounts.qwen_accounts,
        accounts.kimi_accounts,
        accounts.mistral_accounts,
        accounts.copilot_accounts,
        accounts.cursor_accounts,
        accounts.amp_accounts,
    ))
}

#[cfg(test)]
mod route_tests {
    use ctx_provider_runtime::ProvidersBootstrapRouteErrorKind;

    use super::*;

    #[test]
    fn bootstrap_route_parse_failure_preserves_invalid_workspace_body() {
        let error = parse_bootstrap_workspace_id("not-a-uuid").unwrap_err();

        assert_eq!(error.kind(), ProvidersBootstrapRouteErrorKind::BadRequest);
        assert_eq!(error.body()["error"].as_str(), Some("invalid workspace id"));
    }

    #[test]
    fn bootstrap_route_error_preserves_not_found_body() {
        let error =
            bootstrap_route_error(ProvidersBootstrapError::not_found("workspace not found"));

        assert_eq!(error.kind(), ProvidersBootstrapRouteErrorKind::NotFound);
        assert_eq!(error.body()["error"].as_str(), Some("workspace not found"));
    }

    #[test]
    fn bootstrap_route_error_preserves_internal_body() {
        let error = bootstrap_route_error(ProvidersBootstrapError::internal(
            "failed to load workspace execution settings: boom",
        ));

        assert_eq!(error.kind(), ProvidersBootstrapRouteErrorKind::Internal);
        assert_eq!(
            error.body()["error"].as_str(),
            Some("failed to load workspace execution settings: boom")
        );
    }
}

async fn load_bootstrap_workspace(
    handle: &ProviderBootstrapHandle,
    ws_id: WorkspaceId,
) -> Result<(), ProvidersBootstrapError> {
    let exists = handle
        .global_store()
        .get_workspace(ws_id)
        .await
        .map_err(|_| ProvidersBootstrapError::internal("failed to load workspace"))?
        .is_some();
    if exists {
        return Ok(());
    }

    Err(ProvidersBootstrapError::not_found("workspace not found"))
}

async fn load_preferred_model_by_provider(
    handle: &ProviderBootstrapHandle,
    ws_id: WorkspaceId,
) -> Result<HashMap<String, String>, ProvidersBootstrapError> {
    let store = handle.store_for_workspace(ws_id).await.map_err(|error| {
        ProvidersBootstrapError::internal(format!(
            "failed to load workspace provider model preferences: {}",
            logs::redact_sensitive(&error.to_string())
        ))
    })?;
    ctx_workspace_config::load_preferred_new_session_models(&store)
        .await
        .map_err(|error| {
            ProvidersBootstrapError::internal(format!(
                "failed to load workspace provider model preferences: {}",
                logs::redact_sensitive(&error.to_string())
            ))
        })
}

struct BootstrapAccounts {
    codex_accounts: CodexAccountsResponse,
    claude_accounts: ClaudeAccountsResponse,
    gemini_accounts: GeminiAccountsResponse,
    qwen_accounts: QwenAccountsResponse,
    kimi_accounts: KimiAccountsResponse,
    mistral_accounts: MistralAccountsResponse,
    copilot_accounts: CopilotAccountsResponse,
    cursor_accounts: CursorAccountsResponse,
    amp_accounts: AmpAccountsResponse,
}

fn bootstrap_accounts_error(provider_id: &str, err: anyhow::Error) -> ProvidersBootstrapError {
    ProvidersBootstrapError::internal(format!(
        "failed to load {provider_id} accounts: {}",
        logs::redact_sensitive(&err.to_string())
    ))
}

async fn load_bootstrap_accounts(
    handle: &ProviderBootstrapHandle,
) -> Result<BootstrapAccounts, ProvidersBootstrapError> {
    let codex_accounts = load_bootstrap_codex_accounts(handle)
        .await
        .map_err(|err| bootstrap_accounts_error("codex", err))?;
    let claude_registry = provider_accounts::load_claude_registry(handle.data_root())
        .await
        .map_err(|err| bootstrap_accounts_error("claude-crp", err))?;
    let gemini_registry = provider_accounts::load_gemini_registry(handle.data_root())
        .await
        .map_err(|err| bootstrap_accounts_error("gemini", err))?;
    let qwen_registry = provider_accounts::load_qwen_registry(handle.data_root())
        .await
        .map_err(|err| bootstrap_accounts_error("qwen", err))?;
    let kimi_registry = provider_accounts::load_kimi_registry(handle.data_root())
        .await
        .map_err(|err| bootstrap_accounts_error("kimi", err))?;
    let mistral_registry = provider_accounts::load_mistral_registry(handle.data_root())
        .await
        .map_err(|err| bootstrap_accounts_error("mistral", err))?;
    let copilot_registry = provider_accounts::load_copilot_registry(handle.data_root())
        .await
        .map_err(|err| bootstrap_accounts_error("copilot", err))?;
    let cursor_registry = provider_accounts::load_cursor_registry(handle.data_root())
        .await
        .map_err(|err| bootstrap_accounts_error("cursor", err))?;
    let amp_registry = provider_accounts::ensure_amp_registry_from_runtime_auth(handle.data_root())
        .await
        .map_err(|err| bootstrap_accounts_error("amp", err))?;

    Ok(BootstrapAccounts {
        codex_accounts,
        claude_accounts: ClaudeAccountsResponse::from(claude_registry),
        gemini_accounts: GeminiAccountsResponse::from(gemini_registry),
        qwen_accounts: QwenAccountsResponse::from(qwen_registry),
        kimi_accounts: KimiAccountsResponse::from(kimi_registry),
        mistral_accounts: MistralAccountsResponse::from(mistral_registry),
        copilot_accounts: CopilotAccountsResponse::from(copilot_registry),
        cursor_accounts: CursorAccountsResponse::from(cursor_registry),
        amp_accounts: AmpAccountsResponse::from(amp_registry),
    })
}

async fn load_bootstrap_codex_accounts(
    handle: &ProviderBootstrapHandle,
) -> anyhow::Result<CodexAccountsResponse> {
    let registry = provider_accounts::load_codex_registry(handle.data_root()).await?;
    let logins = handle
        .providers()
        .with_codex_login_sessions(|map| map.values().cloned().collect())
        .await;
    Ok(CodexAccountsResponse::new(
        registry.active_account_id,
        registry.accounts,
        logins,
    ))
}

async fn build_bootstrap_options(
    handle: &ProviderBootstrapHandle,
    ws_id: WorkspaceId,
    provider_status: ProviderStatus,
    preferred_model_id: Option<String>,
) -> (
    String,
    serde_json::Value,
    Option<ctx_harness_sources::HarnessProviderSourceConfig>,
) {
    let options = provider_bootstrap::build_provider_bootstrap_options(
        handle.data_root(),
        ws_id,
        provider_status,
        preferred_model_id,
    )
    .await;
    (options.provider_id, options.options, options.source_config)
}
