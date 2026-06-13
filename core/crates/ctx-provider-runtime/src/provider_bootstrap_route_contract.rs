use std::collections::HashMap;

use ctx_harness_sources::HarnessProviderSourceConfig;
use ctx_provider_accounts::{
    AmpAccountsResponse, ClaudeAccountsResponse, CodexAccountsResponse, CopilotAccountsResponse,
    CursorAccountsResponse, GeminiAccountsResponse, KimiAccountsResponse, MistralAccountsResponse,
    QwenAccountsResponse,
};
use ctx_providers::adapters::ProviderStatus;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize)]
pub struct ProvidersBootstrapResponse {
    providers: Vec<ProviderStatus>,
    provider_options: HashMap<String, serde_json::Value>,
    provider_harness_config: HashMap<String, HarnessProviderSourceConfig>,
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

impl ProvidersBootstrapResponse {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        providers: Vec<ProviderStatus>,
        provider_options: HashMap<String, serde_json::Value>,
        provider_harness_config: HashMap<String, HarnessProviderSourceConfig>,
        codex_accounts: CodexAccountsResponse,
        claude_accounts: ClaudeAccountsResponse,
        gemini_accounts: GeminiAccountsResponse,
        qwen_accounts: QwenAccountsResponse,
        kimi_accounts: KimiAccountsResponse,
        mistral_accounts: MistralAccountsResponse,
        copilot_accounts: CopilotAccountsResponse,
        cursor_accounts: CursorAccountsResponse,
        amp_accounts: AmpAccountsResponse,
    ) -> Self {
        Self {
            providers,
            provider_options,
            provider_harness_config,
            codex_accounts,
            claude_accounts,
            gemini_accounts,
            qwen_accounts,
            kimi_accounts,
            mistral_accounts,
            copilot_accounts,
            cursor_accounts,
            amp_accounts,
        }
    }
}

#[derive(Debug)]
pub struct ProvidersBootstrapRouteRequest {
    workspace_id: String,
}

impl ProvidersBootstrapRouteRequest {
    pub fn new(workspace_id: String) -> Self {
        Self { workspace_id }
    }

    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ProvidersBootstrapRouteErrorKind {
    BadRequest,
    NotFound,
    Internal,
}

#[derive(Debug)]
pub struct ProvidersBootstrapRouteError {
    kind: ProvidersBootstrapRouteErrorKind,
    body: Value,
}

impl ProvidersBootstrapRouteError {
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::with_message(ProvidersBootstrapRouteErrorKind::BadRequest, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::with_message(ProvidersBootstrapRouteErrorKind::NotFound, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::with_message(ProvidersBootstrapRouteErrorKind::Internal, message)
    }

    pub fn kind(&self) -> ProvidersBootstrapRouteErrorKind {
        self.kind
    }

    pub fn body(&self) -> &Value {
        &self.body
    }

    fn with_message(kind: ProvidersBootstrapRouteErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            body: serde_json::json!({
                "error": message.into(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_route_error_preserves_json_body() {
        let error = ProvidersBootstrapRouteError::not_found("workspace not found");

        assert_eq!(error.kind(), ProvidersBootstrapRouteErrorKind::NotFound);
        assert_eq!(error.body()["error"].as_str(), Some("workspace not found"));
    }

    #[test]
    fn bootstrap_route_request_preserves_raw_workspace_id() {
        let request = ProvidersBootstrapRouteRequest::new(" raw-workspace ".to_string());

        assert_eq!(request.workspace_id(), " raw-workspace ");
    }
}
