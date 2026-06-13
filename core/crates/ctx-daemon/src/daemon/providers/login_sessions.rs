mod browser;
mod codex;
mod interactive;

pub use browser::{
    amp_login_status, finish_amp_login_session, finish_gemini_login_session,
    finish_mistral_login_session, finish_qwen_login_session, gemini_login_status,
    mistral_login_status, qwen_login_status, set_amp_login_auth_url, set_amp_login_failed,
    set_amp_login_failed_if_no_error, set_amp_login_timeout_if_no_error, set_gemini_login_auth_url,
    set_gemini_login_failed, set_gemini_login_failed_if_no_error,
    set_gemini_login_timeout_if_no_error, set_mistral_login_auth_url, set_mistral_login_failed,
    set_mistral_login_failed_if_no_error, set_mistral_login_timeout_if_no_error,
    set_qwen_login_auth_url, set_qwen_login_failed, set_qwen_login_failed_if_no_error,
    set_qwen_login_timeout_if_no_error, start_amp_login_session, start_gemini_login_session,
    start_mistral_login_session, start_qwen_login_session,
};
pub use codex::{
    claim_codex_login_callback, codex_login_status, codex_login_statuses,
    finish_codex_login_session, remove_codex_login_session, restore_codex_login_completion_token,
    start_codex_login_session, CodexLoginCallbackClaimError, StartedCodexLoginSession,
};
pub use interactive::{
    claude_login_status, cursor_login_status, finish_claude_login_session,
    finish_cursor_login_session, finish_kimi_login_session, kimi_login_status,
    set_claude_login_auth_url, set_cursor_login_error, set_kimi_login_failed,
    set_kimi_login_terminal_status, set_kimi_login_timeout_if_no_error, start_claude_login_session,
    start_cursor_login_session, start_kimi_login_session, update_cursor_login_auth_url,
};

#[derive(Debug)]
pub struct StartedLoginSession {
    pub login_id: String,
    pub auth_url: Option<String>,
    pub device_code: Option<String>,
}

pub(super) fn new_started_login_session(
    auth_url: Option<String>,
    device_code: Option<String>,
) -> StartedLoginSession {
    StartedLoginSession {
        login_id: uuid::Uuid::new_v4().to_string(),
        auth_url,
        device_code,
    }
}
