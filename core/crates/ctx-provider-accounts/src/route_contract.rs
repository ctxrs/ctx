use serde::{Deserialize, Serialize};

use crate::provider_accounts;

#[derive(Debug, Deserialize)]
pub struct ProviderActiveAccountRouteRequest {
    account_id: Option<String>,
}

impl ProviderActiveAccountRouteRequest {
    pub fn new(account_id: Option<String>) -> Self {
        Self { account_id }
    }

    pub fn account_id(&self) -> Option<&str> {
        self.account_id.as_deref()
    }

    pub fn into_account_id(self) -> Option<String> {
        self.account_id
    }
}

#[derive(Debug, Deserialize)]
pub struct CodexHostImportRouteRequest {
    label: Option<String>,
}

impl CodexHostImportRouteRequest {
    pub fn new(label: Option<String>) -> Self {
        Self { label }
    }

    pub fn into_label(self) -> Option<String> {
        self.label
    }
}

#[derive(Debug, Deserialize)]
pub struct ClaudeAccountUpsertRouteRequest {
    label: Option<String>,
    #[serde(alias = "auth_token")]
    setup_token: String,
}

impl ClaudeAccountUpsertRouteRequest {
    pub fn new(label: Option<String>, setup_token: String) -> Self {
        Self { label, setup_token }
    }

    pub fn into_parts(self) -> (Option<String>, String) {
        (self.label, self.setup_token)
    }
}

#[derive(Debug, Deserialize)]
pub struct GeminiAccountUpsertRouteRequest {
    label: Option<String>,
    oauth_creds_json: String,
    #[serde(default)]
    google_accounts_json: Option<String>,
    #[serde(default)]
    email: Option<String>,
}

impl GeminiAccountUpsertRouteRequest {
    pub fn new(
        label: Option<String>,
        oauth_creds_json: String,
        google_accounts_json: Option<String>,
        email: Option<String>,
    ) -> Self {
        Self {
            label,
            oauth_creds_json,
            google_accounts_json,
            email,
        }
    }

    pub fn into_parts(self) -> (Option<String>, String, Option<String>, Option<String>) {
        (
            self.label,
            self.oauth_creds_json,
            self.google_accounts_json,
            self.email,
        )
    }
}

#[derive(Debug, Deserialize)]
pub struct QwenAccountUpsertRouteRequest {
    label: Option<String>,
    oauth_creds_json: String,
    #[serde(default)]
    email: Option<String>,
}

impl QwenAccountUpsertRouteRequest {
    pub fn new(label: Option<String>, oauth_creds_json: String, email: Option<String>) -> Self {
        Self {
            label,
            oauth_creds_json,
            email,
        }
    }

    pub fn into_parts(self) -> (Option<String>, String, Option<String>) {
        (self.label, self.oauth_creds_json, self.email)
    }
}

#[derive(Debug, Deserialize)]
pub struct AmpAccountUpsertRouteRequest {
    label: Option<String>,
    #[serde(default)]
    email: Option<String>,
}

impl AmpAccountUpsertRouteRequest {
    pub fn new(label: Option<String>, email: Option<String>) -> Self {
        Self { label, email }
    }

    pub fn into_parts(self) -> (Option<String>, Option<String>) {
        (self.label, self.email)
    }
}

#[derive(Debug, Deserialize)]
pub struct MistralAccountUpsertRouteRequest {
    label: Option<String>,
    #[serde(default)]
    email: Option<String>,
}

impl MistralAccountUpsertRouteRequest {
    pub fn new(label: Option<String>, email: Option<String>) -> Self {
        Self { label, email }
    }

    pub fn into_parts(self) -> (Option<String>, Option<String>) {
        (self.label, self.email)
    }
}

#[derive(Debug, Deserialize)]
pub struct KimiAccountUpsertRouteRequest {
    label: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    credentials_json: String,
    #[serde(default)]
    config_toml: Option<String>,
    #[serde(default)]
    email: Option<String>,
}

impl KimiAccountUpsertRouteRequest {
    pub fn new(
        label: Option<String>,
        provider: Option<String>,
        credentials_json: String,
        config_toml: Option<String>,
        email: Option<String>,
    ) -> Self {
        Self {
            label,
            provider,
            credentials_json,
            config_toml,
            email,
        }
    }

