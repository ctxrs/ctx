use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ctx_observability::logs;
use ctx_provider_accounts as provider_accounts;
use ctx_provider_runtime::provider_session_auth::{
    ProviderSessionAuthenticationError, ProviderSessionAuthenticationRequest,
};
use ctx_provider_runtime::ProviderRuntime;
use ctx_providers::events::NormalizedEvent;
use tokio::sync::mpsc;

pub(super) const GEMINI_LOGIN_POLL_INTERVAL: Duration = Duration::from_millis(700);
pub(super) const QWEN_LOGIN_POLL_INTERVAL: Duration = Duration::from_millis(700);
pub(super) const AMP_LOGIN_POLL_INTERVAL: Duration = Duration::from_millis(700);
pub(super) const MISTRAL_LOGIN_POLL_INTERVAL: Duration = Duration::from_millis(700);
pub(super) const QWEN_OAUTH_AUTH_METHOD_ID: &str = "qwen-oauth";
pub(super) const AMP_BROWSER_AUTH_METHOD_ID: &str = "amp_browser_login";

const GEMINI_LOGIN_TIMEOUT_DEFAULT: Duration = Duration::from_secs(300);
const QWEN_LOGIN_TIMEOUT_DEFAULT: Duration = Duration::from_secs(300);
const AMP_LOGIN_TIMEOUT_DEFAULT: Duration = Duration::from_secs(300);
const MISTRAL_LOGIN_TIMEOUT_DEFAULT: Duration = Duration::from_secs(300);

pub(super) fn gemini_login_timeout() -> Duration {
    login_timeout_from_env(
        "CTX_GEMINI_LOGIN_TIMEOUT_SECS",
        GEMINI_LOGIN_TIMEOUT_DEFAULT,
    )
}

pub(super) fn qwen_login_timeout() -> Duration {
    login_timeout_from_env("CTX_QWEN_LOGIN_TIMEOUT_SECS", QWEN_LOGIN_TIMEOUT_DEFAULT)
}

pub(super) fn amp_login_timeout() -> Duration {
    login_timeout_from_env("CTX_AMP_LOGIN_TIMEOUT_SECS", AMP_LOGIN_TIMEOUT_DEFAULT)
}

pub(super) fn mistral_login_timeout() -> Duration {
    login_timeout_from_env(
        "CTX_MISTRAL_LOGIN_TIMEOUT_SECS",
        MISTRAL_LOGIN_TIMEOUT_DEFAULT,
    )
}

fn login_timeout_from_env(env_key: &str, default: Duration) -> Duration {
    let seconds = std::env::var(env_key)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default.as_secs());
    Duration::from_secs(seconds)
}

pub(super) async fn authenticate_browser_login_session(
    providers: &ProviderRuntime,
    provider_id: &'static str,
    session_key: String,
    workdir: PathBuf,
    env: HashMap<String, String>,
    method_id: Option<String>,
) -> Result<mpsc::Receiver<NormalizedEvent>, String> {
    let (event_tx, event_rx) = mpsc::channel(64);
    let auth_result = providers
        .authenticate_provider_session(
            provider_id,
            ProviderSessionAuthenticationRequest {
                session_key,
                workdir,
                env,
                method_id,
                event_sink: event_tx,
                hooks: ctx_providers::adapters::ProviderRunHooks::default(),
            },
        )
        .await;
    match auth_result {
        Ok(()) => Ok(event_rx),
        Err(ProviderSessionAuthenticationError::AdapterUnavailable) => {
            Err("provider adapter not available".to_string())
        }
        Err(ProviderSessionAuthenticationError::Authenticate(err)) => {
            Err(logs::redact_sensitive(&err.to_string()))
        }
    }
}

pub(super) async fn cleanup_login_home(login_home: &Path) {
    let _ = tokio::fs::remove_dir_all(login_home).await;
}

pub(super) async fn read_trimmed_file(path: &Path) -> Option<String> {
    tokio::fs::read_to_string(path)
        .await
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(super) fn first_email_from_google_accounts(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => {
            let email = map
                .get("email")
                .or_else(|| map.get("accountEmail"))
                .or_else(|| map.get("account_email"))
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|candidate| !candidate.is_empty())
                .map(ToString::to_string);
            email.or_else(|| map.values().find_map(first_email_from_google_accounts))
        }
        serde_json::Value::Array(values) => {
            values.iter().find_map(first_email_from_google_accounts)
        }
        _ => None,
    }
}

pub(super) fn first_email_from_value(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => {
            let direct = map
                .get("email")
                .or_else(|| map.get("accountEmail"))
                .or_else(|| map.get("account_email"))
                .or_else(|| map.get("user_email"))
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|candidate| !candidate.is_empty())
                .map(ToString::to_string);
            direct.or_else(|| map.values().find_map(first_email_from_value))
        }
        serde_json::Value::Array(values) => values.iter().find_map(first_email_from_value),
        _ => None,
    }
}

pub(super) fn auth_notice_code(payload: &serde_json::Value) -> &str {
    payload
        .get("code")
        .or_else(|| payload.get("kind"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
}

pub(super) fn is_auth_success_notice_code(code: &str) -> bool {
    matches!(
        code,
        "auth_complete" | "auth_completed" | "auth_success" | "authenticated"
    )
}

pub(super) fn is_auth_failure_notice_code(code: &str) -> bool {
    matches!(
        code,
        "auth_failed" | "auth_error" | "provider_session_ref_claim_failed"
    )
}

pub(super) fn extract_auth_url_from_value(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(raw) => {
            let trimmed = raw.trim();
            if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
                return Some(trimmed.to_string());
            }
            extract_auth_url(trimmed)
        }
        serde_json::Value::Object(map) => {
            let direct = [
                "auth_url",
                "authUrl",
                "url",
                "login_url",
                "loginUrl",
                "authorize_url",
            ]
            .into_iter()
            .find_map(|key| map.get(key))
            .and_then(extract_auth_url_from_value);
            direct.or_else(|| map.values().find_map(extract_auth_url_from_value))
        }
        serde_json::Value::Array(values) => values.iter().find_map(extract_auth_url_from_value),
        _ => None,
    }
}

fn extract_auth_url(text: &str) -> Option<String> {
    let start = text.find("https://").or_else(|| text.find("http://"))?;
    let candidate = &text[start..];
    let end = candidate
        .find(|ch: char| ch.is_whitespace() || ch == '"' || ch == '\'' || ch == '`')
        .unwrap_or(candidate.len());
    let url = candidate[..end]
        .trim_end_matches(&['.', ',', ';', ')', ']'][..])
        .to_string();
    (!url.is_empty()).then_some(url)
}

pub(super) fn gemini_auth_method_id() -> String {
    provider_accounts::GEMINI_CREDENTIAL_KIND_OAUTH_PERSONAL.to_string()
}
