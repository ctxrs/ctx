use std::collections::HashMap;

use ctx_provider_accounts as provider_accounts;

use crate::ProviderRuntime;

macro_rules! login_session_accessors {
    ($(($method:ident, $field:ident, $status:ty)),+ $(,)?) => {
        impl ProviderRuntime {
            $(
                pub async fn $method<R>(
                    &self,
                    f: impl FnOnce(&mut HashMap<String, $status>) -> R,
                ) -> R {
                    let mut sessions = self.$field.lock().await;
                    f(&mut sessions)
                }
            )+
        }
    };
}

login_session_accessors!(
    (
        with_codex_login_sessions,
        codex_login_sessions,
        provider_accounts::CodexLoginStatus
    ),
    (
        with_claude_login_sessions,
        claude_login_sessions,
        provider_accounts::ClaudeLoginStatus
    ),
    (
        with_gemini_login_sessions,
        gemini_login_sessions,
        provider_accounts::GeminiLoginStatus
    ),
    (
        with_qwen_login_sessions,
        qwen_login_sessions,
        provider_accounts::QwenLoginStatus
    ),
    (
        with_kimi_login_sessions,
        kimi_login_sessions,
        provider_accounts::KimiLoginStatus
    ),
    (
        with_cursor_login_sessions,
        cursor_login_sessions,
        provider_accounts::CursorLoginStatus
    ),
    (
        with_amp_login_sessions,
        amp_login_sessions,
        provider_accounts::AmpLoginStatus
    ),
    (
        with_mistral_login_sessions,
        mistral_login_sessions,
        provider_accounts::MistralLoginStatus
    ),
);
