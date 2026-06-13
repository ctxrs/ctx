use super::super::auth_url::extract_auth_url;
use super::super::output::{first_email_from_text, CursorLoginOutputLine};
use super::*;
use ctx_provider_runtime::ProviderRuntime;

async fn update_cursor_auth_url(providers: &ProviderRuntime, login_id: &str, auth_url: String) {
    login_sessions::update_cursor_login_auth_url(providers, login_id, auth_url).await;
}

pub(super) async fn record_cursor_login_output(
    providers: &ProviderRuntime,
    login_id: &str,
    output_line: CursorLoginOutputLine,
    transcript: &mut String,
    observed_email: &mut Option<String>,
    observed_auth_url: &mut Option<String>,
) {
    transcript.push_str(&output_line.line);
    transcript.push('\n');
    if !output_line.is_stderr && observed_email.is_none() {
        *observed_email = first_email_from_text(&output_line.line);
    }
    if let Some(candidate) =
        extract_auth_url(&output_line.line).or_else(|| extract_auth_url(transcript))
    {
        let needs_update = observed_auth_url
            .as_ref()
            .is_none_or(|current| candidate.len() > current.len());
        if needs_update {
            *observed_auth_url = Some(candidate.clone());
            update_cursor_auth_url(providers, login_id, candidate).await;
        }
    }
}
