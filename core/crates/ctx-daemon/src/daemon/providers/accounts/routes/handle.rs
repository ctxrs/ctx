use crate::daemon::ProviderAccountsHandle;
use ctx_provider_accounts::{
    AmpAccountUpsertRouteRequest, AmpAccountsResponse, ClaudeAccountUpsertRouteRequest,
    ClaudeAccountsResponse, CodexAccountsResponse, CodexHostImportProbeRouteResponse,
    CodexHostImportRouteRequest, CopilotAccountUpsertRouteRequest, CopilotAccountsResponse,
    CursorAccountUpsertRouteRequest, CursorAccountsResponse, GeminiAccountUpsertRouteRequest,
    GeminiAccountsResponse, KimiAccountUpsertRouteRequest, KimiAccountsResponse,
    MistralAccountUpsertRouteRequest, MistralAccountsResponse, ProviderAccountRouteError,
    ProviderActiveAccountRouteRequest, QwenAccountUpsertRouteRequest, QwenAccountsResponse,
};

use super::operations;

impl ProviderAccountsHandle {
    fn route_deps(&self) -> operations::ProviderAccountRouteDeps<'_> {
        operations::ProviderAccountRouteDeps::new(self.data_root(), self.providers())
    }

    pub async fn codex_host_import_probe_for_route(&self) -> CodexHostImportProbeRouteResponse {
        super::super::probe_host_codex_auth_candidate().await.into()
    }

    pub async fn codex_accounts_for_route(
        &self,
    ) -> Result<CodexAccountsResponse, ProviderAccountRouteError> {
        operations::codex_accounts_response(self.route_deps()).await
    }

    pub async fn import_host_codex_auth_for_route(
        &self,
        request: CodexHostImportRouteRequest,
    ) -> Result<CodexAccountsResponse, ProviderAccountRouteError> {
        operations::import_host_codex_auth_response(self.route_deps(), request.into_label()).await
    }

    pub async fn set_active_codex_account_for_route(
        &self,
        request: ProviderActiveAccountRouteRequest,
    ) -> Result<CodexAccountsResponse, ProviderAccountRouteError> {
        operations::set_active_codex_account_response(self.route_deps(), request.into_account_id())
            .await
    }

    pub async fn delete_codex_account_for_route(
        &self,
        account_id: &str,
    ) -> Result<CodexAccountsResponse, ProviderAccountRouteError> {
        operations::delete_codex_account_response(self.route_deps(), account_id).await
    }

    pub async fn amp_accounts_for_route(
        &self,
    ) -> Result<AmpAccountsResponse, ProviderAccountRouteError> {
        operations::amp_accounts_response(self.route_deps()).await
    }

    pub async fn upsert_amp_account_for_route(
        &self,
        request: AmpAccountUpsertRouteRequest,
    ) -> Result<AmpAccountsResponse, ProviderAccountRouteError> {
        let (label, email) = request.into_parts();
        operations::upsert_amp_account_response(self.route_deps(), label, email).await
    }

    pub async fn set_active_amp_account_for_route(
        &self,
        request: ProviderActiveAccountRouteRequest,
    ) -> Result<AmpAccountsResponse, ProviderAccountRouteError> {
        operations::set_active_amp_account_response(self.route_deps(), request.into_account_id())
            .await
    }

    pub async fn delete_amp_account_for_route(
        &self,
        account_id: &str,
    ) -> Result<AmpAccountsResponse, ProviderAccountRouteError> {
        operations::delete_amp_account_response(self.route_deps(), account_id).await
    }

    pub async fn claude_accounts_for_route(
        &self,
    ) -> Result<ClaudeAccountsResponse, ProviderAccountRouteError> {
        operations::claude_accounts_response(self.route_deps()).await
    }

    pub async fn upsert_claude_account_for_route(
        &self,
        request: ClaudeAccountUpsertRouteRequest,
    ) -> Result<ClaudeAccountsResponse, ProviderAccountRouteError> {
        let (label, setup_token) = request.into_parts();
        operations::add_claude_account_response(self.route_deps(), label, setup_token).await
    }

    pub async fn set_active_claude_account_for_route(
        &self,
        request: ProviderActiveAccountRouteRequest,
    ) -> Result<ClaudeAccountsResponse, ProviderAccountRouteError> {
        operations::set_active_claude_account_response(self.route_deps(), request.into_account_id())
            .await
    }

    pub async fn delete_claude_account_for_route(
        &self,
        account_id: &str,
    ) -> Result<ClaudeAccountsResponse, ProviderAccountRouteError> {
        operations::delete_claude_account_response(self.route_deps(), account_id).await
    }

    pub async fn copilot_accounts_for_route(
        &self,
    ) -> Result<CopilotAccountsResponse, ProviderAccountRouteError> {
        operations::copilot_accounts_response(self.route_deps()).await
    }

    pub async fn upsert_copilot_account_for_route(
        &self,
        request: CopilotAccountUpsertRouteRequest,
    ) -> Result<CopilotAccountsResponse, ProviderAccountRouteError> {
        let (label, token, email) = request.into_parts();
        operations::add_copilot_account_response(self.route_deps(), label, token, email).await
    }

    pub async fn set_active_copilot_account_for_route(
        &self,
        request: ProviderActiveAccountRouteRequest,
    ) -> Result<CopilotAccountsResponse, ProviderAccountRouteError> {
        operations::set_active_copilot_account_response(
            self.route_deps(),
            request.into_account_id(),
        )
        .await
    }

    pub async fn delete_copilot_account_for_route(
        &self,
        account_id: &str,
    ) -> Result<CopilotAccountsResponse, ProviderAccountRouteError> {
        operations::delete_copilot_account_response(self.route_deps(), account_id).await
    }

    pub async fn cursor_accounts_for_route(
        &self,
    ) -> Result<CursorAccountsResponse, ProviderAccountRouteError> {
        operations::cursor_accounts_response(self.route_deps()).await
    }

    pub async fn upsert_cursor_account_for_route(
        &self,
        request: CursorAccountUpsertRouteRequest,
    ) -> Result<CursorAccountsResponse, ProviderAccountRouteError> {
        let (label, token, email) = request.into_parts();
        operations::add_cursor_account_response(self.route_deps(), label, token, email).await
    }

    pub async fn set_active_cursor_account_for_route(
        &self,
        request: ProviderActiveAccountRouteRequest,
    ) -> Result<CursorAccountsResponse, ProviderAccountRouteError> {
        operations::set_active_cursor_account_response(self.route_deps(), request.into_account_id())
            .await
    }

    pub async fn delete_cursor_account_for_route(
        &self,
        account_id: &str,
    ) -> Result<CursorAccountsResponse, ProviderAccountRouteError> {
        operations::delete_cursor_account_response(self.route_deps(), account_id).await
    }

    pub async fn gemini_accounts_for_route(
        &self,
    ) -> Result<GeminiAccountsResponse, ProviderAccountRouteError> {
        operations::gemini_accounts_response(self.route_deps()).await
    }

    pub async fn upsert_gemini_account_for_route(
        &self,
        request: GeminiAccountUpsertRouteRequest,
    ) -> Result<GeminiAccountsResponse, ProviderAccountRouteError> {
        let (label, oauth_creds_json, google_accounts_json, email) = request.into_parts();
        operations::add_gemini_account_response(
            self.route_deps(),
            label,
            oauth_creds_json,
            google_accounts_json,
            email,
        )
        .await
    }

    pub async fn set_active_gemini_account_for_route(
        &self,
        request: ProviderActiveAccountRouteRequest,
    ) -> Result<GeminiAccountsResponse, ProviderAccountRouteError> {
        operations::set_active_gemini_account_response(self.route_deps(), request.into_account_id())
            .await
    }

    pub async fn delete_gemini_account_for_route(
        &self,
        account_id: &str,
    ) -> Result<GeminiAccountsResponse, ProviderAccountRouteError> {
        operations::delete_gemini_account_response(self.route_deps(), account_id).await
    }

    pub async fn kimi_accounts_for_route(
        &self,
    ) -> Result<KimiAccountsResponse, ProviderAccountRouteError> {
        operations::kimi_accounts_response(self.route_deps()).await
    }

    pub async fn upsert_kimi_account_for_route(
        &self,
        request: KimiAccountUpsertRouteRequest,
    ) -> Result<KimiAccountsResponse, ProviderAccountRouteError> {
        let (label, provider, credentials_json, config_toml, email) = request.into_parts();
        operations::add_kimi_account_response(
            self.route_deps(),
            label,
            provider,
            credentials_json,
            config_toml,
            email,
        )
        .await
    }

    pub async fn set_active_kimi_account_for_route(
        &self,
        request: ProviderActiveAccountRouteRequest,
    ) -> Result<KimiAccountsResponse, ProviderAccountRouteError> {
        operations::set_active_kimi_account_response(self.route_deps(), request.into_account_id())
            .await
    }

    pub async fn delete_kimi_account_for_route(
        &self,
        account_id: &str,
    ) -> Result<KimiAccountsResponse, ProviderAccountRouteError> {
        operations::delete_kimi_account_response(self.route_deps(), account_id).await
    }

    pub async fn mistral_accounts_for_route(
        &self,
    ) -> Result<MistralAccountsResponse, ProviderAccountRouteError> {
        operations::mistral_accounts_response(self.route_deps()).await
    }

    pub async fn upsert_mistral_account_for_route(
        &self,
        request: MistralAccountUpsertRouteRequest,
    ) -> Result<MistralAccountsResponse, ProviderAccountRouteError> {
        let (label, email) = request.into_parts();
        operations::upsert_mistral_account_response(self.route_deps(), label, email).await
    }

    pub async fn set_active_mistral_account_for_route(
        &self,
        request: ProviderActiveAccountRouteRequest,
    ) -> Result<MistralAccountsResponse, ProviderAccountRouteError> {
        operations::set_active_mistral_account_response(
            self.route_deps(),
            request.into_account_id(),
        )
        .await
    }

    pub async fn delete_mistral_account_for_route(
        &self,
        account_id: &str,
    ) -> Result<MistralAccountsResponse, ProviderAccountRouteError> {
        operations::delete_mistral_account_response(self.route_deps(), account_id).await
    }

    pub async fn qwen_accounts_for_route(
        &self,
    ) -> Result<QwenAccountsResponse, ProviderAccountRouteError> {
        operations::qwen_accounts_response(self.route_deps()).await
    }

    pub async fn upsert_qwen_account_for_route(
        &self,
        request: QwenAccountUpsertRouteRequest,
    ) -> Result<QwenAccountsResponse, ProviderAccountRouteError> {
        let (label, oauth_creds_json, email) = request.into_parts();
        operations::add_qwen_account_response(self.route_deps(), label, oauth_creds_json, email)
            .await
    }

    pub async fn set_active_qwen_account_for_route(
        &self,
        request: ProviderActiveAccountRouteRequest,
    ) -> Result<QwenAccountsResponse, ProviderAccountRouteError> {
        operations::set_active_qwen_account_response(self.route_deps(), request.into_account_id())
            .await
    }

    pub async fn delete_qwen_account_for_route(
        &self,
        account_id: &str,
    ) -> Result<QwenAccountsResponse, ProviderAccountRouteError> {
        operations::delete_qwen_account_response(self.route_deps(), account_id).await
    }
}
