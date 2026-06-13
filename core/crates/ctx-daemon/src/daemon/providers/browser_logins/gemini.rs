use std::time::Instant;

use ctx_observability::logs;
use ctx_provider_runtime::ProviderRuntime;
use tokio::sync::mpsc;

use crate::daemon::providers::login_deps::ProviderLoginDeps;
use crate::daemon::providers::{accounts, login_sessions, StartedLoginSession};

use super::common;

pub async fn start_gemini_browser_login(
    deps: ProviderLoginDeps,
    label: Option<String>,
) -> StartedLoginSession {
    let session = login_sessions::start_gemini_login_session(deps.providers()).await;
    let login_id = session.login_id.clone();
    tokio::spawn(async move {
        monitor_gemini_login(deps, login_id, label).await;
    });
    session
}

async fn monitor_gemini_login(deps: ProviderLoginDeps, login_id: String, label: Option<String>) {
    let paths = match accounts::prepare_gemini_login_paths(deps.data_root(), &login_id).await {
        Ok(paths) => paths,
        Err(err) => {
            login_sessions::set_gemini_login_failed(deps.providers(), &login_id, err).await;
            return;
        }
    };
    let provider_env =
        accounts::gemini_login_provider_env(deps.data_root(), deps.daemon_url(), &paths.login_home);
    let mut event_rx = match common::authenticate_browser_login_session(
        deps.providers(),
        "gemini",
        format!("gemini-login-{login_id}"),
        paths.workdir.clone(),
        provider_env,
        Some(common::gemini_auth_method_id()),
    )
    .await
    {
        Ok(event_rx) => event_rx,
        Err(err) => {
            login_sessions::set_gemini_login_failed(deps.providers(), &login_id, err).await;
            common::cleanup_login_home(&paths.login_home).await;
            return;
        }
    };

    let started_at = Instant::now();
    let timeout = common::gemini_login_timeout();
    let mut observed_auth_url = false;

    loop {
        let event_outcome = drain_gemini_login_events(
            deps.providers(),
            &login_id,
            &mut event_rx,
            observed_auth_url,
        )
        .await;
        if event_outcome.failed {
            common::cleanup_login_home(&paths.login_home).await;
            return;
        }
        observed_auth_url = event_outcome.observed_auth_url;

        if complete_gemini_login_if_credentials_exist(&deps, &login_id, &label, &paths).await {
            return;
        }

        if event_outcome.channel_disconnected && !observed_auth_url {
            login_sessions::set_gemini_login_failed_if_no_error(
                deps.providers(),
                &login_id,
                "Gemini sign-in did not emit an OAuth URL; the runtime may require API-key auth in this environment."
                    .to_string(),
            )
            .await;
            common::cleanup_login_home(&paths.login_home).await;
            return;
        }

        if started_at.elapsed() >= timeout {
            login_sessions::set_gemini_login_timeout_if_no_error(
                deps.providers(),
                &login_id,
                "timed out waiting for Gemini OAuth completion".to_string(),
            )
            .await;
            common::cleanup_login_home(&paths.login_home).await;
            return;
        }

        tokio::time::sleep(common::GEMINI_LOGIN_POLL_INTERVAL).await;
    }
}

struct GeminiLoginEventOutcome {
    observed_auth_url: bool,
    channel_disconnected: bool,
    failed: bool,
}

async fn drain_gemini_login_events(
    providers: &ProviderRuntime,
    login_id: &str,
    event_rx: &mut mpsc::Receiver<ctx_providers::events::NormalizedEvent>,
    mut observed_auth_url: bool,
) -> GeminiLoginEventOutcome {
    let mut channel_disconnected = false;
    loop {
        match event_rx.try_recv() {
            Ok(event) => {
                if let Some(auth_url) = common::extract_auth_url_from_value(&event.payload_json) {
                    observed_auth_url = true;
                    login_sessions::set_gemini_login_auth_url(providers, login_id, auth_url).await;
                }
                if common::is_auth_failure_notice_code(common::auth_notice_code(
                    &event.payload_json,
                )) {
                    let message = event
                        .payload_json
                        .get("message")
                        .and_then(serde_json::Value::as_str)
                        .map(logs::redact_sensitive)
                        .unwrap_or_else(|| "gemini authenticate reported an error".to_string());
                    login_sessions::set_gemini_login_failed(providers, login_id, message).await;
                    return GeminiLoginEventOutcome {
                        observed_auth_url,
                        channel_disconnected,
                        failed: true,
                    };
                }
            }
            Err(mpsc::error::TryRecvError::Empty) => break,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                channel_disconnected = true;
                break;
            }
        }
    }
    GeminiLoginEventOutcome {
        observed_auth_url,
        channel_disconnected,
        failed: false,
    }
}

async fn complete_gemini_login_if_credentials_exist(
    deps: &ProviderLoginDeps,
    login_id: &str,
    label: &Option<String>,
    paths: &accounts::PreparedGeminiLoginPaths,
) -> bool {
    let Some(oauth_raw) = common::read_trimmed_file(&paths.oauth_path).await else {
        return false;
    };
    let oauth_value = serde_json::from_str::<serde_json::Value>(&oauth_raw);
    let oauth_valid = oauth_value
        .as_ref()
        .ok()
        .is_some_and(serde_json::Value::is_object);
    if !oauth_valid {
        login_sessions::set_gemini_login_failed(
            deps.providers(),
            login_id,
            "captured oauth_creds.json is not a valid JSON object".to_string(),
        )
        .await;
        common::cleanup_login_home(&paths.login_home).await;
        return true;
    }

    let google_accounts_raw = common::read_trimmed_file(&paths.google_accounts_path).await;
    let google_accounts_value = google_accounts_raw
        .as_deref()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok());
    let email = google_accounts_value
        .as_ref()
        .and_then(common::first_email_from_google_accounts);
    let added = accounts::add_gemini_account_for_login(
        deps.data_root(),
        deps.providers(),
        label.clone(),
        oauth_raw,
        google_accounts_raw,
        email,
    )
    .await;
    match added {
        Ok(outcome) => {
            let restart_error = outcome.restart_error_message();
            login_sessions::finish_gemini_login_session(
                deps.providers(),
                login_id,
                outcome.active_account_id,
                restart_error,
            )
            .await;
        }
        Err(err) => {
            login_sessions::set_gemini_login_failed(
                deps.providers(),
                login_id,
                logs::redact_sensitive(&err.auth_login_error_message()),
            )
            .await;
        }
    }
    common::cleanup_login_home(&paths.login_home).await;
    true
}
