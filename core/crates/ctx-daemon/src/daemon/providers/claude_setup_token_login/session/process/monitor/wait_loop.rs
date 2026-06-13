use std::time::Instant;

use super::super::{
    ClaudeLoginProcess, CLAUDE_LOGIN_COMPLETION_TIMEOUT, CLAUDE_LOGIN_NO_AUTH_URL_TIMEOUT,
};
use super::output::{
    drain_claude_login_output, observe_claude_login_line, ClaudeLoginOutputDrainMode,
};
use crate::daemon::providers::claude_setup_token_login::auth_url::refresh_claude_auth_url_from_capture_path;
use ctx_provider_runtime::ProviderRuntime;

pub(super) struct ClaudeLoginObservation {
    pub(super) observed_auth_url: Option<String>,
    pub(super) transcript: String,
    pub(super) terminal_error: Option<String>,
    pub(super) exit_result: Option<anyhow::Result<portable_pty::ExitStatus>>,
}

pub(super) async fn wait_for_claude_login_observation(
    providers: &ProviderRuntime,
    login_id: &str,
    login: &mut ClaudeLoginProcess,
) -> ClaudeLoginObservation {
    let mut transcript = String::new();
    let mut observed_auth_url = login.auth_url.clone();
    let mut output_closed = false;
    let auth_url_deadline = Instant::now() + CLAUDE_LOGIN_NO_AUTH_URL_TIMEOUT;
    let mut completion_deadline = observed_auth_url
        .as_ref()
        .map(|_| Instant::now() + CLAUDE_LOGIN_COMPLETION_TIMEOUT);
    let mut exit_result: Option<anyhow::Result<portable_pty::ExitStatus>> = None;
    let mut terminal_error: Option<String> = None;

    for line in std::mem::take(&mut login.buffered_lines) {
        let outcome = observe_claude_login_line(
            providers,
            login_id,
            &mut observed_auth_url,
            &mut transcript,
            &login.browser_open_capture_path,
            line,
        )
        .await;
        if let Some(error) = outcome.terminal_error {
            terminal_error = Some(error);
            break;
        }
        if outcome.auth_url_became_observed {
            completion_deadline = Some(Instant::now() + CLAUDE_LOGIN_COMPLETION_TIMEOUT);
        }
    }

    while terminal_error.is_none() {
        let _ = refresh_claude_auth_url_from_capture_path(
            &mut observed_auth_url,
            &login.browser_open_capture_path,
        );
        let deadline = completion_deadline.unwrap_or(auth_url_deadline);
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            terminal_error = Some(timeout_error_message(observed_auth_url.is_some()));
            break;
        }
        let timeout_future = tokio::time::sleep(remaining);
        tokio::pin!(timeout_future);

        tokio::select! {
            maybe_line = login.line_rx.recv(), if !output_closed => {
                match maybe_line {
                    Some(line) => {
                        let outcome = observe_claude_login_line(
                            providers,
                            login_id,
                            &mut observed_auth_url,
                            &mut transcript,
                            &login.browser_open_capture_path,
                            line,
                        )
                        .await;
                        if let Some(error) = outcome.terminal_error {
                            terminal_error = Some(error);
                            break;
                        }
                        if outcome.auth_url_became_observed {
                            completion_deadline = Some(Instant::now() + CLAUDE_LOGIN_COMPLETION_TIMEOUT);
                        }
                    }
                    None => {
                        output_closed = true;
                    }
                }
            }
            exit = &mut login.exit_rx => {
                exit_result = Some(match exit {
                    Ok(result) => result,
                    Err(err) => Err(anyhow::anyhow!("claude setup-token exit channel closed: {err}")),
                });
                break;
            }
            _ = &mut timeout_future => {
                terminal_error = Some(timeout_error_message(observed_auth_url.is_some()));
                break;
            }
        }
    }

    let drain_mode = if exit_result.is_some() {
        ClaudeLoginOutputDrainMode::TrailingGrace
    } else {
        ClaudeLoginOutputDrainMode::PendingOnly
    };
    drain_claude_login_output(
        providers,
        login_id,
        &mut observed_auth_url,
        &mut transcript,
        &login.browser_open_capture_path,
        &mut login.line_rx,
        drain_mode,
    )
    .await;

    ClaudeLoginObservation {
        observed_auth_url,
        transcript,
        terminal_error,
        exit_result,
    }
}

fn timeout_error_message(has_auth_url: bool) -> String {
    if has_auth_url {
        "claude setup-token timed out waiting for browser sign-in completion".to_string()
    } else {
        "claude setup-token did not emit an authentication URL".to_string()
    }
}
