use super::*;
use crate::daemon::providers::login_deps::ProviderLoginDeps;

pub(super) struct CursorLoginCompletion {
    pub(super) status: String,
    pub(super) error: Option<String>,
    pub(super) account_id: Option<String>,
}

pub(super) async fn complete_cursor_login(
    deps: &ProviderLoginDeps,
    label: Option<String>,
    capture_path: &StdPath,
    observed_email: Option<String>,
    timeout_error: Option<String>,
    exit_result: std::io::Result<std::process::ExitStatus>,
) -> CursorLoginCompletion {
    let mut status = "failed".to_string();
    let mut error = timeout_error;
    let mut account_id = None;

    if error.is_none() {
        match exit_result {
            Ok(exit_status) if exit_status.success() => {
                match parse_cursor_captured_tokens(capture_path).await {
                    Ok((access_token, refresh_token, api_key)) => {
                        let auth_token = access_token.or(api_key);
                        if let Some(auth_token) = auth_token {
                            match accounts::add_cursor_oauth_account_for_login(
                                deps.data_root(),
                                deps.providers(),
                                label,
                                auth_token,
                                refresh_token,
                                observed_email,
                            )
                            .await
                            {
                                Ok(outcome) => {
                                    let restart_error = outcome.restart_error_message();
                                    account_id = outcome.active_account_id;
                                    if let Some(error_message) = restart_error.as_deref() {
                                        error = Some(logs::redact_sensitive(error_message));
                                    } else {
                                        status = "success".to_string();
                                    }
                                }
                                Err(err) => {
                                    error = Some(logs::redact_sensitive(
                                        &err.auth_login_error_message(),
                                    ));
                                }
                            }
                        } else {
                            error = Some(
                                "Cursor login completed but no managed auth token was captured"
                                    .to_string(),
                            );
                        }
                    }
                    Err(err) => {
                        error = Some(logs::redact_sensitive(&err.to_string()));
                    }
                }
            }
            Ok(exit_status) => {
                error = Some(format!(
                    "cursor-agent login exited with status {exit_status}"
                ));
            }
            Err(err) => {
                error = Some(format!("waiting for cursor-agent login failed: {err}"));
            }
        }
    }

    CursorLoginCompletion {
        status,
        error,
        account_id,
    }
}
