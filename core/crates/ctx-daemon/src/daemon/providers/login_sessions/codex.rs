use ctx_provider_accounts as provider_accounts;
use ctx_provider_runtime::ProviderRuntime;

#[derive(Debug)]
pub struct StartedCodexLoginSession {
    pub account_id: String,
    pub auth_url: String,
    pub expected_callback_url: Option<String>,
    pub completion_token: String,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum CodexLoginCallbackClaimError {
    NotFound,
    NotPending,
    InvalidCompletionToken,
    MissingExpectedCallback,
}

pub async fn start_codex_login_session(
    providers: &ProviderRuntime,
    account_id: String,
    auth_url: String,
    expected_callback_url: Option<String>,
) -> StartedCodexLoginSession {
    let completion_token = uuid::Uuid::new_v4().to_string();
    let status = provider_accounts::CodexLoginStatus {
        account_id: account_id.clone(),
        auth_url: auth_url.clone(),
        expected_callback_url: expected_callback_url.clone(),
        completion_token: Some(completion_token.clone()),
        status: "pending".to_string(),
        error: None,
    };
    providers
        .with_codex_login_sessions(|map| {
            map.insert(account_id.clone(), status);
        })
        .await;

    StartedCodexLoginSession {
        account_id,
        auth_url,
        expected_callback_url,
        completion_token,
    }
}

pub async fn codex_login_status(
    providers: &ProviderRuntime,
    account_id: &str,
) -> Option<provider_accounts::CodexLoginStatus> {
    providers
        .with_codex_login_sessions(|map| map.get(account_id).cloned())
        .await
}

pub async fn codex_login_statuses(
    providers: &ProviderRuntime,
) -> Vec<provider_accounts::CodexLoginStatus> {
    providers
        .with_codex_login_sessions(|map| map.values().cloned().collect())
        .await
}

pub async fn remove_codex_login_session(
    providers: &ProviderRuntime,
    account_id: &str,
) -> Vec<provider_accounts::CodexLoginStatus> {
    providers
        .with_codex_login_sessions(|map| {
            map.remove(account_id);
            map.values().cloned().collect()
        })
        .await
}

pub async fn claim_codex_login_callback(
    providers: &ProviderRuntime,
    account_id: &str,
    completion_token: &str,
) -> Result<String, CodexLoginCallbackClaimError> {
    providers
        .with_codex_login_sessions(|map| {
            let Some(status) = map.get_mut(account_id) else {
                return Err(CodexLoginCallbackClaimError::NotFound);
            };
            if status.status != "pending" {
                return Err(CodexLoginCallbackClaimError::NotPending);
            }
            if status.completion_token.as_deref() != Some(completion_token) {
                return Err(CodexLoginCallbackClaimError::InvalidCompletionToken);
            }
            let Some(expected_callback) = status.expected_callback_url.clone() else {
                return Err(CodexLoginCallbackClaimError::MissingExpectedCallback);
            };
            status.completion_token = None;
            Ok(expected_callback)
        })
        .await
}

pub async fn restore_codex_login_completion_token(
    providers: &ProviderRuntime,
    account_id: &str,
    completion_token: &str,
) {
    providers
        .with_codex_login_sessions(|map| {
            if let Some(status) = map.get_mut(account_id) {
                if status.status == "pending" && status.completion_token.is_none() {
                    status.completion_token = Some(completion_token.to_string());
                }
            }
        })
        .await;
}

pub async fn finish_codex_login_session(
    providers: &ProviderRuntime,
    account_id: &str,
    success: bool,
    error: Option<String>,
) {
    providers
        .with_codex_login_sessions(|map| {
            if let Some(entry) = map.get_mut(account_id) {
                entry.status = if success {
                    "success".to_string()
                } else {
                    "failed".to_string()
                };
                entry.completion_token = None;
                entry.error = error;
            }
        })
        .await;
}