    pub fn into_parts(
        self,
    ) -> (
        Option<String>,
        Option<String>,
        String,
        Option<String>,
        Option<String>,
    ) {
        (
            self.label,
            self.provider,
            self.credentials_json,
            self.config_toml,
            self.email,
        )
    }
}

#[derive(Debug, Deserialize)]
pub struct CopilotAccountUpsertRouteRequest {
    label: Option<String>,
    token: String,
    #[serde(default)]
    email: Option<String>,
}

impl CopilotAccountUpsertRouteRequest {
    pub fn new(label: Option<String>, token: String, email: Option<String>) -> Self {
        Self {
            label,
            token,
            email,
        }
    }

    pub fn into_parts(self) -> (Option<String>, String, Option<String>) {
        (self.label, self.token, self.email)
    }
}

#[derive(Debug, Deserialize)]
pub struct CursorAccountUpsertRouteRequest {
    label: Option<String>,
    token: String,
    #[serde(default)]
    email: Option<String>,
}

impl CursorAccountUpsertRouteRequest {
    pub fn new(label: Option<String>, token: String, email: Option<String>) -> Self {
        Self {
            label,
            token,
            email,
        }
    }

    pub fn into_parts(self) -> (Option<String>, String, Option<String>) {
        (self.label, self.token, self.email)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CodexHostImportProbeRouteResponse {
    available: bool,
    path: Option<String>,
    auth_kind: Option<String>,
    error: Option<String>,
}

impl From<provider_accounts::CodexHostImportProbe> for CodexHostImportProbeRouteResponse {
    fn from(probe: provider_accounts::CodexHostImportProbe) -> Self {
        Self {
            available: probe.available,
            path: probe.path,
            auth_kind: probe.auth_kind,
            error: probe.error,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CodexAccountsResponse {
    active_account_id: Option<String>,
    accounts: Vec<provider_accounts::CodexAccountEntry>,
    logins: Vec<provider_accounts::CodexLoginStatus>,
}

impl CodexAccountsResponse {
    pub fn new(
        active_account_id: Option<String>,
        accounts: Vec<provider_accounts::CodexAccountEntry>,
        logins: Vec<provider_accounts::CodexLoginStatus>,
    ) -> Self {
        Self {
            active_account_id,
            accounts,
            logins,
        }
    }
}

macro_rules! account_response {
    ($response:ident, $entry:ty, $registry:ty) => {
        #[derive(Debug, Serialize)]
        pub struct $response {
            active_account_id: Option<String>,
            accounts: Vec<$entry>,
        }

        impl $response {
            pub fn new(active_account_id: Option<String>, accounts: Vec<$entry>) -> Self {
                Self {
                    active_account_id,
                    accounts,
                }
            }
        }

        impl From<$registry> for $response {
            fn from(registry: $registry) -> Self {
                Self::new(registry.active_account_id, registry.accounts)
            }
        }
    };
}

account_response!(
    ClaudeAccountsResponse,
    provider_accounts::ClaudeAccountEntry,
    provider_accounts::ClaudeAccountRegistry
);
account_response!(
    GeminiAccountsResponse,
    provider_accounts::GeminiAccountEntry,
    provider_accounts::GeminiAccountRegistry
);
account_response!(
    QwenAccountsResponse,
    provider_accounts::QwenAccountEntry,
    provider_accounts::QwenAccountRegistry
);
account_response!(
    KimiAccountsResponse,
    provider_accounts::KimiAccountEntry,
    provider_accounts::KimiAccountRegistry
);
account_response!(
    MistralAccountsResponse,
    provider_accounts::MistralAccountEntry,
    provider_accounts::MistralAccountRegistry
);
account_response!(
    CopilotAccountsResponse,
    provider_accounts::CopilotAccountEntry,
    provider_accounts::CopilotAccountRegistry
);
account_response!(
    CursorAccountsResponse,
    provider_accounts::CursorAccountEntry,
    provider_accounts::CursorAccountRegistry
);
account_response!(
    AmpAccountsResponse,
    provider_accounts::AmpAccountEntry,
    provider_accounts::AmpAccountRegistry
);

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ProviderAccountRouteErrorKind {
    BadRequest,
    NotFound,
    Internal,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ProviderAccountRouteError {
    kind: ProviderAccountRouteErrorKind,
    message: String,
}

impl ProviderAccountRouteError {
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            kind: ProviderAccountRouteErrorKind::BadRequest,
            message: message.into(),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            kind: ProviderAccountRouteErrorKind::NotFound,
            message: message.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            kind: ProviderAccountRouteErrorKind::Internal,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> ProviderAccountRouteErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct ProviderLoginStartRouteRequest {
    label: Option<String>,
}

impl ProviderLoginStartRouteRequest {
    pub fn new(label: Option<String>) -> Self {
        Self { label }
    }

    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    pub fn into_label(self) -> Option<String> {
        self.label
    }
}

#[derive(Debug, Serialize)]
pub struct ProviderLoginStartRouteResponse {
    login_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    auth_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    device_code: Option<String>,
}

impl ProviderLoginStartRouteResponse {
    pub fn new(
        login_id: impl Into<String>,
        auth_url: Option<String>,
        device_code: Option<String>,
    ) -> Self {
        Self {
            login_id: login_id.into(),
            auth_url,
            device_code,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct AmpLoginStatusRouteResponse {
    login_id: String,
    #[serde(default)]
    auth_url: Option<String>,
    status: String,
    #[serde(default)]
    error: Option<String>,
}

impl From<provider_accounts::AmpLoginStatus> for AmpLoginStatusRouteResponse {
    fn from(status: provider_accounts::AmpLoginStatus) -> Self {
        Self {
            login_id: status.login_id,
            auth_url: status.auth_url,
            status: status.status,
            error: status.error,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct GeminiLoginStatusRouteResponse {
    login_id: String,
    #[serde(default)]
    auth_url: Option<String>,
    status: String,
    #[serde(default)]
    account_id: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

impl From<provider_accounts::GeminiLoginStatus> for GeminiLoginStatusRouteResponse {
    fn from(status: provider_accounts::GeminiLoginStatus) -> Self {
        Self {
            login_id: status.login_id,
            auth_url: status.auth_url,
            status: status.status,
            account_id: status.account_id,
            error: status.error,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct QwenLoginStatusRouteResponse {
    login_id: String,
    #[serde(default)]
    auth_url: Option<String>,
    status: String,
    #[serde(default)]
    account_id: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

impl From<provider_accounts::QwenLoginStatus> for QwenLoginStatusRouteResponse {
    fn from(status: provider_accounts::QwenLoginStatus) -> Self {
        Self {
            login_id: status.login_id,
            auth_url: status.auth_url,
            status: status.status,
            account_id: status.account_id,
            error: status.error,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct MistralLoginStatusRouteResponse {
    login_id: String,
    #[serde(default)]
    auth_url: Option<String>,
    status: String,
    #[serde(default)]
    error: Option<String>,
}

impl From<provider_accounts::MistralLoginStatus> for MistralLoginStatusRouteResponse {
    fn from(status: provider_accounts::MistralLoginStatus) -> Self {
        Self {
            login_id: status.login_id,
            auth_url: status.auth_url,
            status: status.status,
            error: status.error,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct KimiLoginStatusRouteResponse {
    login_id: String,
    status: String,
    #[serde(default)]
    account_id: Option<String>,
    #[serde(default)]
    auth_url: Option<String>,
    #[serde(default)]
    device_code: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

impl From<provider_accounts::KimiLoginStatus> for KimiLoginStatusRouteResponse {
    fn from(status: provider_accounts::KimiLoginStatus) -> Self {
        Self {
            login_id: status.login_id,
            status: status.status,
            account_id: status.account_id,
            auth_url: status.auth_url,
            device_code: status.device_code,
            error: status.error,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderLoginRouteErrorKind {
    NotFound,
    BadGateway,
}

#[derive(Debug)]
pub struct ProviderLoginRouteError {
    kind: ProviderLoginRouteErrorKind,
    message: String,
}

impl ProviderLoginRouteError {
    pub fn new(kind: ProviderLoginRouteErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ProviderLoginRouteErrorKind::NotFound, message)
    }

    pub fn bad_gateway(message: impl Into<String>) -> Self {
        Self::new(ProviderLoginRouteErrorKind::BadGateway, message)
    }

    pub fn kind(&self) -> ProviderLoginRouteErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct CursorLoginStartRouteRequest {
    label: Option<String>,
}

impl CursorLoginStartRouteRequest {
    pub fn new(label: Option<String>) -> Self {
        Self { label }
    }

    pub fn into_label(self) -> Option<String> {
        self.label
    }
}

#[derive(Debug, Serialize)]
pub struct CursorLoginStartRouteResponse {
    login_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    auth_url: Option<String>,
}

impl CursorLoginStartRouteResponse {
    pub fn new(login_id: impl Into<String>, auth_url: Option<String>) -> Self {
        Self {
            login_id: login_id.into(),
            auth_url,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CursorLoginStatusRouteResponse {
    login_id: String,
    #[serde(default)]
    auth_url: Option<String>,
    status: String,
    #[serde(default)]
    account_id: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

impl From<provider_accounts::CursorLoginStatus> for CursorLoginStatusRouteResponse {
    fn from(status: provider_accounts::CursorLoginStatus) -> Self {
        Self {
            login_id: status.login_id,
            auth_url: status.auth_url,
            status: status.status,
            account_id: status.account_id,
            error: status.error,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorLoginRouteErrorKind {
    BadRequest,
    NotFound,
    Internal,
}

#[derive(Debug)]
pub struct CursorLoginRouteError {
    kind: CursorLoginRouteErrorKind,
    message: String,
}

impl CursorLoginRouteError {
    pub fn new(kind: CursorLoginRouteErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(CursorLoginRouteErrorKind::BadRequest, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(CursorLoginRouteErrorKind::NotFound, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(CursorLoginRouteErrorKind::Internal, message)
    }

    pub fn kind(&self) -> CursorLoginRouteErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct ClaudeLoginStartRouteRequest {
    label: Option<String>,
}

impl ClaudeLoginStartRouteRequest {
    pub fn new(label: Option<String>) -> Self {
        Self { label }
    }

    pub fn into_label(self) -> Option<String> {
        self.label
    }
}

#[derive(Debug, Serialize)]
pub struct ClaudeLoginStartRouteResponse {
    login_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    auth_url: Option<String>,
}

impl ClaudeLoginStartRouteResponse {
    pub fn new(login_id: impl Into<String>, auth_url: Option<String>) -> Self {
        Self {
            login_id: login_id.into(),
            auth_url,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ClaudeLoginStatusRouteResponse {
    login_id: String,
    #[serde(default)]
    auth_url: Option<String>,
    status: String,
    #[serde(default)]
    account_id: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

impl From<provider_accounts::ClaudeLoginStatus> for ClaudeLoginStatusRouteResponse {
    fn from(status: provider_accounts::ClaudeLoginStatus) -> Self {
        Self {
            login_id: status.login_id,
            auth_url: status.auth_url,
            status: status.status,
            account_id: status.account_id,
            error: status.error,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ClaudeLoginRouteErrorKind {
    BadRequest,
    NotFound,
    Internal,
}

#[derive(Debug)]
pub struct ClaudeLoginRouteError {
    kind: ClaudeLoginRouteErrorKind,
    message: String,
}

impl ClaudeLoginRouteError {
    pub fn new(kind: ClaudeLoginRouteErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(ClaudeLoginRouteErrorKind::BadRequest, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ClaudeLoginRouteErrorKind::NotFound, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ClaudeLoginRouteErrorKind::Internal, message)
    }

    pub fn kind(&self) -> ClaudeLoginRouteErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct CodexLoginStartRouteRequest {
    label: Option<String>,
}

impl CodexLoginStartRouteRequest {
    pub fn new(label: Option<String>) -> Self {
        Self { label }
    }

    pub fn into_label(self) -> Option<String> {
        self.label
    }
}

#[derive(Debug, Serialize)]
pub struct CodexLoginStartRouteResponse {
    account_id: String,
    auth_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_callback_url: Option<String>,
    completion_token: String,
}

impl CodexLoginStartRouteResponse {
    pub fn new(
        account_id: impl Into<String>,
        auth_url: impl Into<String>,
        expected_callback_url: Option<String>,
        completion_token: impl Into<String>,
    ) -> Self {
        Self {
            account_id: account_id.into(),
            auth_url: auth_url.into(),
            expected_callback_url,
            completion_token: completion_token.into(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct CodexLoginCompleteRouteRequest {
    callback_url: String,
    completion_token: String,
}

impl CodexLoginCompleteRouteRequest {
    pub fn new(callback_url: impl Into<String>, completion_token: impl Into<String>) -> Self {
        Self {
            callback_url: callback_url.into(),
            completion_token: completion_token.into(),
        }
    }

    pub fn into_parts(self) -> (String, String) {
        (self.callback_url, self.completion_token)
    }
}

#[derive(Debug, Serialize)]
pub struct CodexLoginCompleteRouteResponse {
    accepted: bool,
    status_code: u16,
}

impl CodexLoginCompleteRouteResponse {
    pub fn new(accepted: bool, status_code: u16) -> Self {
        Self {
            accepted,
            status_code,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CodexLoginStatusRouteResponse {
    account_id: String,
    auth_url: String,
    #[serde(default)]
    expected_callback_url: Option<String>,
    #[serde(default)]
    completion_token: Option<String>,
    status: String,
    #[serde(default)]
    error: Option<String>,
}

impl From<provider_accounts::CodexLoginStatus> for CodexLoginStatusRouteResponse {
    fn from(status: provider_accounts::CodexLoginStatus) -> Self {
        Self {
            account_id: status.account_id,
            auth_url: status.auth_url,
            expected_callback_url: status.expected_callback_url,
            completion_token: status.completion_token,
            status: status.status,
            error: status.error,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum CodexLoginRouteErrorKind {
    BadRequest,
    NotFound,
    Conflict,
    Unauthorized,
    BadGateway,
    Internal,
}

#[derive(Debug)]
pub struct CodexLoginRouteError {
    kind: CodexLoginRouteErrorKind,
    message: String,
}

impl CodexLoginRouteError {
    pub fn new(kind: CodexLoginRouteErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(CodexLoginRouteErrorKind::BadRequest, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(CodexLoginRouteErrorKind::NotFound, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(CodexLoginRouteErrorKind::Conflict, message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(CodexLoginRouteErrorKind::Unauthorized, message)
    }

    pub fn bad_gateway(message: impl Into<String>) -> Self {
        Self::new(CodexLoginRouteErrorKind::BadGateway, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(CodexLoginRouteErrorKind::Internal, message)
    }

    pub fn kind(&self) -> CodexLoginRouteErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn claude_upsert_route_request_accepts_auth_token_alias() {
        let request: ClaudeAccountUpsertRouteRequest =
            serde_json::from_value(json!({ "label": "Claude", "auth_token": "token" }))
                .expect("deserialize alias");

        let (label, setup_token) = request.into_parts();
        assert_eq!(label.as_deref(), Some("Claude"));
        assert_eq!(setup_token, "token");
    }

    #[test]
    fn provider_upsert_route_requests_preserve_optional_defaults() {
        let gemini: GeminiAccountUpsertRouteRequest = serde_json::from_value(json!({
            "oauth_creds_json": "{}"
        }))
        .expect("deserialize gemini");
        let (label, _, google_accounts_json, email) = gemini.into_parts();
        assert!(label.is_none());
        assert!(google_accounts_json.is_none());
        assert!(email.is_none());

        let kimi: KimiAccountUpsertRouteRequest = serde_json::from_value(json!({
            "credentials_json": "{}"
        }))
        .expect("deserialize kimi");
        let (label, provider, _, config_toml, email) = kimi.into_parts();
        assert!(label.is_none());
        assert!(provider.is_none());
        assert!(config_toml.is_none());
        assert!(email.is_none());
    }

    #[test]
    fn active_account_request_omitted_and_null_account_ids_clear_active_account() {
        let omitted: ProviderActiveAccountRouteRequest =
            serde_json::from_value(json!({})).expect("deserialize omitted account id");
        let null: ProviderActiveAccountRouteRequest =
            serde_json::from_value(json!({ "account_id": null }))
                .expect("deserialize null account id");

        assert_eq!(omitted.account_id(), None);
        assert_eq!(omitted.into_account_id(), None);
        assert_eq!(null.account_id(), None);
        assert_eq!(null.into_account_id(), None);
    }

    #[test]
    fn codex_host_import_probe_route_response_preserves_wire_shape() {
        let response =
            CodexHostImportProbeRouteResponse::from(provider_accounts::CodexHostImportProbe {
                available: false,
                path: None,
                auth_kind: None,
                error: Some("missing auth".to_string()),
            });

        assert_eq!(
            serde_json::to_value(response).expect("serialize probe"),
            json!({
                "available": false,
                "path": null,
                "auth_kind": null,
                "error": "missing auth",
            })
        );
    }

    #[test]
    fn provider_account_route_error_preserves_kind_and_message() {
        let error = ProviderAccountRouteError::not_found("unknown account");

        assert_eq!(error.kind(), ProviderAccountRouteErrorKind::NotFound);
        assert_eq!(error.message(), "unknown account");
    }

    #[test]
    fn provider_login_start_request_defaults_to_no_label() {
        let request: ProviderLoginStartRouteRequest =
            serde_json::from_value(json!({})).expect("deserialize login start");

        assert_eq!(request.label(), None);
        assert_eq!(request.into_label(), None);
    }

    #[test]
    fn provider_login_start_response_omits_absent_optional_fields() {
        let payload = serde_json::to_value(ProviderLoginStartRouteResponse::new(
            "login-1".to_string(),
            None,
            None,
        ))
        .expect("serialize start response");

        assert_eq!(payload["login_id"].as_str(), Some("login-1"));
        assert!(payload.get("auth_url").is_none());
        assert!(payload.get("device_code").is_none());
    }

    #[test]
    fn provider_login_start_response_preserves_present_optional_fields() {
        let payload = serde_json::to_value(ProviderLoginStartRouteResponse::new(
            "login-2",
            Some("https://example.test/auth".to_string()),
            Some("CODE-123".to_string()),
        ))
        .expect("serialize start response");

        assert_eq!(
            payload["auth_url"].as_str(),
            Some("https://example.test/auth")
        );
        assert_eq!(payload["device_code"].as_str(), Some("CODE-123"));
    }

    #[test]
    fn provider_login_status_route_responses_match_domain_wire_shape() {
        let amp = provider_accounts::AmpLoginStatus {
            login_id: "amp-login".to_string(),
            auth_url: None,
            status: "pending".to_string(),
            error: None,
        };
        assert_eq!(
            serde_json::to_value(AmpLoginStatusRouteResponse::from(amp.clone())).unwrap(),
            serde_json::to_value(amp).unwrap()
        );

        let gemini = provider_accounts::GeminiLoginStatus {
            login_id: "gemini-login".to_string(),
            auth_url: None,
            status: "complete".to_string(),
            account_id: None,
            error: None,
        };
        assert_eq!(
            serde_json::to_value(GeminiLoginStatusRouteResponse::from(gemini.clone())).unwrap(),
            serde_json::to_value(gemini).unwrap()
        );

        let qwen = provider_accounts::QwenLoginStatus {
            login_id: "qwen-login".to_string(),
            auth_url: None,
            status: "error".to_string(),
            account_id: None,
            error: Some("failed".to_string()),
        };
        assert_eq!(
            serde_json::to_value(QwenLoginStatusRouteResponse::from(qwen.clone())).unwrap(),
            serde_json::to_value(qwen).unwrap()
        );

        let mistral = provider_accounts::MistralLoginStatus {
            login_id: "mistral-login".to_string(),
            auth_url: None,
            status: "pending".to_string(),
            error: None,
        };
        assert_eq!(
            serde_json::to_value(MistralLoginStatusRouteResponse::from(mistral.clone())).unwrap(),
            serde_json::to_value(mistral).unwrap()
        );

        let kimi = provider_accounts::KimiLoginStatus {
            login_id: "kimi-login".to_string(),
            status: "pending".to_string(),
            account_id: None,
            auth_url: None,
            device_code: None,
            error: None,
        };
        assert_eq!(
            serde_json::to_value(KimiLoginStatusRouteResponse::from(kimi.clone())).unwrap(),
            serde_json::to_value(kimi).unwrap()
        );
    }

    #[test]
    fn cursor_and_claude_login_start_responses_omit_absent_auth_url() {
        let cursor = serde_json::to_value(CursorLoginStartRouteResponse::new("cursor-login", None))
            .expect("serialize cursor start");
        let claude = serde_json::to_value(ClaudeLoginStartRouteResponse::new("claude-login", None))
            .expect("serialize claude start");

        assert_eq!(cursor["login_id"].as_str(), Some("cursor-login"));
        assert!(cursor.get("auth_url").is_none());
        assert_eq!(claude["login_id"].as_str(), Some("claude-login"));
        assert!(claude.get("auth_url").is_none());
    }

    #[test]
    fn cursor_and_claude_login_status_route_responses_match_domain_wire_shape() {
        let cursor = provider_accounts::CursorLoginStatus {
            login_id: "cursor-login".to_string(),
            auth_url: None,
            status: "pending".to_string(),
            account_id: None,
            error: None,
        };
        assert_eq!(
            serde_json::to_value(CursorLoginStatusRouteResponse::from(cursor.clone())).unwrap(),
            serde_json::to_value(cursor).unwrap()
        );

        let claude = provider_accounts::ClaudeLoginStatus {
            login_id: "claude-login".to_string(),
            auth_url: None,
            status: "pending".to_string(),
            account_id: None,
            error: None,
        };
        assert_eq!(
            serde_json::to_value(ClaudeLoginStatusRouteResponse::from(claude.clone())).unwrap(),
            serde_json::to_value(claude).unwrap()
        );
    }

    #[test]
    fn codex_login_start_response_omits_absent_expected_callback() {
        let payload = serde_json::to_value(CodexLoginStartRouteResponse::new(
            "acct-1",
            "https://example.test/auth",
            None,
            "token-1",
        ))
        .expect("serialize codex start");

        assert_eq!(payload["account_id"].as_str(), Some("acct-1"));
        assert_eq!(
            payload["auth_url"].as_str(),
            Some("https://example.test/auth")
        );
        assert_eq!(payload["completion_token"].as_str(), Some("token-1"));
        assert!(payload.get("expected_callback_url").is_none());
    }

    #[test]
    fn codex_login_complete_contract_preserves_request_and_response_shape() {
        let request: CodexLoginCompleteRouteRequest = serde_json::from_value(json!({
            "callback_url": "http://localhost:1234/auth/callback",
            "completion_token": "token-1",
        }))
        .expect("deserialize complete request");
        let (callback_url, completion_token) = request.into_parts();
        assert_eq!(callback_url, "http://localhost:1234/auth/callback");
        assert_eq!(completion_token, "token-1");

        let payload = serde_json::to_value(CodexLoginCompleteRouteResponse::new(true, 200))
            .expect("serialize complete response");
        assert_eq!(payload["accepted"].as_bool(), Some(true));
        assert_eq!(payload["status_code"].as_u64(), Some(200));
    }

    #[test]
    fn codex_login_status_route_response_matches_domain_wire_shape() {
        let status = provider_accounts::CodexLoginStatus {
            account_id: "codex-account".to_string(),
            auth_url: "https://chat.openai.com/oauth/authorize".to_string(),
            expected_callback_url: None,
            completion_token: None,
            status: "pending".to_string(),
            error: None,
        };

        assert_eq!(
            serde_json::to_value(CodexLoginStatusRouteResponse::from(status.clone())).unwrap(),
            serde_json::to_value(status).unwrap()
        );
    }

    #[test]
    fn login_route_errors_preserve_kind_and_message() {
        let provider = ProviderLoginRouteError::bad_gateway("provider failed");
        assert_eq!(provider.kind(), ProviderLoginRouteErrorKind::BadGateway);
        assert_eq!(provider.message(), "provider failed");

        let cursor = CursorLoginRouteError::internal("cursor failed");
        assert_eq!(cursor.kind(), CursorLoginRouteErrorKind::Internal);
        assert_eq!(cursor.message(), "cursor failed");

        let claude = ClaudeLoginRouteError::bad_request("bad command");
        assert_eq!(claude.kind(), ClaudeLoginRouteErrorKind::BadRequest);
        assert_eq!(claude.message(), "bad command");

        let codex = CodexLoginRouteError::unauthorized("invalid completion token");
        assert_eq!(codex.kind(), CodexLoginRouteErrorKind::Unauthorized);
        assert_eq!(codex.message(), "invalid completion token");
    }
}
