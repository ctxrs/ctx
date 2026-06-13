use super::*;
use crate::daemon::providers::login_deps::ProviderLoginDeps;
use crate::daemon::providers::{accounts, login_sessions};
use ctx_observability::logs;

fn format_claude_exit_status(status: &portable_pty::ExitStatus) -> String {
    if let Some(signal) = status.signal() {
        return format!("signal {signal}");
    }
    status.exit_code().to_string()
}

pub(super) async fn finalize_claude_login(
    deps: &ProviderLoginDeps,
    login_id: &str,
    label: Option<String>,
    observed_auth_url: Option<String>,
    terminal_error: Option<String>,
    exit_result: Option<anyhow::Result<portable_pty::ExitStatus>>,
    transcript: &str,
) {
    let mut final_status = "failed".to_string();
    let mut final_error = terminal_error;
    let mut final_account_id: Option<String> = None;

    if final_error.is_none() {
        match exit_result {
            Some(Ok(exit)) if exit.success() => match extract_claude_setup_token(transcript) {
                Some(setup_token) => {
                    match accounts::add_claude_account_for_login(
                        deps.data_root(),
                        deps.providers(),
                        label,
                        setup_token,
                    )
                    .await
                    {
                        Ok(outcome) => {
                            let restart_error = outcome.restart_error_message();
                            final_account_id = outcome.active_account_id;
                            if let Some(error) = restart_error {
                                final_error = Some(logs::redact_sensitive(&error));
                            } else {
                                final_status = "success".to_string();
                            }
                        }
                        Err(err) => {
                            final_error =
                                Some(logs::redact_sensitive(&err.auth_login_error_message()));
                        }
                    }
                }
                None => {
                    final_error = Some(
                        "claude setup-token completed but no setup token was detected".to_string(),
                    );
                }
            },
            Some(Ok(exit)) => {
                final_error = Some(format!(
                    "claude setup-token exited with status {}",
                    format_claude_exit_status(&exit)
                ));
            }
            Some(Err(err)) => {
                final_error = Some(format!("waiting for claude setup-token failed: {err}"));
            }
            None => {
                final_error = Some(
                    "claude setup-token monitor ended before process exit was observed".to_string(),
                );
            }
        }
    }

    login_sessions::finish_claude_login_session(
        deps.providers(),
        login_id,
        final_status,
        final_account_id,
        final_error,
        observed_auth_url,
    )
    .await;
}
