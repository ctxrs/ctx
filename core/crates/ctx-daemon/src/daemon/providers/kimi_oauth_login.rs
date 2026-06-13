use std::time::{Duration, Instant};

use anyhow::Context;
use chrono::Utc;
use ctx_observability::logs;
use serde::Deserialize;

use crate::daemon::providers::login_deps::ProviderLoginDeps;
use crate::daemon::providers::{accounts, login_sessions, StartedLoginSession};

const KIMI_CODE_CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";
const KIMI_LOGIN_TIMEOUT_DEFAULT: Duration = Duration::from_secs(300);
const KIMI_LOGIN_POLL_INTERVAL_FALLBACK: Duration = Duration::from_secs(5);
const KIMI_DEFAULT_OAUTH_HOST: &str = "https://auth.kimi.com";

#[derive(Debug)]
pub struct KimiOAuthLoginStartError {
    message: String,
}

impl KimiOAuthLoginStartError {
    fn from_error(err: anyhow::Error) -> Self {
        Self {
            message: logs::redact_sensitive(&err.to_string()),
        }
    }

    #[cfg(test)]
    pub(super) fn for_route_test(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn route_safe_message(&self) -> &str {
        &self.message
    }
}

#[derive(Debug, Deserialize)]
struct KimiDeviceAuthorizationResp {
    user_code: String,
    device_code: String,
    #[serde(default)]
    verification_uri: Option<String>,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    interval: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct KimiTokenSuccessResp {
    access_token: String,
    refresh_token: String,
    expires_in: f64,
    scope: String,
    token_type: String,
}

#[derive(Debug, Deserialize)]
struct KimiTokenErrorResp {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

pub async fn start_kimi_oauth_login(
    deps: ProviderLoginDeps,
    label: Option<String>,
) -> Result<StartedLoginSession, KimiOAuthLoginStartError> {
    let auth = request_kimi_device_authorization()
        .await
        .map_err(KimiOAuthLoginStartError::from_error)?;
    let auth_url = auth
        .verification_uri_complete
        .clone()
        .or(auth.verification_uri.clone());
    let device_code = Some(auth.user_code.clone());
    let login_session =
        login_sessions::start_kimi_login_session(deps.providers(), auth_url, device_code).await;

    let login_id = login_session.login_id.clone();
    let poll_interval = poll_interval_for_authorization(&auth);
    let timeout = timeout_for_authorization(&auth);
    tokio::spawn(async move {
        monitor_kimi_login(
            deps,
            login_id,
            label,
            auth.device_code,
            poll_interval,
            timeout,
        )
        .await;
    });

    Ok(login_session)
}

async fn monitor_kimi_login(
    deps: ProviderLoginDeps,
    login_id: String,
    label: Option<String>,
    device_code: String,
    poll_interval: Duration,
    timeout: Duration,
) {
    let started_at = Instant::now();

    loop {
        if started_at.elapsed() >= timeout {
            login_sessions::set_kimi_login_timeout_if_no_error(
                deps.providers(),
                &login_id,
                "timed out waiting for Kimi sign-in completion".to_string(),
            )
            .await;
            return;
        }

        match poll_kimi_token(&device_code).await {
            Ok(Ok(token)) => {
                let added = accounts::add_kimi_oauth_account_for_login(
                    deps.data_root(),
                    deps.providers(),
                    label.clone(),
                    kimi_token_json(&token),
                    None,
                )
                .await;
                match added {
                    Ok(outcome) => {
                        let restart_error = outcome.restart_error_message();
                        login_sessions::finish_kimi_login_session(
                            deps.providers(),
                            &login_id,
                            outcome.active_account_id,
                            restart_error,
                        )
                        .await;
                    }
                    Err(err) => {
                        login_sessions::set_kimi_login_failed(
                            deps.providers(),
                            &login_id,
                            logs::redact_sensitive(&err.auth_login_error_message()),
                        )
                        .await;
                    }
                }
                return;
            }
            Ok(Err(error)) => {
                let error_code = error.error.as_deref().unwrap_or("oauth_error");
                if matches!(
                    error_code,
                    "authorization_pending" | "slow_down" | "access_denied"
                ) {
                    if error_code == "access_denied" {
                        login_sessions::set_kimi_login_failed(
                            deps.providers(),
                            &login_id,
                            error
                                .error_description
                                .unwrap_or_else(|| "Kimi sign-in was denied.".to_string()),
                        )
                        .await;
                        return;
                    }
                    tokio::time::sleep(poll_interval).await;
                    continue;
                }
                let status = if error_code == "expired_token" {
                    "timeout"
                } else {
                    "failed"
                };
                login_sessions::set_kimi_login_terminal_status(
                    deps.providers(),
                    &login_id,
                    status,
                    error
                        .error_description
                        .unwrap_or_else(|| format!("Kimi sign-in failed: {error_code}")),
                )
                .await;
                return;
            }
            Err(err) => {
                login_sessions::set_kimi_login_failed(
                    deps.providers(),
                    &login_id,
                    logs::redact_sensitive(&err.to_string()),
                )
                .await;
                return;
            }
        }
    }
}

fn poll_interval_for_authorization(auth: &KimiDeviceAuthorizationResp) -> Duration {
    auth.interval
        .filter(|value| *value > 0)
        .map(Duration::from_secs)
        .unwrap_or(KIMI_LOGIN_POLL_INTERVAL_FALLBACK)
}

fn timeout_for_authorization(auth: &KimiDeviceAuthorizationResp) -> Duration {
    auth.expires_in
        .filter(|value| *value > 0)
        .map(Duration::from_secs)
        .map(|value| value.min(kimi_login_timeout()))
        .unwrap_or_else(kimi_login_timeout)
}

fn kimi_oauth_host() -> String {
    std::env::var("KIMI_CODE_OAUTH_HOST")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::env::var("KIMI_OAUTH_HOST")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_else(|| KIMI_DEFAULT_OAUTH_HOST.to_string())
}

fn kimi_login_timeout() -> Duration {
    let seconds = std::env::var("CTX_KIMI_LOGIN_TIMEOUT_SECS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(KIMI_LOGIN_TIMEOUT_DEFAULT.as_secs());
    Duration::from_secs(seconds)
}

async fn request_kimi_device_authorization() -> anyhow::Result<KimiDeviceAuthorizationResp> {
    let url = format!(
        "{}/api/oauth/device_authorization",
        kimi_oauth_host().trim_end_matches('/')
    );
    let response = reqwest::Client::new()
        .post(&url)
        .form(&[("client_id", KIMI_CODE_CLIENT_ID)])
        .send()
        .await
        .context("requesting Kimi device authorization")?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("Kimi device authorization failed ({status}): {body}");
    }
    serde_json::from_str::<KimiDeviceAuthorizationResp>(&body)
        .context("parsing Kimi device authorization response")
}

async fn poll_kimi_token(
    device_code: &str,
) -> anyhow::Result<Result<KimiTokenSuccessResp, KimiTokenErrorResp>> {
    let url = format!(
        "{}/api/oauth/token",
        kimi_oauth_host().trim_end_matches('/')
    );
    let response = reqwest::Client::new()
        .post(&url)
        .form(&[
            ("client_id", KIMI_CODE_CLIENT_ID),
            ("device_code", device_code),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ])
        .send()
        .await
        .context("polling Kimi token endpoint")?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if status.as_u16() == 200 {
        let token = serde_json::from_str::<KimiTokenSuccessResp>(&body)
            .context("parsing Kimi token success response")?;
        return Ok(Ok(token));
    }
    if status.is_client_error() {
        let error =
            serde_json::from_str::<KimiTokenErrorResp>(&body).unwrap_or(KimiTokenErrorResp {
                error: Some("oauth_error".to_string()),
                error_description: Some(body),
            });
        return Ok(Err(error));
    }
    anyhow::bail!("Kimi token polling failed ({status}): {body}");
}

fn kimi_token_json(token: &KimiTokenSuccessResp) -> String {
    serde_json::json!({
        "access_token": token.access_token,
        "refresh_token": token.refresh_token,
        "expires_at": (Utc::now().timestamp_millis() as f64 / 1000.0) + token.expires_in,
        "scope": token.scope,
        "token_type": token.token_type,
    })
    .to_string()
}
