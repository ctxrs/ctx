use ctx_observability::logs;
use ctx_provider_accounts as provider_accounts;
use ctx_provider_runtime::ProviderRuntime;

use super::{new_started_login_session, StartedLoginSession};

fn restart_failure_message(error: anyhow::Error) -> String {
    logs::redact_sensitive(&format!(
        "auth saved but provider restart failed: {error:#}"
    ))
}

macro_rules! auth_account_login_session_helpers {
    (
        $start:ident,
        $status:ident,
        $set_failed:ident,
        $set_failed_if_no_error:ident,
        $set_timeout_if_no_error:ident,
        $set_auth_url:ident,
        $with:ident,
        $status_ty:ty
    ) => {
        pub async fn $start(providers: &ProviderRuntime) -> StartedLoginSession {
            type LoginStatus = $status_ty;
            let session = new_started_login_session(None, None);
            providers
                .$with(|map| {
                    map.insert(
                        session.login_id.clone(),
                        LoginStatus {
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

        pub async fn $status(providers: &ProviderRuntime, login_id: &str) -> Option<$status_ty> {
            providers.$with(|map| map.get(login_id).cloned()).await
        }

        pub async fn $set_failed(providers: &ProviderRuntime, login_id: &str, error: String) {
            providers
                .$with(|map| {
                    if let Some(entry) = map.get_mut(login_id) {
                        entry.status = "failed".to_string();
                        entry.error = Some(error);
                    }
                })
                .await;
        }

        pub async fn $set_failed_if_no_error(
            providers: &ProviderRuntime,
            login_id: &str,
            error: String,
        ) {
            providers
                .$with(|map| {
                    if let Some(entry) = map.get_mut(login_id) {
                        entry.status = "failed".to_string();
                        if entry.error.is_none() {
                            entry.error = Some(error);
                        }
                    }
                })
                .await;
        }

        pub async fn $set_timeout_if_no_error(
            providers: &ProviderRuntime,
            login_id: &str,
            error: String,
        ) {
            providers
                .$with(|map| {
                    if let Some(entry) = map.get_mut(login_id) {
                        entry.status = "timeout".to_string();
                        if entry.error.is_none() {
                            entry.error = Some(error);
                        }
                    }
                })
                .await;
        }

        pub async fn $set_auth_url(providers: &ProviderRuntime, login_id: &str, auth_url: String) {
            providers
                .$with(|map| {
                    if let Some(entry) = map.get_mut(login_id) {
                        entry.auth_url = Some(auth_url);
                    }
                })
                .await;
        }
    };
}

macro_rules! auth_only_login_session_helpers {
    (
        $start:ident,
        $status:ident,
        $set_failed:ident,
        $set_failed_if_no_error:ident,
        $set_timeout_if_no_error:ident,
        $set_auth_url:ident,
        $with:ident,
        $status_ty:ty
    ) => {
        pub async fn $start(providers: &ProviderRuntime) -> StartedLoginSession {
            type LoginStatus = $status_ty;
            let session = new_started_login_session(None, None);
            providers
                .$with(|map| {
                    map.insert(
                        session.login_id.clone(),
                        LoginStatus {
                            login_id: session.login_id.clone(),
                            auth_url: session.auth_url.clone(),
                            status: "pending".to_string(),
                            error: None,
                        },
                    );
                })
                .await;
            session
        }

        pub async fn $status(providers: &ProviderRuntime, login_id: &str) -> Option<$status_ty> {
            providers.$with(|map| map.get(login_id).cloned()).await
        }

        pub async fn $set_failed(providers: &ProviderRuntime, login_id: &str, error: String) {
            providers
                .$with(|map| {
                    if let Some(entry) = map.get_mut(login_id) {
                        entry.status = "failed".to_string();
                        entry.error = Some(error);
                    }
                })
                .await;
        }

        pub async fn $set_failed_if_no_error(
            providers: &ProviderRuntime,
            login_id: &str,
            error: String,
        ) {
            providers
                .$with(|map| {
                    if let Some(entry) = map.get_mut(login_id) {
                        entry.status = "failed".to_string();
                        if entry.error.is_none() {
                            entry.error = Some(error);
                        }
                    }
                })
                .await;
        }

        pub async fn $set_timeout_if_no_error(
            providers: &ProviderRuntime,
            login_id: &str,
            error: String,
        ) {
            providers
                .$with(|map| {
                    if let Some(entry) = map.get_mut(login_id) {
                        entry.status = "timeout".to_string();
                        if entry.error.is_none() {
                            entry.error = Some(error);
                        }
                    }
                })
                .await;
        }

        pub async fn $set_auth_url(providers: &ProviderRuntime, login_id: &str, auth_url: String) {
            providers
                .$with(|map| {
                    if let Some(entry) = map.get_mut(login_id) {
                        entry.auth_url = Some(auth_url);
                    }
                })
                .await;
        }
    };
}

auth_account_login_session_helpers!(
    start_gemini_login_session,
    gemini_login_status,
    set_gemini_login_failed,
    set_gemini_login_failed_if_no_error,
    set_gemini_login_timeout_if_no_error,
    set_gemini_login_auth_url,
    with_gemini_login_sessions,
    provider_accounts::GeminiLoginStatus
);

auth_account_login_session_helpers!(
    start_qwen_login_session,
    qwen_login_status,
    set_qwen_login_failed,
    set_qwen_login_failed_if_no_error,
    set_qwen_login_timeout_if_no_error,
    set_qwen_login_auth_url,
    with_qwen_login_sessions,
    provider_accounts::QwenLoginStatus
);

auth_only_login_session_helpers!(
    start_amp_login_session,
    amp_login_status,
    set_amp_login_failed,
    set_amp_login_failed_if_no_error,
    set_amp_login_timeout_if_no_error,
    set_amp_login_auth_url,
    with_amp_login_sessions,
    provider_accounts::AmpLoginStatus
);

auth_only_login_session_helpers!(
    start_mistral_login_session,
    mistral_login_status,
    set_mistral_login_failed,
    set_mistral_login_failed_if_no_error,
    set_mistral_login_timeout_if_no_error,
    set_mistral_login_auth_url,
    with_mistral_login_sessions,
    provider_accounts::MistralLoginStatus
);

pub async fn finish_gemini_login_session(
    providers: &ProviderRuntime,
    login_id: &str,
    account_id: Option<String>,
    restart_error: Option<String>,
) {
    providers
        .with_gemini_login_sessions(|map| {
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

pub async fn finish_qwen_login_session(
    providers: &ProviderRuntime,
    login_id: &str,
    account_id: Option<String>,
    restart_result: anyhow::Result<()>,
) {
    providers
        .with_qwen_login_sessions(|map| {
            if let Some(entry) = map.get_mut(login_id) {
                entry.account_id = account_id;
                match restart_result {
                    Ok(()) => {
                        entry.status = "success".to_string();
                        entry.error = None;
                    }
                    Err(err) => {
                        entry.status = "failed".to_string();
                        entry.error = Some(restart_failure_message(err));
                    }
                }
            }
        })
        .await;
}

pub async fn finish_amp_login_session(
    providers: &ProviderRuntime,
    login_id: &str,
    restart_result: anyhow::Result<()>,
) {
    providers
        .with_amp_login_sessions(|map| {
            if let Some(entry) = map.get_mut(login_id) {
                entry.auth_url = None;
                match restart_result {
                    Ok(()) => {
                        entry.status = "success".to_string();
                        entry.error = None;
                    }
                    Err(err) => {
                        entry.status = "failed".to_string();
                        entry.error = Some(restart_failure_message(err));
                    }
                }
            }
        })
        .await;
}

pub async fn finish_mistral_login_session(
    providers: &ProviderRuntime,
    login_id: &str,
    restart_result: anyhow::Result<()>,
) {
    providers
        .with_mistral_login_sessions(|map| {
            if let Some(entry) = map.get_mut(login_id) {
                entry.auth_url = None;
                match restart_result {
                    Ok(()) => {
                        entry.status = "success".to_string();
                        entry.error = None;
                    }
                    Err(err) => {
                        entry.status = "failed".to_string();
                        entry.error = Some(restart_failure_message(err));
                    }
                }
            }
        })
        .await;
}
