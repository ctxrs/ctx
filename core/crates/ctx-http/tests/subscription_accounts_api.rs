mod common;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex as AsyncMutex};
use url::Url;

use ctx_core::models::SessionEventType;
use ctx_managed_installs::{
    load_agent_server_config, save_agent_server_config, AgentServerCommand, AgentServerConfigFile,
    ProviderLoginExecutable,
};
use ctx_providers::adapters::{
    ProviderAdapter, ProviderHealth, ProviderRestartMode, ProviderStatus, RunHandle, TurnInput,
};
use ctx_providers::events::NormalizedEvent;

const PROVIDER_LOGIN_STATUS_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Deserialize)]
struct SubscriptionAccountEntry {
    id: String,
    label: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SubscriptionAccountsResponse {
    active_account_id: Option<String>,
    accounts: Vec<SubscriptionAccountEntry>,
}

#[derive(Debug, Deserialize)]
struct ErrorResp {
    error: String,
}

#[derive(Debug, Deserialize)]
struct ClaudeLoginStartResponse {
    login_id: String,
    auth_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ClaudeLoginStatusResponse {
    status: String,
    account_id: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GeminiLoginStartResponse {
    login_id: String,
    auth_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GeminiLoginStatusResponse {
    status: String,
    account_id: Option<String>,
    auth_url: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct QwenLoginStartResponse {
    login_id: String,
    auth_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct QwenLoginStatusResponse {
    status: String,
    account_id: Option<String>,
    auth_url: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct KimiLoginStartResponse {
    login_id: String,
    auth_url: Option<String>,
    device_code: Option<String>,
}

#[derive(Debug, Deserialize)]
struct KimiLoginStatusResponse {
    status: String,
    account_id: Option<String>,
    auth_url: Option<String>,
    device_code: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MistralLoginStartResponse {
    login_id: String,
    auth_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MistralLoginStatusResponse {
    status: String,
    auth_url: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AmpLoginStartResponse {
    login_id: String,
    auth_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AmpLoginStatusResponse {
    status: String,
    auth_url: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CursorLoginStartResponse {
    login_id: String,
    auth_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CursorLoginStatusResponse {
    status: String,
    account_id: Option<String>,
    auth_url: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Clone)]
enum GeminiLoginFixture {
    Success {
        oauth_creds_json: String,
        google_accounts_json: Option<String>,
        auth_url: Option<String>,
    },
    Failure {
        error: String,
    },
    NoAuthUrl,
}

#[derive(Debug, Clone)]
struct GeminiLoginTestAdapter {
    fixture: GeminiLoginFixture,
}

impl GeminiLoginTestAdapter {
    fn success(
        oauth_creds_json: impl Into<String>,
        google_accounts_json: Option<String>,
        auth_url: Option<String>,
    ) -> Self {
        Self {
            fixture: GeminiLoginFixture::Success {
                oauth_creds_json: oauth_creds_json.into(),
                google_accounts_json,
                auth_url,
            },
        }
    }

    fn failure(error: impl Into<String>) -> Self {
        Self {
            fixture: GeminiLoginFixture::Failure {
                error: error.into(),
            },
        }
    }

    fn no_auth_url() -> Self {
        Self {
            fixture: GeminiLoginFixture::NoAuthUrl,
        }
    }
}

#[async_trait]
impl ProviderAdapter for GeminiLoginTestAdapter {
    async fn inspect(&self) -> Result<ProviderStatus> {
        Ok(ProviderStatus {
            provider_id: "gemini".to_string(),
            installed: true,
            detected_path: None,
            version: Some("test".to_string()),
            capabilities: None,
            health: ProviderHealth::Ok,
            diagnostics: Vec::new(),
            details: HashMap::new(),
            usability: ctx_providers::adapters::ProviderUsability::default(),
        })
    }

    async fn run(
        &self,
        _input: TurnInput,
        _workdir: PathBuf,
        _env: HashMap<String, String>,
        _event_sink: mpsc::Sender<NormalizedEvent>,
        _hooks: ctx_providers::adapters::ProviderRunHooks,
    ) -> Result<RunHandle> {
        Err(anyhow!("run is not used in this test adapter"))
    }

    async fn cancel(&self, _handle: &mut RunHandle) -> Result<()> {
        Ok(())
    }

    async fn restart(&self, _reason: &str, _mode: ProviderRestartMode) -> Result<()> {
        Ok(())
    }

    async fn authenticate_session(
        &self,
        _session_key: String,
        _workdir: PathBuf,
        env: HashMap<String, String>,
        method_id: Option<String>,
        event_sink: mpsc::Sender<NormalizedEvent>,
        _hooks: ctx_providers::adapters::ProviderRunHooks,
    ) -> Result<()> {
        if method_id.as_deref() != Some("oauth-personal") {
            return Err(anyhow!(
                "unexpected method_id: {:?}",
                method_id.unwrap_or_default()
            ));
        }
        if env.contains_key("CTX_AUTH_TOKEN") {
            return Err(anyhow!("CTX_AUTH_TOKEN should not be set for browser auth"));
        }
        if env.get("CTX_MCP_DISABLED").map(String::as_str) != Some("1") {
            return Err(anyhow!("CTX_MCP_DISABLED=1 should be set for browser auth"));
        }
        let Some(home) = env.get("GEMINI_CLI_HOME") else {
            return Err(anyhow!("GEMINI_CLI_HOME missing"));
        };
        if env
            .get("NO_BROWSER")
            .is_some_and(|value| value.eq_ignore_ascii_case("true"))
        {
            return Err(anyhow!("NO_BROWSER=true should not be set"));
        }
        let gemini_dir = PathBuf::from(home).join(".gemini");
        tokio::fs::create_dir_all(&gemini_dir).await?;

        match &self.fixture {
            GeminiLoginFixture::Success {
                oauth_creds_json,
                google_accounts_json,
                auth_url,
            } => {
                if let Some(auth_url) = auth_url.as_ref() {
                    let _ = event_sink
                        .send(NormalizedEvent {
                            event_type: SessionEventType::Notice,
                            payload_json: json!({ "auth_url": auth_url }),
                        })
                        .await;
                }
                tokio::fs::write(gemini_dir.join("oauth_creds.json"), oauth_creds_json).await?;
                if let Some(google_accounts_json) = google_accounts_json.as_ref() {
                    tokio::fs::write(
                        gemini_dir.join("google_accounts.json"),
                        google_accounts_json,
                    )
                    .await?;
                }
                Ok(())
            }
            GeminiLoginFixture::Failure { error } => {
                let _ = event_sink
                    .send(NormalizedEvent {
                        event_type: SessionEventType::Notice,
                        payload_json: json!({ "kind": "auth_error", "message": error }),
                    })
                    .await;
                Err(anyhow!("{error}"))
            }
            GeminiLoginFixture::NoAuthUrl => {
                // Keep channel alive briefly and emit no auth URL or oauth files.
                tokio::time::sleep(Duration::from_millis(600)).await;
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone)]
enum QwenLoginFixture {
    Success {
        oauth_creds_json: String,
        auth_url: Option<String>,
    },
}

#[derive(Debug, Clone)]
struct QwenLoginTestAdapter {
    fixture: QwenLoginFixture,
}

impl QwenLoginTestAdapter {
    fn success(oauth_creds_json: impl Into<String>, auth_url: Option<String>) -> Self {
        Self {
            fixture: QwenLoginFixture::Success {
                oauth_creds_json: oauth_creds_json.into(),
                auth_url,
            },
        }
    }
}

#[async_trait]
impl ProviderAdapter for QwenLoginTestAdapter {
    async fn inspect(&self) -> Result<ProviderStatus> {
        Ok(ProviderStatus {
            provider_id: "qwen".to_string(),
            installed: true,
            detected_path: None,
            version: Some("test".to_string()),
            capabilities: None,
            health: ProviderHealth::Ok,
            diagnostics: Vec::new(),
            details: HashMap::new(),
            usability: ctx_providers::adapters::ProviderUsability::default(),
        })
    }

    async fn run(
        &self,
        _input: TurnInput,
        _workdir: PathBuf,
        _env: HashMap<String, String>,
        _event_sink: mpsc::Sender<NormalizedEvent>,
        _hooks: ctx_providers::adapters::ProviderRunHooks,
    ) -> Result<RunHandle> {
        Err(anyhow!("run is not used in this test adapter"))
    }

    async fn cancel(&self, _handle: &mut RunHandle) -> Result<()> {
        Ok(())
    }

    async fn restart(&self, _reason: &str, _mode: ProviderRestartMode) -> Result<()> {
        Ok(())
    }

    async fn authenticate_session(
        &self,
        _session_key: String,
        _workdir: PathBuf,
        env: HashMap<String, String>,
        method_id: Option<String>,
        event_sink: mpsc::Sender<NormalizedEvent>,
        _hooks: ctx_providers::adapters::ProviderRunHooks,
    ) -> Result<()> {
        if method_id.as_deref() != Some("qwen-oauth") {
            return Err(anyhow!(
                "unexpected method_id: {:?}",
                method_id.unwrap_or_default()
            ));
        }
        if env.contains_key("CTX_AUTH_TOKEN") {
            return Err(anyhow!("CTX_AUTH_TOKEN should not be set for browser auth"));
        }
        if env.get("CTX_MCP_DISABLED").map(String::as_str) != Some("1") {
            return Err(anyhow!("CTX_MCP_DISABLED=1 should be set for browser auth"));
        }
        let Some(home) = env.get("HOME") else {
            return Err(anyhow!("HOME missing"));
        };
        let qwen_dir = PathBuf::from(home).join(".qwen");
        tokio::fs::create_dir_all(&qwen_dir).await?;

        match &self.fixture {
            QwenLoginFixture::Success {
                oauth_creds_json,
                auth_url,
            } => {
                if let Some(auth_url) = auth_url.as_ref() {
                    let _ = event_sink
                        .send(NormalizedEvent {
                            event_type: SessionEventType::Notice,
                            payload_json: json!({ "auth_url": auth_url }),
                        })
                        .await;
                }
                tokio::fs::write(qwen_dir.join("oauth_creds.json"), oauth_creds_json).await?;
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone)]
enum MistralLoginFixture {
    Success {
        auth_url: Option<String>,
        email: Option<String>,
    },
}

#[derive(Debug, Clone)]
struct MistralLoginTestAdapter {
    fixture: MistralLoginFixture,
}

impl MistralLoginTestAdapter {
    fn success(auth_url: Option<String>, email: Option<String>) -> Self {
        Self {
            fixture: MistralLoginFixture::Success { auth_url, email },
        }
    }
}

#[async_trait]
impl ProviderAdapter for MistralLoginTestAdapter {
    async fn inspect(&self) -> Result<ProviderStatus> {
        Ok(ProviderStatus {
            provider_id: "mistral".to_string(),
            installed: true,
            detected_path: None,
            version: Some("test".to_string()),
            capabilities: None,
            health: ProviderHealth::Ok,
            diagnostics: Vec::new(),
            details: HashMap::new(),
            usability: ctx_providers::adapters::ProviderUsability::default(),
        })
    }

    async fn run(
        &self,
        _input: TurnInput,
        _workdir: PathBuf,
        _env: HashMap<String, String>,
        _event_sink: mpsc::Sender<NormalizedEvent>,
        _hooks: ctx_providers::adapters::ProviderRunHooks,
    ) -> Result<RunHandle> {
        Err(anyhow!("run is not used in this test adapter"))
    }

    async fn cancel(&self, _handle: &mut RunHandle) -> Result<()> {
        Ok(())
    }

    async fn restart(&self, _reason: &str, _mode: ProviderRestartMode) -> Result<()> {
        Ok(())
    }

    async fn authenticate_session(
        &self,
        _session_key: String,
        _workdir: PathBuf,
        env: HashMap<String, String>,
        _method_id: Option<String>,
        event_sink: mpsc::Sender<NormalizedEvent>,
        _hooks: ctx_providers::adapters::ProviderRunHooks,
    ) -> Result<()> {
        if env.contains_key("CTX_AUTH_TOKEN") {
            return Err(anyhow!("CTX_AUTH_TOKEN should not be set for browser auth"));
        }
        if env.get("CTX_MCP_DISABLED").map(String::as_str) != Some("1") {
            return Err(anyhow!("CTX_MCP_DISABLED=1 should be set for browser auth"));
        }
        if !env.contains_key("HOME") {
            return Err(anyhow!("HOME missing"));
        }
        match &self.fixture {
            MistralLoginFixture::Success { auth_url, email } => {
                if let Some(auth_url) = auth_url.as_ref() {
                    let _ = event_sink
                        .send(NormalizedEvent {
                            event_type: SessionEventType::Notice,
                            payload_json: json!({ "auth_url": auth_url }),
                        })
                        .await;
                }
                let _ = event_sink
                    .send(NormalizedEvent {
                        event_type: SessionEventType::Notice,
                        payload_json: json!({
                            "code": "auth_complete",
                            "email": email,
                        }),
                    })
                    .await;
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone)]
enum AmpLoginFixture {
    AuthRequired {
        auth_url: Option<String>,
        message: String,
    },
}

#[derive(Debug, Clone)]
struct AmpLoginTestAdapter {
    fixture: AmpLoginFixture,
}

impl AmpLoginTestAdapter {
    fn auth_required(auth_url: Option<String>, message: impl Into<String>) -> Self {
        Self {
            fixture: AmpLoginFixture::AuthRequired {
                auth_url,
                message: message.into(),
            },
        }
    }
}

#[async_trait]
impl ProviderAdapter for AmpLoginTestAdapter {
    async fn inspect(&self) -> Result<ProviderStatus> {
        Ok(ProviderStatus {
            provider_id: "amp".to_string(),
            installed: true,
            detected_path: None,
            version: Some("test".to_string()),
            capabilities: None,
            health: ProviderHealth::Ok,
            diagnostics: Vec::new(),
            details: HashMap::new(),
            usability: ctx_providers::adapters::ProviderUsability::default(),
        })
    }

    async fn run(
        &self,
        _input: TurnInput,
        _workdir: PathBuf,
        _env: HashMap<String, String>,
        _event_sink: mpsc::Sender<NormalizedEvent>,
        _hooks: ctx_providers::adapters::ProviderRunHooks,
    ) -> Result<RunHandle> {
        Err(anyhow!("run is not used in this test adapter"))
    }

    async fn cancel(&self, _handle: &mut RunHandle) -> Result<()> {
        Ok(())
    }

    async fn restart(&self, _reason: &str, _mode: ProviderRestartMode) -> Result<()> {
        Ok(())
    }

    async fn authenticate_session(
        &self,
        _session_key: String,
        _workdir: PathBuf,
        env: HashMap<String, String>,
        method_id: Option<String>,
        event_sink: mpsc::Sender<NormalizedEvent>,
        _hooks: ctx_providers::adapters::ProviderRunHooks,
    ) -> Result<()> {
        if method_id.as_deref() != Some("amp_browser_login") {
            return Err(anyhow!(
                "unexpected method_id: {:?}",
                method_id.unwrap_or_default()
            ));
        }
        if env.contains_key("CTX_AUTH_TOKEN") {
            return Err(anyhow!("CTX_AUTH_TOKEN should not be set for browser auth"));
        }
        if env.get("CTX_MCP_DISABLED").map(String::as_str) != Some("1") {
            return Err(anyhow!("CTX_MCP_DISABLED=1 should be set for browser auth"));
        }
        if !env.contains_key("HOME") {
            return Err(anyhow!("HOME missing"));
        }

        match &self.fixture {
            AmpLoginFixture::AuthRequired { auth_url, message } => {
                if let Some(auth_url) = auth_url.as_ref() {
                    let _ = event_sink
                        .send(NormalizedEvent {
                            event_type: SessionEventType::Notice,
                            payload_json: json!({ "auth_url": auth_url }),
                        })
                        .await;
                }
                let _ = event_sink
                    .send(NormalizedEvent {
                        event_type: SessionEventType::Notice,
                        payload_json: json!({
                            "code": "auth_required",
                            "message": message,
                        }),
                    })
                    .await;
                Ok(())
            }
        }
    }
}

fn providers_with_gemini_adapter(
    adapter: Arc<dyn ProviderAdapter>,
) -> HashMap<String, Arc<dyn ProviderAdapter>> {
    let mut providers = common::fake_providers();
    providers.insert("gemini".to_string(), adapter);
    providers
}

fn providers_with_qwen_adapter(
    adapter: Arc<dyn ProviderAdapter>,
) -> HashMap<String, Arc<dyn ProviderAdapter>> {
    let mut providers = common::fake_providers();
    providers.insert("qwen".to_string(), adapter);
    providers
}

fn providers_with_mistral_adapter(
    adapter: Arc<dyn ProviderAdapter>,
) -> HashMap<String, Arc<dyn ProviderAdapter>> {
    let mut providers = common::fake_providers();
    providers.insert("mistral".to_string(), adapter);
    providers
}

fn providers_with_amp_adapter(
    adapter: Arc<dyn ProviderAdapter>,
) -> HashMap<String, Arc<dyn ProviderAdapter>> {
    let mut providers = common::fake_providers();
    providers.insert("amp".to_string(), adapter);
    providers
}

fn providers_with_kimi_adapter(
    adapter: Arc<dyn ProviderAdapter>,
) -> HashMap<String, Arc<dyn ProviderAdapter>> {
    let mut providers = common::fake_providers();
    providers.insert("kimi".to_string(), adapter);
    providers
}

async fn assert_managed_subscription_crud(provider_id: &str, upsert_body: serde_json::Value) {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let server = fixture.spawn_server().await;

    let accounts_url = format!("{}/api/providers/{provider_id}/accounts", server.base_url);
    let active_url = format!(
        "{}/api/providers/{provider_id}/active-account",
        server.base_url
    );

    let created = server
        .client
        .post(&accounts_url)
        .json(&upsert_body)
        .send()
        .await
        .expect("create account request");
    assert_eq!(created.status(), StatusCode::OK);
    let created_body: SubscriptionAccountsResponse =
        created.json().await.expect("created response json");
    assert_eq!(created_body.accounts.len(), 1);
    let account_id = created_body.accounts[0].id.clone();
    assert_eq!(
        created_body.active_account_id.as_deref(),
        Some(account_id.as_str())
    );

    let listed = server
        .client
        .get(&accounts_url)
        .send()
        .await
        .expect("list accounts request");
    assert_eq!(listed.status(), StatusCode::OK);
    let listed_body: SubscriptionAccountsResponse =
        listed.json().await.expect("list response json");
    assert_eq!(listed_body.accounts.len(), 1);
    assert_eq!(listed_body.accounts[0].id, account_id);

    let missing = server
        .client
        .put(&active_url)
        .json(&json!({ "account_id": "missing-account" }))
        .send()
        .await
        .expect("set active missing request");
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    let activated = server
        .client
        .put(&active_url)
        .json(&json!({ "account_id": account_id }))
        .send()
        .await
        .expect("set active request");
    assert_eq!(activated.status(), StatusCode::OK);
    let activated_body: SubscriptionAccountsResponse =
        activated.json().await.expect("set active response json");
    assert_eq!(
        activated_body.active_account_id.as_deref(),
        Some(activated_body.accounts[0].id.as_str())
    );

    let missing_delete = server
        .client
        .delete(format!("{accounts_url}/missing-account"))
        .send()
        .await
        .expect("delete missing account request");
    assert_eq!(missing_delete.status(), StatusCode::NOT_FOUND);

    let deleted = server
        .client
        .delete(format!("{accounts_url}/{}", activated_body.accounts[0].id))
        .send()
        .await
        .expect("delete account request");
    assert_eq!(deleted.status(), StatusCode::OK);
    let deleted_body: SubscriptionAccountsResponse =
        deleted.json().await.expect("delete response json");
    assert!(deleted_body.accounts.is_empty());
    assert!(deleted_body.active_account_id.is_none());
}

async fn write_mock_claude_runtime(
    data_root: &std::path::Path,
    script_contents: &str,
) -> std::path::PathBuf {
    let script_path = data_root.join("mock-claude");
    tokio::fs::write(&script_path, script_contents)
        .await
        .expect("write mock claude script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script_path)
            .expect("mock claude metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms).expect("set mock claude permissions");
    }
    script_path
}

async fn write_mock_cursor_runtime(
    data_root: &std::path::Path,
    script_contents: &str,
) -> std::path::PathBuf {
    let script_path = data_root.join("cursor-agent");
    tokio::fs::write(&script_path, script_contents)
        .await
        .expect("write mock cursor script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script_path)
            .expect("mock cursor metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms).expect("set mock cursor permissions");
    }
    script_path
}

static CLAUDE_TOKEN_ENV_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());
static KIMI_TOKEN_ENV_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());

struct TestEnvVar {
    key: &'static str,
    prev: Option<String>,
}

impl TestEnvVar {
    fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var(key).ok();
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, prev }
    }
}

impl Drop for TestEnvVar {
    fn drop(&mut self) {
        match self.prev.as_deref() {
            Some(value) => unsafe {
                std::env::set_var(self.key, value);
            },
            None => unsafe {
                std::env::remove_var(self.key);
            },
        }
    }
}

#[derive(Debug, Deserialize)]
struct KimiDeviceAuthorizationRequest {
    client_id: String,
}

#[derive(Debug, Clone, Deserialize)]
struct KimiTokenPollRequest {
    client_id: String,
    device_code: String,
    grant_type: String,
}

#[derive(Debug, Clone)]
enum KimiOAuthScenario {
    Success,
    DeviceAuthorizationFailure,
    PendingUntilTimeout,
    ExpiredToken,
    AccessDenied,
    UnknownClientError,
    TokenServerError,
    MalformedTokenSuccess,
}

impl KimiOAuthScenario {
    fn device_authorization_response(&self) -> Response {
        if matches!(self, Self::DeviceAuthorizationFailure) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "device authorization failed access_token=startup-secret",
            )
                .into_response();
        }

        let expires_in = if matches!(self, Self::PendingUntilTimeout) {
            1
        } else {
            30
        };
        axum::Json(json!({
            "user_code": "ABCD-1234",
            "device_code": "device-code-1",
            "verification_uri": "http://127.0.0.1/verify",
            "verification_uri_complete": "http://127.0.0.1/verify?user_code=ABCD-1234",
            "expires_in": expires_in,
            "interval": 1,
        }))
        .into_response()
    }

    fn token_response(&self, attempt: usize) -> Response {
        match self {
            Self::Success => {
                if attempt == 1 {
                    return (
                        StatusCode::BAD_REQUEST,
                        axum::Json(json!({
                            "error": "authorization_pending",
                            "error_description": "Waiting for Kimi sign-in",
                        })),
                    )
                        .into_response();
                }
                axum::Json(json!({
                    "access_token": "kimi-access",
                    "refresh_token": "kimi-refresh",
                    "expires_in": 3600,
                    "scope": "openid profile email",
                    "token_type": "Bearer",
                }))
                .into_response()
            }
            Self::DeviceAuthorizationFailure => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "token endpoint should not be reached",
            )
                .into_response(),
            Self::PendingUntilTimeout => (
                StatusCode::BAD_REQUEST,
                axum::Json(json!({
                    "error": "authorization_pending",
                    "error_description": "Waiting for Kimi sign-in",
                })),
            )
                .into_response(),
            Self::ExpiredToken => (
                StatusCode::BAD_REQUEST,
                axum::Json(json!({
                    "error": "expired_token",
                    "error_description": "Device code expired",
                })),
            )
                .into_response(),
            Self::AccessDenied => (
                StatusCode::BAD_REQUEST,
                axum::Json(json!({
                    "error": "access_denied",
                    "error_description": "User denied Kimi sign-in.",
                })),
            )
                .into_response(),
            Self::UnknownClientError => (
                StatusCode::BAD_REQUEST,
                axum::Json(json!({
                    "error": "strange_oauth_error",
                    "error_description": "Kimi OAuth returned an unknown error",
                })),
            )
                .into_response(),
            Self::TokenServerError => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "token endpoint failed access_token=token-secret",
            )
                .into_response(),
            Self::MalformedTokenSuccess => (
                StatusCode::OK,
                r#"{"access_token":"kimi-access","refresh_token":"kimi-refresh"}"#,
            )
                .into_response(),
        }
    }
}

async fn start_kimi_oauth_server() -> (common::TestServer, Arc<Mutex<Vec<KimiTokenPollRequest>>>) {
    start_kimi_oauth_server_with_scenario(KimiOAuthScenario::Success).await
}

async fn start_kimi_oauth_server_with_scenario(
    scenario: KimiOAuthScenario,
) -> (common::TestServer, Arc<Mutex<Vec<KimiTokenPollRequest>>>) {
    let polls = Arc::new(Mutex::new(Vec::<KimiTokenPollRequest>::new()));
    let polls_for_token = Arc::clone(&polls);
    let scenario_for_auth = scenario.clone();
    let scenario_for_token = scenario;
    let app = axum::Router::new()
        .route(
            "/api/oauth/device_authorization",
            axum::routing::post(
                move |axum::Form(payload): axum::Form<KimiDeviceAuthorizationRequest>| {
                    let scenario = scenario_for_auth.clone();
                    async move {
                        assert_eq!(payload.client_id, "17e5f671-d194-4dfb-9706-5516cb48c098");
                        scenario.device_authorization_response()
                    }
                },
            ),
        )
        .route(
            "/api/oauth/token",
            axum::routing::post(
                move |axum::Form(payload): axum::Form<KimiTokenPollRequest>| {
                    let polls = Arc::clone(&polls_for_token);
                    let scenario = scenario_for_token.clone();
                    async move {
                        assert_eq!(payload.client_id, "17e5f671-d194-4dfb-9706-5516cb48c098");
                        assert_eq!(
                            payload.grant_type,
                            "urn:ietf:params:oauth:grant-type:device_code"
                        );
                        assert_eq!(payload.device_code, "device-code-1");
                        let mut recorded = polls.lock().expect("poll mutex");
                        recorded.push(payload);
                        let attempt = recorded.len();
                        drop(recorded);
                        scenario.token_response(attempt)
                    }
                },
            ),
        );
    let server = common::spawn_http_server(app).await;
    (server, polls)
}

#[derive(Debug)]
struct KimiRestartFailureAdapter;

#[async_trait]
impl ProviderAdapter for KimiRestartFailureAdapter {
    async fn inspect(&self) -> Result<ProviderStatus> {
        Ok(ProviderStatus {
            provider_id: "kimi".to_string(),
            installed: true,
            detected_path: None,
            version: Some("test".to_string()),
            capabilities: None,
            health: ProviderHealth::Ok,
            diagnostics: Vec::new(),
            details: HashMap::new(),
            usability: ctx_providers::adapters::ProviderUsability::default(),
        })
    }

    async fn run(
        &self,
        _input: TurnInput,
        _workdir: PathBuf,
        _env: HashMap<String, String>,
        _event_sink: mpsc::Sender<NormalizedEvent>,
        _hooks: ctx_providers::adapters::ProviderRunHooks,
    ) -> Result<RunHandle> {
        Err(anyhow!("run is not used in this test adapter"))
    }

    async fn cancel(&self, _handle: &mut RunHandle) -> Result<()> {
        Ok(())
    }

    async fn restart(&self, _reason: &str, _mode: ProviderRestartMode) -> Result<()> {
        Err(anyhow!("forced kimi restart failure"))
    }

    fn supports_restart_mode(&self, mode: ProviderRestartMode) -> bool {
        matches!(mode, ProviderRestartMode::Drain)
    }
}

fn parse_claude_auth_url(start_body: &ClaudeLoginStartResponse) -> Url {
    Url::parse(
        start_body
            .auth_url
            .as_deref()
            .expect("claude login auth url should be present"),
    )
    .expect("claude auth url")
}

async fn poll_claude_login_status(
    server: &common::TestServer,
    login_id: &str,
    timeout: Duration,
) -> ClaudeLoginStatusResponse {
    let status_url = format!(
        "{}/api/providers/claude-crp/accounts/login/{}",
        server.base_url, login_id
    );
    let deadline = Instant::now() + timeout;
    loop {
        let resp = server
            .client
            .get(&status_url)
            .send()
            .await
            .expect("claude login status request");
        assert_eq!(resp.status(), StatusCode::OK);
        let body: ClaudeLoginStatusResponse = resp.json().await.expect("claude login status body");
        if body.status != "pending" {
            return body;
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("claude login did not reach terminal status in time");
}

fn empty_agent_server_config() -> AgentServerConfigFile {
    AgentServerConfigFile {
        providers: HashMap::new(),
        ..AgentServerConfigFile::default()
    }
}

fn set_login_executable(
    cfg: &mut AgentServerConfigFile,
    provider_id: &str,
    executable_path: &Path,
) {
    cfg.provider_login_executables.insert(
        provider_id.to_string(),
        ProviderLoginExecutable {
            executable_path: executable_path.to_string_lossy().to_string(),
        },
    );
}

async fn poll_gemini_login_status(
    server: &common::TestServer,
    login_id: &str,
) -> GeminiLoginStatusResponse {
    let status_url = format!(
        "{}/api/providers/gemini/accounts/login/{}",
        server.base_url, login_id
    );
    let deadline = Instant::now() + PROVIDER_LOGIN_STATUS_TIMEOUT;
    loop {
        let resp = server
            .client
            .get(&status_url)
            .send()
            .await
            .expect("gemini status request");
        assert_eq!(resp.status(), StatusCode::OK);
        let body: GeminiLoginStatusResponse = resp.json().await.expect("gemini status body");
        if body.status != "pending" {
            return body;
        }
        if Instant::now() >= deadline {
            panic!("gemini login did not complete in time");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn poll_qwen_login_status(
    server: &common::TestServer,
    login_id: &str,
) -> QwenLoginStatusResponse {
    let status_url = format!(
        "{}/api/providers/qwen/accounts/login/{}",
        server.base_url, login_id
    );
    let deadline = Instant::now() + PROVIDER_LOGIN_STATUS_TIMEOUT;
    loop {
        let resp = server
            .client
            .get(&status_url)
            .send()
            .await
            .expect("qwen status request");
        assert_eq!(resp.status(), StatusCode::OK);
        let body: QwenLoginStatusResponse = resp.json().await.expect("qwen status body");
        if body.status != "pending" {
            return body;
        }
        if Instant::now() >= deadline {
            panic!("qwen login did not complete in time");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn poll_kimi_login_status(
    server: &common::TestServer,
    login_id: &str,
) -> KimiLoginStatusResponse {
    let status_url = format!(
        "{}/api/providers/kimi/accounts/login/{}",
        server.base_url, login_id
    );
    let deadline = Instant::now() + PROVIDER_LOGIN_STATUS_TIMEOUT;
    loop {
        let resp = server
            .client
            .get(&status_url)
            .send()
            .await
            .expect("kimi status request");
        assert_eq!(resp.status(), StatusCode::OK);
        let body: KimiLoginStatusResponse = resp.json().await.expect("kimi status body");
        if body.status != "pending" {
            return body;
        }
        if Instant::now() >= deadline {
            panic!("kimi login did not complete in time");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn run_kimi_login_scenario(
    scenario: KimiOAuthScenario,
    providers: HashMap<String, Arc<dyn ProviderAdapter>>,
) -> (
    KimiLoginStartResponse,
    KimiLoginStatusResponse,
    Arc<Mutex<Vec<KimiTokenPollRequest>>>,
) {
    let _env_lock = KIMI_TOKEN_ENV_LOCK.lock().await;
    let oauth_server = start_kimi_oauth_server_with_scenario(scenario).await;
    let _oauth_host = TestEnvVar::set("KIMI_CODE_OAUTH_HOST", oauth_server.0.base_url.as_str());
    let _timeout = TestEnvVar::set("CTX_KIMI_LOGIN_TIMEOUT_SECS", "5");

    let fixture = common::fake_daemon_fixture_with_providers(providers, "http://127.0.0.1:0").await;
    let server = fixture.spawn_server().await;

    let start_url = format!(
        "{}/api/providers/kimi/accounts/login/start",
        server.base_url
    );
    let start_resp = server
        .client
        .post(start_url)
        .json(&json!({ "label": "Kimi Google" }))
        .send()
        .await
        .expect("start kimi login request");
    assert_eq!(start_resp.status(), StatusCode::OK);
    let start_body: KimiLoginStartResponse = start_resp.json().await.expect("start body");
    let status = poll_kimi_login_status(&server, &start_body.login_id).await;

    (start_body, status, oauth_server.1)
}

async fn poll_mistral_login_status(
    server: &common::TestServer,
    login_id: &str,
) -> MistralLoginStatusResponse {
    let status_url = format!(
        "{}/api/providers/mistral/accounts/login/{}",
        server.base_url, login_id
    );
    let deadline = Instant::now() + PROVIDER_LOGIN_STATUS_TIMEOUT;
    loop {
        let resp = server
            .client
            .get(&status_url)
            .send()
            .await
            .expect("mistral status request");
        assert_eq!(resp.status(), StatusCode::OK);
        let body: MistralLoginStatusResponse = resp.json().await.expect("mistral status body");
        if body.status != "pending" {
            return body;
        }
        if Instant::now() >= deadline {
            panic!("mistral login did not complete in time");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn poll_amp_login_status(
    server: &common::TestServer,
    login_id: &str,
) -> AmpLoginStatusResponse {
    let status_url = format!(
        "{}/api/providers/amp/accounts/login/{}",
        server.base_url, login_id
    );
    let deadline = Instant::now() + PROVIDER_LOGIN_STATUS_TIMEOUT;
    loop {
        let resp = server
            .client
            .get(&status_url)
            .send()
            .await
            .expect("amp status request");
        assert_eq!(resp.status(), StatusCode::OK);
        let body: AmpLoginStatusResponse = resp.json().await.expect("amp status body");
        if body.status != "pending" {
            return body;
        }
        if Instant::now() >= deadline {
            panic!("amp login did not complete in time");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn poll_cursor_login_status(
    server: &common::TestServer,
    login_id: &str,
) -> CursorLoginStatusResponse {
    let status_url = format!(
        "{}/api/providers/cursor/accounts/login/{}",
        server.base_url, login_id
    );
    let deadline = Instant::now() + PROVIDER_LOGIN_STATUS_TIMEOUT;
    loop {
        let resp = server
            .client
            .get(&status_url)
            .send()
            .await
            .expect("cursor status request");
        assert_eq!(resp.status(), StatusCode::OK);
        let body: CursorLoginStatusResponse = resp.json().await.expect("cursor status body");
        if body.status != "pending" {
            return body;
        }
        if Instant::now() >= deadline {
            panic!("cursor login did not complete in time");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn claude_subscription_accounts_crud_round_trip() {
    assert_managed_subscription_crud(
        "claude-crp",
        json!({
            "label": "Claude Team",
            "setup_token": "sk-ant-oat01-abcDEF1234567890_abcdefghijklmnopqrstuvwxyz_0123456789"
        }),
    )
    .await;
}

#[tokio::test]
async fn claude_login_setup_token_path_succeeds_when_cli_invokes_browser_shim() {
    let _env_lock = CLAUDE_TOKEN_ENV_LOCK.lock().await;
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let data_dir = fixture.data_dir.path();
    let server = fixture.spawn_server().await;

    let fake_open_dir = data_dir.join("fake-open-bin");
    std::fs::create_dir_all(&fake_open_dir).expect("create fake open dir");
    let opened_url_path = data_dir.join("opened-url.txt");
    let fake_open_path = fake_open_dir.join("open");
    std::fs::write(
        &fake_open_path,
        format!(
            "#!/usr/bin/env bash\nset -euo pipefail\nprintf '%s\\n' \"$1\" > \"{}\"\n",
            opened_url_path.display()
        ),
    )
    .expect("write fake open");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&fake_open_path)
            .expect("fake open metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&fake_open_path, perms).expect("chmod fake open");
    }

    let script_path = write_mock_claude_runtime(
        data_dir,
        &format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
export PATH="{fake_open_dir}:$PATH"
"$BROWSER" "https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A64111%2Fcallback&state=test"
echo "Long-lived authentication token created successfully!"
echo ""
echo "Your OAuth token (valid for 1 year):"
echo ""
echo "sk-ant-oat01-abcDEF1234567890_"
echo "ZXY987654321"
"#,
            fake_open_dir = fake_open_dir.display(),
        ),
    )
    .await;
    let mut cfg = empty_agent_server_config();
    cfg.providers.insert(
        "claude-cli".to_string(),
        AgentServerCommand {
            command: script_path.to_string_lossy().to_string(),
            args: vec![],
            dependencies: vec![],
            managed: None,
        },
    );
    save_agent_server_config(data_dir, &cfg)
        .await
        .expect("save agent config");

    let start_url = format!(
        "{}/api/providers/claude-crp/accounts/login/start",
        server.base_url
    );
    let start_resp = server
        .client
        .post(&start_url)
        .json(&json!({}))
        .send()
        .await
        .expect("start claude login request");
    assert_eq!(start_resp.status(), StatusCode::OK);
    let start_body: ClaudeLoginStartResponse = start_resp.json().await.expect("start body");
    assert_eq!(
        start_body.auth_url.as_deref(),
        Some("https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A64111%2Fcallback&state=test")
    );

    let status =
        poll_claude_login_status(&server, &start_body.login_id, Duration::from_secs(30)).await;
    assert_eq!(status.status, "success");
    assert!(status.account_id.is_some());
    assert!(status.error.is_none());
    assert_eq!(
        std::fs::read_to_string(&opened_url_path).expect("read opened url"),
        "https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A64111%2Fcallback&state=test\n"
    );
    // Keep the mock runtime and fake browser tempdir alive until the spawned
    // setup-token process has completed.
    assert!(data_dir.exists());
}

#[tokio::test]
async fn claude_login_start_requires_managed_or_configured_runtime_command() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let server = fixture.spawn_server().await;

    let start_url = format!(
        "{}/api/providers/claude-crp/accounts/login/start",
        server.base_url
    );
    let start_resp = server
        .client
        .post(start_url)
        .json(&json!({ "label": "Claude setup-token" }))
        .send()
        .await
        .expect("start claude login request");
    assert_eq!(start_resp.status(), StatusCode::BAD_REQUEST);
    let body: ErrorResp = start_resp.json().await.expect("start error body");
    assert!(body
        .error
        .contains("runtime_command_missing: provider=claude-cli"));
    assert!(body.error.contains("host PATH lookup is not supported"));
}

#[tokio::test]
async fn claude_login_start_rejects_manual_copy_code_fallback_without_browser_open_capture() {
    let _env_lock = CLAUDE_TOKEN_ENV_LOCK.lock().await;
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let data_dir = fixture.data_dir.path();
    let server = fixture.spawn_server().await;
    let script_path = write_mock_claude_runtime(
        data_dir,
        r#"#!/usr/bin/env bash
set -euo pipefail
echo "Browser didn't open? Use the URL below to sign in"
echo "https://claude.ai/oauth/authorize?redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback&state=bad"
"#,
    )
    .await;
    let mut cfg = empty_agent_server_config();
    cfg.providers.insert(
        "claude-cli".to_string(),
        AgentServerCommand {
            command: script_path.to_string_lossy().to_string(),
            args: vec![],
            dependencies: vec![],
            managed: None,
        },
    );
    save_agent_server_config(data_dir, &cfg)
        .await
        .expect("save agent config");

    let start_url = format!(
        "{}/api/providers/claude-crp/accounts/login/start",
        server.base_url
    );
    let start_resp = server
        .client
        .post(&start_url)
        .json(&json!({}))
        .send()
        .await
        .expect("start claude login request");
    assert_eq!(start_resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let error_body: serde_json::Value = start_resp.json().await.expect("error body");
    assert!(error_body["error"]
        .as_str()
        .unwrap_or_default()
        .contains("fell back to manual code entry"));
    // Keep the mock runtime tempdir alive through the async start request.
    assert!(data_dir.exists());
}

#[tokio::test]
async fn claude_login_start_ignores_manual_copy_code_fallback_after_browser_open_capture() {
    let _env_lock = CLAUDE_TOKEN_ENV_LOCK.lock().await;
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let data_dir = fixture.data_dir.path();
    let server = fixture.spawn_server().await;

    let fake_open_dir = data_dir.join("fake-open-bin");
    std::fs::create_dir_all(&fake_open_dir).expect("create fake open dir");
    let fake_open_path = fake_open_dir.join("open");
    std::fs::write(
        &fake_open_path,
        "#!/usr/bin/env bash\nset -euo pipefail\nexit 0\n",
    )
    .expect("write fake open");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&fake_open_path)
            .expect("fake open metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&fake_open_path, perms).expect("chmod fake open");
    }

    let script_path = write_mock_claude_runtime(
        data_dir,
        &format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
export PATH="{fake_open_dir}:$PATH"
"$BROWSER" "https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A64111%2Fcallback&state=test"
echo "Browser didn't open? Use the URL below to sign in"
echo "https://claude.ai/oauth/authorize?redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback&state=bad"
echo "Long-lived authentication token created successfully!"
echo ""
echo "Your OAuth token (valid for 1 year):"
echo ""
echo "sk-ant-oat01-abcDEF1234567890_"
echo "ZXY987654321"
"#,
            fake_open_dir = fake_open_dir.display(),
        ),
    )
    .await;
    let mut cfg = empty_agent_server_config();
    cfg.providers.insert(
        "claude-cli".to_string(),
        AgentServerCommand {
            command: script_path.to_string_lossy().to_string(),
            args: vec![],
            dependencies: vec![],
            managed: None,
        },
    );
    save_agent_server_config(data_dir, &cfg)
        .await
        .expect("save agent config");

    let start_url = format!(
        "{}/api/providers/claude-crp/accounts/login/start",
        server.base_url
    );
    let start_resp = server
        .client
        .post(&start_url)
        .json(&json!({}))
        .send()
        .await
        .expect("start claude login request");
    assert_eq!(start_resp.status(), StatusCode::OK);
    let start_body: ClaudeLoginStartResponse = start_resp.json().await.expect("start body");
    assert_eq!(
        start_body.auth_url.as_deref(),
        Some("https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A64111%2Fcallback&state=test")
    );

    let status =
        poll_claude_login_status(&server, &start_body.login_id, Duration::from_secs(30)).await;
    assert_eq!(status.status, "success");
    assert!(status.account_id.is_some());
    assert!(status.error.is_none());
    // Keep the mock runtime and fake browser tempdir alive until the spawned
    // setup-token process has completed.
    assert!(data_dir.exists());
}

// The real desktop/browser lane still needs OS automation, but the tests below
// are process-contract checks rather than full browser coverage.
#[tokio::test]
#[ignore = "Claude subscription login requires full OS automation for truthful coverage; excluded from verify:quick until that lane exists"]
async fn claude_login_start_returns_pending_setup_token_session() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let data_dir = fixture.data_dir.path();
    let server = fixture.spawn_server().await;

    let script_path = write_mock_claude_runtime(
        data_dir,
        r#"#!/usr/bin/env bash
set -euo pipefail
echo "Claude setup-token URL: https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A64111%2Fcallback&state=test"
sleep 30
"#,
    )
    .await;
    let mut cfg = empty_agent_server_config();
    cfg.providers.insert(
        "claude-cli".to_string(),
        AgentServerCommand {
            command: script_path.to_string_lossy().to_string(),
            args: vec!["--shim".to_string()],
            dependencies: vec![],
            managed: None,
        },
    );
    save_agent_server_config(data_dir, &cfg)
        .await
        .expect("save agent config");

    let start_url = format!(
        "{}/api/providers/claude-crp/accounts/login/start",
        server.base_url
    );
    let start_resp = server
        .client
        .post(start_url)
        .json(&json!({ "label": "Claude setup-token" }))
        .send()
        .await
        .expect("start claude login request");
    assert_eq!(start_resp.status(), StatusCode::OK);
    let start_body: ClaudeLoginStartResponse = start_resp.json().await.expect("start body");
    assert!(!start_body.login_id.is_empty());
    let auth_url = parse_claude_auth_url(&start_body);
    assert_eq!(auth_url.scheme(), "https");
    assert_eq!(auth_url.host_str(), Some("claude.ai"));
    assert_eq!(auth_url.path(), "/oauth/authorize");
    assert_eq!(
        auth_url
            .query_pairs()
            .find(|(k, _)| k == "redirect_uri")
            .map(|(_, v)| v.into_owned())
            .as_deref(),
        Some("http://localhost:64111/callback")
    );

    let status_url = format!(
        "{}/api/providers/claude-crp/accounts/login/{}",
        server.base_url, start_body.login_id
    );
    let status_resp = server
        .client
        .get(status_url)
        .send()
        .await
        .expect("claude login status request");
    assert_eq!(status_resp.status(), StatusCode::OK);
    let status: ClaudeLoginStatusResponse =
        status_resp.json().await.expect("claude login status body");
    assert_eq!(status.status, "pending");
    assert!(status.account_id.is_none());
    assert!(status.error.is_none());
}

#[tokio::test]
#[ignore = "Claude subscription login requires full OS automation for truthful coverage; excluded from verify:quick until that lane exists"]
async fn claude_login_start_requires_usable_configured_login_command() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let data_dir = fixture.data_dir.path();
    let server = fixture.spawn_server().await;
    let missing_path = data_dir.join("missing-claude");
    let mut cfg = empty_agent_server_config();
    cfg.providers.insert(
        "claude-cli".to_string(),
        AgentServerCommand {
            command: missing_path.to_string_lossy().to_string(),
            args: vec![],
            dependencies: vec![],
            managed: None,
        },
    );
    save_agent_server_config(data_dir, &cfg)
        .await
        .expect("save agent config");

    let start_url = format!(
        "{}/api/providers/claude-crp/accounts/login/start",
        server.base_url
    );
    let start_resp = server
        .client
        .post(start_url)
        .json(&json!({ "label": "Claude setup-token" }))
        .send()
        .await
        .expect("start claude login request");
    assert_eq!(start_resp.status(), StatusCode::BAD_REQUEST);
    let body: ErrorResp = start_resp.json().await.expect("start error body");
    assert!(body
        .error
        .contains("runtime_command_not_found: provider=claude-cli source=user_override"));
}

#[tokio::test]
#[ignore = "Claude subscription login requires full OS automation for truthful coverage; excluded from verify:quick until that lane exists"]
async fn claude_login_start_reconstructs_wrapped_auth_url() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let data_dir = fixture.data_dir.path();
    let server = fixture.spawn_server().await;

    let script_path = write_mock_claude_runtime(
        data_dir,
        r#"#!/usr/bin/env bash
set -euo pipefail
printf "Claude setup-token URL: https://claude.ai/oauth/authorize?redirect_uri=http%%3A%%2F%%2Flocalhost%%3A\n"
printf "64111%%2Fcallback&state=test\n"
echo "forced failure after auth url"
exit 5
"#,
    )
    .await;
    let mut cfg = empty_agent_server_config();
    cfg.providers.insert(
        "claude-cli".to_string(),
        AgentServerCommand {
            command: script_path.to_string_lossy().to_string(),
            args: vec![],
            dependencies: vec![],
            managed: None,
        },
    );
    save_agent_server_config(data_dir, &cfg)
        .await
        .expect("save agent config");

    let start_url = format!(
        "{}/api/providers/claude-crp/accounts/login/start",
        server.base_url
    );
    let start_resp = server
        .client
        .post(start_url)
        .json(&json!({}))
        .send()
        .await
        .expect("start claude login request");
    assert_eq!(start_resp.status(), StatusCode::OK);
    let start_body: ClaudeLoginStartResponse = start_resp.json().await.expect("start body");
    assert_eq!(
        start_body.auth_url.as_deref(),
        Some("https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A64111%2Fcallback&state=test")
    );
}

#[tokio::test]
#[ignore = "Claude subscription login requires full OS automation for truthful coverage; excluded from verify:quick until that lane exists"]
async fn claude_login_setup_token_path_succeeds_without_callback_submission() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let data_dir = fixture.data_dir.path();
    let server = fixture.spawn_server().await;

    let script_path = write_mock_claude_runtime(
        data_dir,
        r#"#!/usr/bin/env bash
set -euo pipefail
echo "Claude setup-token URL: https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A64111%2Fcallback&state=test"
echo "Long-lived authentication token created successfully!"
echo ""
echo "Your OAuth token (valid for 1 year):"
echo ""
echo "sk-ant-oat01-abcDEF1234567890_"
echo "ZXY987654321"
"#,
    )
    .await;
    let mut cfg = empty_agent_server_config();
    cfg.providers.insert(
        "claude-cli".to_string(),
        AgentServerCommand {
            command: script_path.to_string_lossy().to_string(),
            args: vec![],
            dependencies: vec![],
            managed: None,
        },
    );
    save_agent_server_config(data_dir, &cfg)
        .await
        .expect("save agent config");

    let start_url = format!(
        "{}/api/providers/claude-crp/accounts/login/start",
        server.base_url
    );
    let start_resp = server
        .client
        .post(&start_url)
        .json(&json!({}))
        .send()
        .await
        .expect("start claude login request");
    assert_eq!(start_resp.status(), StatusCode::OK);
    let start_body: ClaudeLoginStartResponse = start_resp.json().await.expect("start body");
    assert!(start_body.auth_url.is_some());

    let status =
        poll_claude_login_status(&server, &start_body.login_id, Duration::from_secs(30)).await;
    assert_eq!(status.status, "success");
    assert!(status.account_id.is_some());
    assert!(status.error.is_none());
}

#[tokio::test]
#[ignore = "Claude subscription login requires full OS automation for truthful coverage; excluded from verify:quick until that lane exists"]
async fn claude_login_success_without_token_reports_actionable_error() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let data_dir = fixture.data_dir.path();
    let server = fixture.spawn_server().await;

    let script_path = write_mock_claude_runtime(
        data_dir,
        r#"#!/usr/bin/env bash
set -euo pipefail
echo "Claude setup-token URL: https://claude.ai/oauth/authorize?code=test"
echo "Long-lived authentication token created successfully!"
echo "Token omitted intentionally for test."
"#,
    )
    .await;
    let mut cfg = empty_agent_server_config();
    cfg.providers.insert(
        "claude-cli".to_string(),
        AgentServerCommand {
            command: script_path.to_string_lossy().to_string(),
            args: vec![],
            dependencies: vec![],
            managed: None,
        },
    );
    save_agent_server_config(data_dir, &cfg)
        .await
        .expect("save agent config");

    let start_url = format!(
        "{}/api/providers/claude-crp/accounts/login/start",
        server.base_url
    );
    let start_resp = server
        .client
        .post(start_url)
        .json(&json!({}))
        .send()
        .await
        .expect("start claude login request");
    assert_eq!(start_resp.status(), StatusCode::OK);
    let start_body: ClaudeLoginStartResponse = start_resp.json().await.expect("start body");

    let status =
        poll_claude_login_status(&server, &start_body.login_id, Duration::from_secs(30)).await;
    assert_eq!(status.status, "failed");
    assert!(status.account_id.is_none());
    assert!(status
        .error
        .unwrap_or_default()
        .contains("no setup token was detected"));
}

#[tokio::test]
#[ignore = "Claude subscription login requires full OS automation for truthful coverage; excluded from verify:quick until that lane exists"]
async fn claude_login_hang_without_auth_url_times_out_and_fails() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let data_dir = fixture.data_dir.path();
    let server = fixture.spawn_server().await;

    let script_path = write_mock_claude_runtime(
        data_dir,
        r#"#!/usr/bin/env bash
set -euo pipefail
sleep 30
"#,
    )
    .await;
    let mut cfg = empty_agent_server_config();
    cfg.providers.insert(
        "claude-cli".to_string(),
        AgentServerCommand {
            command: script_path.to_string_lossy().to_string(),
            args: vec![],
            dependencies: vec![],
            managed: None,
        },
    );
    save_agent_server_config(data_dir, &cfg)
        .await
        .expect("save agent config");

    let start_url = format!(
        "{}/api/providers/claude-crp/accounts/login/start",
        server.base_url
    );
    let start_resp = server
        .client
        .post(start_url)
        .json(&json!({}))
        .send()
        .await
        .expect("start claude login request");
    assert_eq!(start_resp.status(), StatusCode::OK);
    let start_body: ClaudeLoginStartResponse = start_resp.json().await.expect("start body");
    assert!(start_body.auth_url.is_none());

    let status =
        poll_claude_login_status(&server, &start_body.login_id, Duration::from_secs(20)).await;
    assert_eq!(status.status, "failed");
    assert!(status.account_id.is_none());
    let error = status.error.unwrap_or_default();
    assert!(
        error.contains("did not emit an authentication URL"),
        "unexpected claude no-auth-url error: {error}"
    );
}

#[tokio::test]
#[ignore = "Claude subscription login requires full OS automation for truthful coverage; excluded from verify:quick until that lane exists"]
async fn claude_login_without_label_preserves_existing_account_label() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let data_dir = fixture.data_dir.path();
    let server = fixture.spawn_server().await;

    let shared_token = "sk-ant-oat01-abcDEF1234567890_abcdefghijklmnopqrstuvwxyz_0123456789";
    let accounts_url = format!("{}/api/providers/claude-crp/accounts", server.base_url);
    let existing_resp = server
        .client
        .post(&accounts_url)
        .json(&json!({
            "label": "Claude Existing Label",
            "setup_token": shared_token
        }))
        .send()
        .await
        .expect("create existing claude account request");
    assert_eq!(existing_resp.status(), StatusCode::OK);
    let existing_body: SubscriptionAccountsResponse = existing_resp
        .json()
        .await
        .expect("existing claude account body");
    let existing_account = existing_body
        .accounts
        .first()
        .expect("existing account should be present");
    let existing_id = existing_account.id.clone();
    assert_eq!(
        existing_account.label.as_deref(),
        Some("Claude Existing Label")
    );

    let script_with_token = format!(
        r#"#!/usr/bin/env bash
set -euo pipefail
echo "Claude setup-token URL: https://claude.ai/oauth/authorize?code=test"
echo "Long-lived authentication token created successfully!"
echo ""
echo "Your OAuth token (valid for 1 year):"
echo ""
echo "{shared_token}"
"#,
    );
    let script_path = write_mock_claude_runtime(data_dir, &script_with_token).await;
    let mut cfg = empty_agent_server_config();
    cfg.providers.insert(
        "claude-cli".to_string(),
        AgentServerCommand {
            command: script_path.to_string_lossy().to_string(),
            args: vec![],
            dependencies: vec![],
            managed: None,
        },
    );
    save_agent_server_config(data_dir, &cfg)
        .await
        .expect("save agent config");

    let start_url = format!(
        "{}/api/providers/claude-crp/accounts/login/start",
        server.base_url
    );
    let start_resp = server
        .client
        .post(start_url)
        .json(&json!({}))
        .send()
        .await
        .expect("start claude login request");
    assert_eq!(start_resp.status(), StatusCode::OK);
    let start_body: ClaudeLoginStartResponse = start_resp.json().await.expect("start body");

    let status =
        poll_claude_login_status(&server, &start_body.login_id, Duration::from_secs(30)).await;
    assert_eq!(status.status, "success");
    assert_eq!(status.account_id.as_deref(), Some(existing_id.as_str()));

    let listed_resp = server
        .client
        .get(&accounts_url)
        .send()
        .await
        .expect("list claude accounts request");
    assert_eq!(listed_resp.status(), StatusCode::OK);
    let listed_body: SubscriptionAccountsResponse =
        listed_resp.json().await.expect("list claude accounts body");
    assert_eq!(listed_body.accounts.len(), 1);
    assert_eq!(listed_body.accounts[0].id, existing_id);
    assert_eq!(
        listed_body.accounts[0].label.as_deref(),
        Some("Claude Existing Label")
    );
}

#[tokio::test]
async fn gemini_subscription_accounts_crud_round_trip() {
    assert_managed_subscription_crud(
        "gemini",
        json!({
            "label": "Gemini Team",
            "oauth_creds_json": "{\"access_token\":\"a\",\"refresh_token\":\"b\"}",
            "google_accounts_json": "[{\"email\":\"dev@example.com\"}]",
            "email": "dev@example.com"
        }),
    )
    .await;
}

#[tokio::test]
async fn gemini_login_start_and_status_success_persists_account() {
    let providers = providers_with_gemini_adapter(Arc::new(GeminiLoginTestAdapter::success(
        r#"{"access_token":"access","refresh_token":"refresh"}"#,
        Some(r#"[{"email":"gemini-dev@example.com"}]"#.to_string()),
        Some("https://accounts.google.com/o/oauth2/auth?code=test".to_string()),
    )));
    let fixture = common::fake_daemon_fixture_with_providers(providers, "http://127.0.0.1:0").await;
    let server = fixture.spawn_server().await;

    let start_url = format!(
        "{}/api/providers/gemini/accounts/login/start",
        server.base_url
    );
    let start_resp = server
        .client
        .post(start_url)
        .json(&json!({ "label": "Gemini OAuth" }))
        .send()
        .await
        .expect("start gemini login request");
    assert_eq!(start_resp.status(), StatusCode::OK);
    let start_body: GeminiLoginStartResponse = start_resp.json().await.expect("start body");
    assert!(!start_body.login_id.is_empty());
    assert!(start_body.auth_url.is_none());

    let status = poll_gemini_login_status(&server, &start_body.login_id).await;
    assert_eq!(status.status, "success");
    assert!(status.account_id.is_some());
    assert!(status.error.is_none());
    assert!(status.auth_url.as_deref().is_some());

    let accounts_url = format!("{}/api/providers/gemini/accounts", server.base_url);
    let accounts_resp = server
        .client
        .get(accounts_url)
        .send()
        .await
        .expect("gemini accounts request");
    assert_eq!(accounts_resp.status(), StatusCode::OK);
    let accounts: SubscriptionAccountsResponse = accounts_resp.json().await.expect("accounts body");
    assert_eq!(accounts.accounts.len(), 1);
    assert_eq!(accounts.active_account_id, status.account_id);
    assert_eq!(accounts.accounts[0].label.as_deref(), Some("Gemini OAuth"));
}

#[tokio::test]
async fn gemini_login_start_and_status_failure_reports_error() {
    let providers = providers_with_gemini_adapter(Arc::new(GeminiLoginTestAdapter::failure(
        "gemini auth failed",
    )));
    let fixture = common::fake_daemon_fixture_with_providers(providers, "http://127.0.0.1:0").await;
    let server = fixture.spawn_server().await;

    let start_url = format!(
        "{}/api/providers/gemini/accounts/login/start",
        server.base_url
    );
    let start_resp = server
        .client
        .post(start_url)
        .json(&json!({}))
        .send()
        .await
        .expect("start gemini login request");
    assert_eq!(start_resp.status(), StatusCode::OK);
    let start_body: GeminiLoginStartResponse = start_resp.json().await.expect("start body");

    let status = poll_gemini_login_status(&server, &start_body.login_id).await;
    assert_eq!(status.status, "failed");
    assert!(status.account_id.is_none());
    assert!(status
        .error
        .unwrap_or_default()
        .contains("gemini auth failed"));

    let accounts_url = format!("{}/api/providers/gemini/accounts", server.base_url);
    let accounts_resp = server
        .client
        .get(accounts_url)
        .send()
        .await
        .expect("gemini accounts request");
    assert_eq!(accounts_resp.status(), StatusCode::OK);
    let accounts: SubscriptionAccountsResponse = accounts_resp.json().await.expect("accounts body");
    assert!(accounts.accounts.is_empty());
    assert!(accounts.active_account_id.is_none());
}

#[tokio::test]
async fn gemini_login_fails_fast_when_no_auth_url_is_emitted() {
    let providers = providers_with_gemini_adapter(Arc::new(GeminiLoginTestAdapter::no_auth_url()));
    let fixture = common::fake_daemon_fixture_with_providers(providers, "http://127.0.0.1:0").await;
    let server = fixture.spawn_server().await;

    let start_url = format!(
        "{}/api/providers/gemini/accounts/login/start",
        server.base_url
    );
    let start_resp = server
        .client
        .post(start_url)
        .json(&json!({}))
        .send()
        .await
        .expect("start gemini login request");
    assert_eq!(start_resp.status(), StatusCode::OK);
    let start_body: GeminiLoginStartResponse = start_resp.json().await.expect("start body");

    let status = poll_gemini_login_status(&server, &start_body.login_id).await;
    assert_eq!(status.status, "failed");
    assert!(status
        .error
        .unwrap_or_default()
        .contains("did not emit an OAuth URL"));
}

#[tokio::test]
async fn amp_login_auth_required_notice_reports_real_message() {
    let providers = providers_with_amp_adapter(Arc::new(AmpLoginTestAdapter::auth_required(
        Some("https://ampcode.com/auth".to_string()),
        "Amp needs subscription approval",
    )));
    let fixture = common::fake_daemon_fixture_with_providers(providers, "http://127.0.0.1:0").await;
    let server = fixture.spawn_server().await;

    let start_url = format!("{}/api/providers/amp/accounts/login/start", server.base_url);
    let start_resp = server
        .client
        .post(start_url)
        .json(&json!({}))
        .send()
        .await
        .expect("start amp login request");
    assert_eq!(start_resp.status(), StatusCode::OK);
    let start_body: AmpLoginStartResponse = start_resp.json().await.expect("start body");
    assert!(!start_body.login_id.is_empty());
    assert!(start_body.auth_url.is_none());

    let status = poll_amp_login_status(&server, &start_body.login_id).await;
    assert_eq!(status.status, "failed");
    assert_eq!(
        status.error.as_deref(),
        Some("Amp needs subscription approval")
    );
    assert_eq!(status.auth_url.as_deref(), Some("https://ampcode.com/auth"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kimi_login_start_and_status_success_persists_oauth_account() {
    let _env_lock = KIMI_TOKEN_ENV_LOCK.lock().await;
    let oauth_server = start_kimi_oauth_server().await;
    let _oauth_host = TestEnvVar::set("KIMI_CODE_OAUTH_HOST", oauth_server.0.base_url.as_str());
    let _timeout = TestEnvVar::set("CTX_KIMI_LOGIN_TIMEOUT_SECS", "5");

    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let data_dir = fixture.data_dir.path();
    let server = fixture.spawn_server().await;

    let start_url = format!(
        "{}/api/providers/kimi/accounts/login/start",
        server.base_url
    );
    let start_resp = server
        .client
        .post(start_url)
        .json(&json!({ "label": "Kimi Google" }))
        .send()
        .await
        .expect("start kimi login request");
    assert_eq!(start_resp.status(), StatusCode::OK);
    let start_body: KimiLoginStartResponse = start_resp.json().await.expect("start body");
    assert!(!start_body.login_id.is_empty());
    assert_eq!(
        start_body.auth_url.as_deref(),
        Some("http://127.0.0.1/verify?user_code=ABCD-1234")
    );
    assert_eq!(start_body.device_code.as_deref(), Some("ABCD-1234"));

    let status = poll_kimi_login_status(&server, &start_body.login_id).await;
    assert_eq!(status.status, "success");
    assert!(status.error.is_none());
    assert!(status.account_id.is_some());
    assert_eq!(
        status.auth_url.as_deref(),
        Some("http://127.0.0.1/verify?user_code=ABCD-1234")
    );
    assert_eq!(status.device_code.as_deref(), Some("ABCD-1234"));

    let accounts_url = format!("{}/api/providers/kimi/accounts", server.base_url);
    let accounts_resp = server
        .client
        .get(accounts_url)
        .send()
        .await
        .expect("kimi accounts request");
    assert_eq!(accounts_resp.status(), StatusCode::OK);
    let accounts: SubscriptionAccountsResponse = accounts_resp.json().await.expect("accounts body");
    assert_eq!(accounts.accounts.len(), 1);
    assert_eq!(accounts.active_account_id, status.account_id);
    assert_eq!(accounts.accounts[0].label.as_deref(), Some("Kimi Google"));

    let registry = ctx_provider_accounts::load_kimi_registry(data_dir)
        .await
        .unwrap();
    assert_eq!(registry.accounts.len(), 1);
    assert_eq!(registry.accounts[0].kind, "oauth");

    let polls = oauth_server.1.lock().expect("poll mutex");
    assert!(polls.len() >= 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kimi_login_start_device_authorization_failure_returns_bad_gateway() {
    let _env_lock = KIMI_TOKEN_ENV_LOCK.lock().await;
    let oauth_server =
        start_kimi_oauth_server_with_scenario(KimiOAuthScenario::DeviceAuthorizationFailure).await;
    let _oauth_host = TestEnvVar::set("KIMI_CODE_OAUTH_HOST", oauth_server.0.base_url.as_str());

    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let server = fixture.spawn_server().await;

    let start_url = format!(
        "{}/api/providers/kimi/accounts/login/start",
        server.base_url
    );
    let start_resp = server
        .client
        .post(start_url)
        .json(&json!({}))
        .send()
        .await
        .expect("start kimi login request");
    assert_eq!(start_resp.status(), StatusCode::BAD_GATEWAY);
    let body: ErrorResp = start_resp.json().await.expect("start error body");
    assert!(body.error.contains("Kimi device authorization failed"));
    assert!(body.error.contains("[REDACTED]"));
    assert!(!body.error.contains("startup-secret"));

    let polls = oauth_server.1.lock().expect("poll mutex");
    assert!(polls.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kimi_login_pending_until_timeout_reports_timeout() {
    let (_start, status, polls) = run_kimi_login_scenario(
        KimiOAuthScenario::PendingUntilTimeout,
        common::fake_providers(),
    )
    .await;

    assert_eq!(status.status, "timeout");
    assert_eq!(
        status.error.as_deref(),
        Some("timed out waiting for Kimi sign-in completion")
    );
    assert!(status.account_id.is_none());
    assert!(!polls.lock().expect("poll mutex").is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kimi_login_expired_token_reports_timeout() {
    let (_start, status, _polls) =
        run_kimi_login_scenario(KimiOAuthScenario::ExpiredToken, common::fake_providers()).await;

    assert_eq!(status.status, "timeout");
    assert_eq!(status.error.as_deref(), Some("Device code expired"));
    assert!(status.account_id.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kimi_login_access_denied_reports_failed() {
    let (_start, status, _polls) =
        run_kimi_login_scenario(KimiOAuthScenario::AccessDenied, common::fake_providers()).await;

    assert_eq!(status.status, "failed");
    assert_eq!(status.error.as_deref(), Some("User denied Kimi sign-in."));
    assert!(status.account_id.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kimi_login_unknown_oauth_error_reports_failed() {
    let (_start, status, _polls) = run_kimi_login_scenario(
        KimiOAuthScenario::UnknownClientError,
        common::fake_providers(),
    )
    .await;

    assert_eq!(status.status, "failed");
    assert_eq!(
        status.error.as_deref(),
        Some("Kimi OAuth returned an unknown error")
    );
    assert!(status.account_id.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kimi_login_token_server_error_reports_failed_with_redaction() {
    let (_start, status, _polls) = run_kimi_login_scenario(
        KimiOAuthScenario::TokenServerError,
        common::fake_providers(),
    )
    .await;

    assert_eq!(status.status, "failed");
    let error = status.error.expect("token server error");
    assert!(error.contains("Kimi token polling failed"));
    assert!(error.contains("[REDACTED]"));
    assert!(!error.contains("token-secret"));
    assert!(status.account_id.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kimi_login_malformed_token_success_reports_failed() {
    let (_start, status, _polls) = run_kimi_login_scenario(
        KimiOAuthScenario::MalformedTokenSuccess,
        common::fake_providers(),
    )
    .await;

    assert_eq!(status.status, "failed");
    assert!(status
        .error
        .as_deref()
        .is_some_and(|error| error.contains("parsing Kimi token success response")));
    assert!(status.account_id.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kimi_login_success_with_restart_failure_reports_failed() {
    let providers = providers_with_kimi_adapter(Arc::new(KimiRestartFailureAdapter));
    let (_start, status, _polls) =
        run_kimi_login_scenario(KimiOAuthScenario::Success, providers).await;

    assert_eq!(status.status, "failed");
    assert!(status.account_id.is_some());
    assert!(
        status.error.as_deref().is_some_and(|error| {
            error.contains("auth saved but provider restart failed")
                && error.contains("forced kimi restart failure")
        }),
        "unexpected error: {:?}",
        status.error
    );
}

#[tokio::test]
async fn qwen_login_start_and_status_success_persists_account() {
    let providers = providers_with_qwen_adapter(Arc::new(QwenLoginTestAdapter::success(
        r#"{"access_token":"access","refresh_token":"refresh"}"#,
        Some("https://chat.qwen.ai/oauth/authorize?code=test".to_string()),
    )));
    let fixture = common::fake_daemon_fixture_with_providers(providers, "http://127.0.0.1:0").await;
    let server = fixture.spawn_server().await;

    let start_url = format!(
        "{}/api/providers/qwen/accounts/login/start",
        server.base_url
    );
    let start_resp = server
        .client
        .post(start_url)
        .json(&json!({ "label": "Qwen OAuth" }))
        .send()
        .await
        .expect("start qwen login request");
    assert_eq!(start_resp.status(), StatusCode::OK);
    let start_body: QwenLoginStartResponse = start_resp.json().await.expect("start body");
    assert!(!start_body.login_id.is_empty());
    assert!(start_body.auth_url.is_none());

    let status = poll_qwen_login_status(&server, &start_body.login_id).await;
    assert_eq!(status.status, "success");
    assert!(status.account_id.is_some());
    assert!(status.error.is_none());
    assert!(status.auth_url.as_deref().is_some());

    let accounts_url = format!("{}/api/providers/qwen/accounts", server.base_url);
    let accounts_resp = server
        .client
        .get(accounts_url)
        .send()
        .await
        .expect("qwen accounts request");
    assert_eq!(accounts_resp.status(), StatusCode::OK);
    let accounts: SubscriptionAccountsResponse = accounts_resp.json().await.expect("accounts body");
    assert_eq!(accounts.accounts.len(), 1);
    assert_eq!(accounts.active_account_id, status.account_id);
    assert_eq!(accounts.accounts[0].label.as_deref(), Some("Qwen OAuth"));
}

#[tokio::test]
async fn kimi_accounts_list_fails_closed_on_malformed_registry_json() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let data_dir = fixture.data_dir.path();
    let path = ctx_provider_accounts::kimi_registry_path(data_dir);
    tokio::fs::create_dir_all(path.parent().expect("registry parent"))
        .await
        .expect("create kimi registry parent");
    tokio::fs::write(&path, "{ invalid json")
        .await
        .expect("write malformed kimi registry");

    let server = fixture.spawn_server().await;

    let accounts_url = format!("{}/api/providers/kimi/accounts", server.base_url);
    let response = server
        .client
        .get(accounts_url)
        .send()
        .await
        .expect("kimi accounts request");
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body: ErrorResp = response.json().await.expect("error body");
    assert!(
        body.error.contains("Kimi account registry"),
        "expected registry label in error: {}",
        body.error
    );
    assert!(
        body.error.contains("parsing"),
        "expected parse context in error: {}",
        body.error
    );
}

#[tokio::test]
async fn auth_import_profiles_route_fails_closed_on_malformed_registry_json() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let data_dir = fixture.data_dir.path();
    let path = data_dir
        .join("providers")
        .join("auth_import")
        .join("profiles.json");
    tokio::fs::create_dir_all(path.parent().expect("registry parent"))
        .await
        .expect("create import registry parent");
    tokio::fs::write(&path, "{ invalid json")
        .await
        .expect("write malformed import registry");

    let server = fixture.spawn_server().await;

    let profiles_url = format!("{}/api/providers/auth/import/profiles", server.base_url);
    let response = server
        .client
        .get(profiles_url)
        .send()
        .await
        .expect("auth import profiles request");
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body: ErrorResp = response.json().await.expect("error body");
    assert!(
        body.error.contains("parsing imported auth registry"),
        "expected parse context in error: {}",
        body.error
    );
    assert!(
        body.error.contains("profiles.json"),
        "expected registry path in error: {}",
        body.error
    );
}

#[tokio::test]
async fn qwen_subscription_accounts_crud_round_trip() {
    assert_managed_subscription_crud(
        "qwen",
        json!({
            "label": "Qwen Managed",
            "oauth_creds_json": "{\"access_token\":\"a\",\"refresh_token\":\"b\"}",
            "email": "dev@example.com"
        }),
    )
    .await;
}

#[tokio::test]
async fn cursor_login_start_requires_cursor_agent_runtime() {
    let _env_lock = CLAUDE_TOKEN_ENV_LOCK.lock().await;
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let data_dir = fixture.data_dir.path();
    let server = fixture.spawn_server().await;
    let _path_guard = TestEnvVar::set("PATH", data_dir.to_string_lossy().as_ref());

    let start_url = format!(
        "{}/api/providers/cursor/accounts/login/start",
        server.base_url
    );
    let start_resp = server
        .client
        .post(start_url)
        .json(&json!({}))
        .send()
        .await
        .expect("start cursor login request");
    assert_eq!(start_resp.status(), StatusCode::BAD_REQUEST);
    let body: ErrorResp = start_resp.json().await.expect("start error body");
    assert!(body
        .error
        .contains("runtime_command_missing: provider=cursor-login"));
}

#[tokio::test]
async fn cursor_login_start_and_status_success_persists_account() {
    let _env_lock = CLAUDE_TOKEN_ENV_LOCK.lock().await;
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let data_dir = fixture.data_dir.path();
    let server = fixture.spawn_server().await;

    let cursor_script = write_mock_cursor_runtime(
        data_dir,
        r#"#!/bin/sh
set -eu
capture_path="${CTX_CURSOR_CAPTURE_FILE:-}"
printf '%s\n' 'https://cursor.com/login/device?code=test'
printf '%s\n' 'Signed in as cursor-dev@example.com'
if [ -n "$capture_path" ]; then
  printf '%s\n' '{"event":"captured","service":"cursor-access-token","value":"cursor-access-token"}' >> "$capture_path"
  printf '%s\n' '{"event":"captured","service":"cursor-refresh-token","value":"cursor-refresh-token"}' >> "$capture_path"
fi
"#,
    )
    .await;

    let mut cfg = empty_agent_server_config();
    set_login_executable(&mut cfg, "cursor", &cursor_script);
    save_agent_server_config(data_dir, &cfg)
        .await
        .expect("save agent config");

    let start_url = format!(
        "{}/api/providers/cursor/accounts/login/start",
        server.base_url
    );
    let start_resp = server
        .client
        .post(start_url)
        .json(&json!({ "label": "Cursor OAuth" }))
        .send()
        .await
        .expect("start cursor login request");
    assert_eq!(start_resp.status(), StatusCode::OK);
    let start_body: CursorLoginStartResponse = start_resp.json().await.expect("start body");
    assert!(!start_body.login_id.is_empty());
    assert!(start_body.auth_url.is_none());

    let status = poll_cursor_login_status(&server, &start_body.login_id).await;
    assert_eq!(status.status, "success");
    assert!(status.account_id.is_some());
    assert_eq!(
        status.auth_url.as_deref(),
        Some("https://cursor.com/login/device?code=test")
    );
    assert!(status.error.is_none());

    let accounts_url = format!("{}/api/providers/cursor/accounts", server.base_url);
    let accounts_resp = server
        .client
        .get(accounts_url)
        .send()
        .await
        .expect("cursor accounts request");
    assert_eq!(accounts_resp.status(), StatusCode::OK);
    let accounts: SubscriptionAccountsResponse = accounts_resp.json().await.expect("accounts body");
    assert_eq!(accounts.accounts.len(), 1);
    assert_eq!(accounts.active_account_id, status.account_id);
    assert_eq!(accounts.accounts[0].label.as_deref(), Some("Cursor OAuth"));
}

#[tokio::test]
async fn cursor_login_start_rejects_host_path_discovery_and_does_not_persist_login_command() {
    let _env_lock = CLAUDE_TOKEN_ENV_LOCK.lock().await;
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let data_dir = fixture.data_dir.path();
    let server = fixture.spawn_server().await;

    write_mock_cursor_runtime(
        data_dir,
        r#"#!/bin/sh
set -eu
capture_path="${CTX_CURSOR_CAPTURE_FILE:-}"
printf '%s\n' 'https://cursor.com/login/device?code=discovered'
printf '%s\n' 'Signed in as discovered-cursor@example.com'
if [ -n "$capture_path" ]; then
  printf '%s\n' '{"event":"captured","service":"cursor-access-token","value":"cursor-access-token-discovered"}' >> "$capture_path"
  printf '%s\n' '{"event":"captured","service":"cursor-refresh-token","value":"cursor-refresh-token-discovered"}' >> "$capture_path"
fi
"#,
    )
    .await;
    let existing_path = std::env::var("PATH").unwrap_or_default();
    let combined_path = format!("{}:{}", data_dir.to_string_lossy(), existing_path);
    let _path_guard = TestEnvVar::set("PATH", &combined_path);

    let start_url = format!(
        "{}/api/providers/cursor/accounts/login/start",
        server.base_url
    );
    let start_resp = server
        .client
        .post(start_url)
        .json(&json!({ "label": "Cursor OAuth Discovered" }))
        .send()
        .await
        .expect("start cursor login request");
    assert_eq!(start_resp.status(), StatusCode::BAD_REQUEST);
    let body: ErrorResp = start_resp.json().await.expect("start error body");
    assert!(body
        .error
        .contains("runtime_command_missing: provider=cursor-login"));
    assert!(body.error.contains("host PATH lookup is not supported"));

    let cfg = load_agent_server_config(data_dir)
        .await
        .expect("load persisted agent config");
    assert!(
        !cfg.provider_login_executables.contains_key("cursor"),
        "host PATH discovery must not persist a cursor login executable"
    );
}

#[tokio::test]
async fn mistral_login_start_and_status_success_persists_account() {
    let providers = providers_with_mistral_adapter(Arc::new(MistralLoginTestAdapter::success(
        Some("https://auth.mistral.ai/oauth/authorize?code=test".to_string()),
        Some("mistral-dev@example.com".to_string()),
    )));
    let fixture = common::fake_daemon_fixture_with_providers(providers, "http://127.0.0.1:0").await;
    let server = fixture.spawn_server().await;

    let start_url = format!(
        "{}/api/providers/mistral/accounts/login/start",
        server.base_url
    );
    let start_resp = server
        .client
        .post(start_url)
        .json(&json!({ "label": "Mistral OAuth" }))
        .send()
        .await
        .expect("start mistral login request");
    assert_eq!(start_resp.status(), StatusCode::OK);
    let start_body: MistralLoginStartResponse = start_resp.json().await.expect("start body");
    assert!(!start_body.login_id.is_empty());
    assert!(start_body.auth_url.is_none());

    let status = poll_mistral_login_status(&server, &start_body.login_id).await;
    assert_eq!(status.status, "success");
    assert!(status.error.is_none());
    assert!(status.auth_url.is_none());

    let accounts_url = format!("{}/api/providers/mistral/accounts", server.base_url);
    let accounts_resp = server
        .client
        .get(accounts_url)
        .send()
        .await
        .expect("mistral accounts request");
    assert_eq!(accounts_resp.status(), StatusCode::OK);
    let accounts: SubscriptionAccountsResponse = accounts_resp.json().await.expect("accounts body");
    assert_eq!(accounts.accounts.len(), 1);
    assert!(accounts.active_account_id.is_some());
    assert_eq!(accounts.accounts[0].label.as_deref(), Some("Mistral OAuth"));
}

#[tokio::test]
async fn mistral_subscription_accounts_crud_round_trip() {
    assert_managed_subscription_crud(
        "mistral",
        json!({
            "label": "Mistral Managed",
            "email": "dev@example.com"
        }),
    )
    .await;
}

#[tokio::test]
async fn amp_subscription_accounts_crud_round_trip() {
    assert_managed_subscription_crud(
        "amp",
        json!({
            "label": "Amp Managed",
            "email": "dev@example.com"
        }),
    )
    .await;
}

#[tokio::test]
async fn kimi_subscription_accounts_crud_round_trip() {
    assert_managed_subscription_crud(
        "kimi",
        json!({
            "label": "Kimi Team",
            "provider": "moonshot",
            "credentials_json": "{\"access_token\":\"a\",\"refresh_token\":\"b\"}",
            "config_toml": "current_provider = \"moonshot\"",
            "email": "dev@example.com"
        }),
    )
    .await;
}

#[tokio::test]
async fn copilot_subscription_accounts_crud_round_trip() {
    assert_managed_subscription_crud(
        "copilot",
        json!({
            "label": "Copilot Team",
            "token": "ghp_abc",
            "email": "dev@example.com"
        }),
    )
    .await;
}

#[tokio::test]
async fn cursor_subscription_accounts_crud_round_trip() {
    assert_managed_subscription_crud(
        "cursor",
        json!({
            "label": "Cursor Team",
            "token": "cursor-key",
            "email": "dev@example.com"
        }),
    )
    .await;
}

#[tokio::test]
async fn gemini_upsert_rejects_invalid_oauth_json() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let server = fixture.spawn_server().await;
    let accounts_url = format!("{}/api/providers/gemini/accounts", server.base_url);

    let resp = server
        .client
        .post(accounts_url)
        .json(&json!({
            "label": "Gemini Bad",
            "oauth_creds_json": "not-json"
        }))
        .send()
        .await
        .expect("invalid upsert request");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: ErrorResp = resp.json().await.expect("error response json");
    assert!(body.error.contains("valid JSON"));
}

#[tokio::test]
async fn kimi_upsert_rejects_invalid_credentials_json() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let server = fixture.spawn_server().await;
    let accounts_url = format!("{}/api/providers/kimi/accounts", server.base_url);

    let resp = server
        .client
        .post(accounts_url)
        .json(&json!({
            "label": "Kimi Bad",
            "credentials_json": "not-json"
        }))
        .send()
        .await
        .expect("invalid upsert request");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: ErrorResp = resp.json().await.expect("error response json");
    assert!(body.error.contains("valid JSON"));
}
