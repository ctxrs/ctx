use std::time::Instant;

use ctx_core::models::SessionEventType;
use ctx_observability::logs;
use tokio::sync::mpsc;

use crate::daemon::providers::login_deps::ProviderLoginDeps;
use crate::daemon::providers::{accounts, login_sessions, StartedLoginSession};

use super::common;

pub async fn start_amp_browser_login(
    deps: ProviderLoginDeps,
    label: Option<String>,
) -> StartedLoginSession {
    let session = login_sessions::start_amp_login_session(deps.providers()).await;
    let login_id = session.login_id.clone();
    tokio::spawn(async move {
        monitor_amp_login(deps, login_id, label).await;
    });
    session
}

async fn monitor_amp_login(deps: ProviderLoginDeps, login_id: String, label: Option<String>) {
    let paths = match accounts::prepare_amp_login_paths(deps.data_root(), &login_id).await {
        Ok(paths) => paths,
        Err(error) => {
            login_sessions::set_amp_login_failed(deps.providers(), &login_id, error).await;
            return;
        }
    };
    let provider_env =
        accounts::amp_login_provider_env(deps.data_root(), deps.daemon_url(), &paths.amp_home);
    let mut event_rx = match common::authenticate_browser_login_session(
        deps.providers(),
        "amp",
        format!("amp-login-{login_id}"),
        paths.workdir.clone(),
        provider_env,
        Some(common::AMP_BROWSER_AUTH_METHOD_ID.to_string()),
    )
    .await
    {
        Ok(event_rx) => event_rx,
        Err(err) => {
            login_sessions::set_amp_login_failed(deps.providers(), &login_id, err).await;
            common::cleanup_login_home(&paths.login_home).await;
            return;
        }
    };

    let started_at = Instant::now();
    let timeout = common::amp_login_timeout();
    let mut progress = AmpLoginProgress::default();

    loop {
        if started_at.elapsed() >= timeout {
            login_sessions::set_amp_login_timeout_if_no_error(
                deps.providers(),
                &login_id,
                "timed out waiting for Amp OAuth completion".to_string(),
            )
            .await;
            common::cleanup_login_home(&paths.login_home).await;
            return;
        }

        match next_amp_login_event(deps.providers(), &login_id, &mut event_rx, &mut progress).await
        {
            AmpLoginEventOutcome::Pending => {}
            AmpLoginEventOutcome::Failed => {
                common::cleanup_login_home(&paths.login_home).await;
                return;
            }
            AmpLoginEventOutcome::ChannelDisconnected => {
                let message = if progress.observed_auth_url {
                    "Amp sign-in session ended before completion."
                } else {
                    "Amp sign-in did not emit an OAuth URL in this environment."
                };
                login_sessions::set_amp_login_failed_if_no_error(
                    deps.providers(),
                    &login_id,
                    message.to_string(),
                )
                .await;
                common::cleanup_login_home(&paths.login_home).await;
                return;
            }
            AmpLoginEventOutcome::Success => {
                complete_amp_login(&deps, &login_id, &label, progress.observed_email.clone()).await;
                common::cleanup_login_home(&paths.login_home).await;
                return;
            }
        }
    }
}

#[derive(Default)]
struct AmpLoginProgress {
    observed_auth_url: bool,
    observed_email: Option<String>,
}

enum AmpLoginEventOutcome {
    Pending,
    Failed,
    ChannelDisconnected,
    Success,
}

async fn next_amp_login_event(
    providers: &ctx_provider_runtime::ProviderRuntime,
    login_id: &str,
    event_rx: &mut mpsc::Receiver<ctx_providers::events::NormalizedEvent>,
    progress: &mut AmpLoginProgress,
) -> AmpLoginEventOutcome {
    let event = match tokio::time::timeout(common::AMP_LOGIN_POLL_INTERVAL, event_rx.recv()).await {
        Ok(Some(event)) => event,
        Ok(None) => return AmpLoginEventOutcome::ChannelDisconnected,
        Err(_) => return AmpLoginEventOutcome::Pending,
    };

    if let Some(auth_url) = common::extract_auth_url_from_value(&event.payload_json) {
        progress.observed_auth_url = true;
        login_sessions::set_amp_login_auth_url(providers, login_id, auth_url).await;
    }
    if progress.observed_email.is_none() {
        progress.observed_email = common::first_email_from_value(&event.payload_json);
    }

    if common::is_auth_failure_notice_code(common::auth_notice_code(&event.payload_json)) {
        let message = event
            .payload_json
            .get("message")
            .and_then(serde_json::Value::as_str)
            .map(logs::redact_sensitive)
            .unwrap_or_else(|| "amp authenticate reported an error".to_string());
        login_sessions::set_amp_login_failed(providers, login_id, message).await;
        return AmpLoginEventOutcome::Failed;
    }

    if matches!(event.event_type, SessionEventType::Notice) {
        let code = common::auth_notice_code(&event.payload_json);
        if common::is_auth_success_notice_code(code) {
            return AmpLoginEventOutcome::Success;
        }
        if matches!(code, "auth_failed" | "auth_error" | "auth_required") {
            let message = event
                .payload_json
                .get("message")
                .and_then(serde_json::Value::as_str)
                .map(logs::redact_sensitive)
                .unwrap_or_else(|| "Amp sign-in failed. Retry.".to_string());
            login_sessions::set_amp_login_failed(providers, login_id, message).await;
            return AmpLoginEventOutcome::Failed;
        }
    }

    AmpLoginEventOutcome::Pending
}

async fn complete_amp_login(
    deps: &ProviderLoginDeps,
    login_id: &str,
    label: &Option<String>,
    observed_email: Option<String>,
) {
    match accounts::upsert_amp_account_for_login(
        deps.data_root(),
        deps.providers(),
        label.clone(),
        observed_email,
    )
    .await
    {
        Ok(outcome) => {
            let (_, restart_result) = outcome.into_restart_result();
            login_sessions::finish_amp_login_session(deps.providers(), login_id, restart_result)
                .await;
        }
        Err(err) => {
            login_sessions::set_amp_login_failed(
                deps.providers(),
                login_id,
                logs::redact_sensitive(&err.auth_login_error_message()),
            )
            .await;
        }
    }
}
