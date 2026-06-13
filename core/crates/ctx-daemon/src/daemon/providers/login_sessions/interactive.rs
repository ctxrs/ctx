use ctx_observability::logs;
use ctx_provider_accounts as provider_accounts;
use ctx_provider_runtime::ProviderRuntime;

use super::{new_started_login_session, StartedLoginSession};

pub async fn start_cursor_login_session(providers: &ProviderRuntime) -> StartedLoginSession {
    let session = new_started_login_session(None, None);
    providers
        .with_cursor_login_sessions(|map| {
            map.insert(
                session.login_id.clone(),
                provider_accounts::CursorLoginStatus {
                    login_id: session.login_id.clone(),
                    auth_url: session.auth_url.clone(),
                    status: "pending".to_string(),
                    account_id: None,
                    error: None,
                },
            );
        })
        .await;
    session
}

pub async fn cursor_login_status(
    providers: &ProviderRuntime,
    login_id: &str,
) -> Option<provider_accounts::CursorLoginStatus> {
    providers
        .with_cursor_login_sessions(|map| map.get(login_id).cloned())
        .await
}

pub async fn set_cursor_login_error(providers: &ProviderRuntime, login_id: &str, error: String) {
    providers
        .with_cursor_login_sessions(|map| {
            if let Some(entry) = map.get_mut(login_id) {
                entry.status = "failed".to_string();
                entry.error = Some(error);
            }
        })
        .await;
}

pub async fn update_cursor_login_auth_url(
    providers: &ProviderRuntime,
    login_id: &str,
    auth_url: String,
) {
    providers
        .with_cursor_login_sessions(|map| {
            if let Some(entry) = map.get_mut(login_id) {
                entry.auth_url = Some(auth_url);
            }
        })
        .await;
}

pub async fn start_claude_login_session(
    providers: &ProviderRuntime,
    auth_url: Option<String>,
) -> StartedLoginSession {
    let session = new_started_login_session(auth_url, None);
    providers
        .with_claude_login_sessions(|map| {
            map.insert(
                session.login_id.clone(),
                provider_accounts::ClaudeLoginStatus {
                    login_id: session.login_id.clone(),
                    auth_url: session.auth_url.clone(),
                    status: "pending".to_string(),
                    account_id: None,
                    error: None,
                },
            );
        })
        .await;
    session
}

pub async fn claude_login_status(
    providers: &ProviderRuntime,
    login_id: &str,
) -> Option<provider_accounts::ClaudeLoginStatus> {
    providers
        .with_claude_login_sessions(|map| map.get(login_id).cloned())
        .await
}

pub async fn set_claude_login_auth_url(
    providers: &ProviderRuntime,
    login_id: &str,
    auth_url: String,
) {
    providers
        .with_claude_login_sessions(|map| {
            if let Some(entry) = map.get_mut(login_id) {
                entry.auth_url = Some(auth_url);
            }
        })
        .await;
}

pub async fn start_kimi_login_session(
    providers: &ProviderRuntime,
    auth_url: Option<String>,
    device_code: Option<String>,
) -> StartedLoginSession {
    let session = new_started_login_session(auth_url, device_code);
    providers
        .with_kimi_login_sessions(|map| {
            map.insert(
                session.login_id.clone(),
                provider_accounts::KimiLoginStatus {
                    login_id: session.login_id.clone(),
                    status: "pending".to_string(),
                    account_id: None,
                    auth_url: session.auth_url.clone(),
                    device_code: session.device_code.clone(),
                    error: None,
                },
            );
        })
        .await;
    session
}

pub async fn kimi_login_status(
    providers: &ProviderRuntime,
    login_id: &str,
) -> Option<provider_accounts::KimiLoginStatus> {
    providers
        .with_kimi_login_sessions(|map| map.get(login_id).cloned())
        .await
}

pub async fn set_kimi_login_failed(providers: &ProviderRuntime, login_id: &str, error: String) {
    providers
        .with_kimi_login_sessions(|map| {
            if let Some(entry) = map.get_mut(login_id) {
                entry.status = "failed".to_string();
                entry.error = Some(error);
            }
        })
        .await;
}

pub async fn set_kimi_login_timeout_if_no_error(
    providers: &ProviderRuntime,
    login_id: &str,
    error: String,
) {
    providers
        .with_kimi_login_sessions(|map| {
            if let Some(entry) = map.get_mut(login_id) {
                entry.status = "timeout".to_string();
                if entry.error.is_none() {
                    entry.error = Some(error);
                }
            }
        })
        .await;
}

pub async fn set_kimi_login_terminal_status(
    providers: &ProviderRuntime,
    login_id: &str,
    status: &'static str,
    error: String,
) {
    providers
        .with_kimi_login_sessions(|map| {
            if let Some(entry) = map.get_mut(login_id) {
                entry.status = status.to_string();
                entry.error = Some(error);
            }
        })
        .await;
}

pub async fn finish_kimi_login_session(
    providers: &ProviderRuntime,
    login_id: &str,
    account_id: Option<String>,
    restart_error: Option<String>,
) {
    providers
        .with_kimi_login_sessions(|map| {
            if let Some(entry) = map.get_mut(login_id) {
                entry.account_id = account_id;
                if let Some(error) = restart_error {
                    entry.status = "failed".to_string();
                    entry.error = Some(logs::redact_sensitive(&error));
                } else {
                    entry.status = "success".to_string();
                    entry.error = None;
                }
            }
        })
        .await;
}

pub async fn finish_cursor_login_session(
    providers: &ProviderRuntime,
    login_id: &str,
    status: String,
    account_id: Option<String>,
    error: Option<String>,
    observed_auth_url: Option<String>,
) {
    providers
        .with_cursor_login_sessions(|map| {
            if let Some(entry) = map.get_mut(login_id) {
                entry.status = status;
                entry.account_id = account_id;
                entry.error = error;
                if entry.auth_url.is_none() {
                    entry.auth_url = observed_auth_url;
                }
            }
        })
        .await;
}

pub async fn finish_claude_login_session(
    providers: &ProviderRuntime,
    login_id: &str,
    status: String,
    account_id: Option<String>,
    error: Option<String>,
    observed_auth_url: Option<String>,
) {
    providers
        .with_claude_login_sessions(|map| {
            if let Some(entry) = map.get_mut(login_id) {
                entry.status = status;
                entry.account_id = account_id;
                entry.error = error;
                if entry.auth_url.is_none() {
                    entry.auth_url = observed_auth_url;
                }
            }
        })
        .await;
}
