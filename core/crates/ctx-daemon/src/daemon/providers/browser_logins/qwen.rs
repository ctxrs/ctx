use std::time::Instant;

use ctx_observability::logs;
use ctx_provider_runtime::ProviderRuntime;
use tokio::sync::mpsc;

use crate::daemon::providers::login_deps::ProviderLoginDeps;
use crate::daemon::providers::{accounts, login_sessions, StartedLoginSession};

use super::common;

pub async fn start_qwen_browser_login(
    deps: ProviderLoginDeps,
    label: Option<String>,
) -> StartedLoginSession {
    let session = login_sessions::start_qwen_login_session(deps.providers()).await;
    let login_id = session.login_id.clone();
    tokio::spawn(async move {
        monitor_qwen_login(deps, login_id, label).await;
    });
    session
}

async fn monitor_qwen_login(deps: ProviderLoginDeps, login_id: String, label: Option<String>) {
    let paths = match accounts::prepare_qwen_login_paths(deps.data_root(), &login_id).await {
        Ok(paths) => paths,
        Err(error) => {
            login_sessions::set_qwen_login_failed(deps.providers(), &login_id, error).await;
            return;
        }
    };
    let provider_env =
        accounts::qwen_login_provider_env(deps.data_root(), deps.daemon_url(), &paths.login_home);
    let mut event_rx = match common::authenticate_browser_login_session(
        deps.providers(),
        "qwen",
        format!("qwen-login-{login_id}"),
        paths.workdir.clone(),
        provider_env,
        Some(common::QWEN_OAUTH_AUTH_METHOD_ID.to_string()),
    )
    .await
    {
        Ok(event_rx) => event_rx,
        Err(err) => {
            login_sessions::set_qwen_login_failed(deps.providers(), &login_id, err).await;
            common::cleanup_login_home(&paths.login_home).await;
            return;
        }
    };

    let started_at = Instant::now();
    let timeout = common::qwen_login_timeout();
    let mut progress = QwenLoginProgress::default();

    loop {
        let event_outcome =
            drain_qwen_login_events(deps.providers(), &login_id, &mut event_rx, &mut progress)
                .await;
        if event_outcome.failed {
            common::cleanup_login_home(&paths.login_home).await;
            return;
        }

        if complete_qwen_login_if_credentials_exist(
            &deps,
            &login_id,
            &label,
            &paths,
            progress.observed_email.clone(),
        )
        .await
        {
            return;
        }

        if event_outcome.channel_disconnected && !progress.observed_auth_url {
            login_sessions::set_qwen_login_failed_if_no_error(
                deps.providers(),
                &login_id,
                "Qwen sign-in did not emit an OAuth URL in this environment.".to_string(),
            )
            .await;
            common::cleanup_login_home(&paths.login_home).await;
            return;
        }

        if started_at.elapsed() >= timeout {
            login_sessions::set_qwen_login_timeout_if_no_error(
                deps.providers(),
                &login_id,
                "timed out waiting for Qwen OAuth completion".to_string(),
            )
            .await;
            common::cleanup_login_home(&paths.login_home).await;
            return;
        }

        tokio::time::sleep(common::QWEN_LOGIN_POLL_INTERVAL).await;
    }
}

#[derive(Default)]
struct QwenLoginProgress {
    observed_auth_url: bool,
    observed_email: Option<String>,
}

struct QwenLoginEventOutcome {
    channel_disconnected: bool,
    failed: bool,
}

async fn drain_qwen_login_events(
    providers: &ProviderRuntime,
    login_id: &str,
    event_rx: &mut mpsc::Receiver<ctx_providers::events::NormalizedEvent>,
    progress: &mut QwenLoginProgress,
) -> QwenLoginEventOutcome {
    let mut channel_disconnected = false;
    loop {
        match event_rx.try_recv() {
            Ok(event) => {
                if let Some(auth_url) = common::extract_auth_url_from_value(&event.payload_json) {
                    progress.observed_auth_url = true;
                    login_sessions::set_qwen_login_auth_url(providers, login_id, auth_url).await;
                }
                if progress.observed_email.is_none() {
                    progress.observed_email = common::first_email_from_value(&event.payload_json);
                }
                if common::is_auth_failure_notice_code(common::auth_notice_code(
                    &event.payload_json,
                )) {
                    let message = event
                        .payload_json
                        .get("message")
                        .and_then(serde_json::Value::as_str)
                        .map(logs::redact_sensitive)
                        .unwrap_or_else(|| "qwen authenticate reported an error".to_string());
                    login_sessions::set_qwen_login_failed(providers, login_id, message).await;
                    return QwenLoginEventOutcome {
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

    QwenLoginEventOutcome {
        channel_disconnected,
        failed: false,
    }
}

async fn complete_qwen_login_if_credentials_exist(
    deps: &ProviderLoginDeps,
    login_id: &str,
    label: &Option<String>,
    paths: &accounts::PreparedQwenLoginPaths,
    observed_email: Option<String>,
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
        login_sessions::set_qwen_login_failed(
            deps.providers(),
            login_id,
            "captured oauth_creds.json is not a valid JSON object".to_string(),
        )
        .await;
        common::cleanup_login_home(&paths.login_home).await;
        return true;
    }

    let added = accounts::add_qwen_account_for_login(
        deps.data_root(),
        deps.providers(),
        label.clone(),
        oauth_raw,
        observed_email,
    )
    .await;
    match added {
        Ok(outcome) => {
            let (active_account_id, restart_result) = outcome.into_restart_result();
            login_sessions::finish_qwen_login_session(
                deps.providers(),
                login_id,
                active_account_id,
                restart_result,
            )
            .await;
        }
        Err(err) => {
            login_sessions::set_qwen_login_failed(
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
