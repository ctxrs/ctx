use super::*;
use crate::daemon::providers::login_sessions;
use ctx_provider_runtime::ProviderRuntime;

pub(super) async fn append_claude_login_line(
    providers: &ProviderRuntime,
    login_id: &str,
    observed_auth_url: &mut Option<String>,
    transcript: &mut String,
    line: String,
) {
    let needs_auth_url_upgrade = match observed_auth_url.as_deref() {
        None => true,
        Some(url) => !auth_url_looks_complete(url),
    };
    if needs_auth_url_upgrade {
        if let Some((candidate, source)) = extract_preferred_claude_auth_url(&line) {
            if should_replace_observed_claude_auth_url(
                observed_auth_url.as_deref(),
                &candidate,
                source,
            ) {
                *observed_auth_url = Some(candidate);
            }
        }
    }
    transcript.push_str(&line);
    transcript.push('\n');
    let needs_auth_url_upgrade = match observed_auth_url.as_deref() {
        None => true,
        Some(url) => !auth_url_looks_complete(url),
    };
    if needs_auth_url_upgrade {
        if let Some((candidate, source)) = extract_preferred_claude_auth_url(transcript) {
            if should_replace_observed_claude_auth_url(
                observed_auth_url.as_deref(),
                &candidate,
                source,
            ) {
                *observed_auth_url = Some(candidate);
            }
        }
    }
    if let Some(url) = observed_auth_url.clone() {
        login_sessions::set_claude_login_auth_url(providers, login_id, url).await;
    }
}
