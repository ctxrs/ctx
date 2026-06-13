use url::Url;

mod common;

#[path = "auth_url/setup_token.rs"]
mod setup_token;

pub(in crate::daemon::providers::claude_setup_token_login) use common::{
    auth_url_looks_complete, extract_auth_url, normalize_claude_login_line,
    read_trailing_claude_login_lines,
};
pub(in crate::daemon::providers::claude_setup_token_login) use setup_token::extract_claude_setup_token;

pub(super) const CLAUDE_BROWSER_OPEN_MARKER: &str = "CTX_CLAUDE_AUTH_URL:";
pub(super) const CLAUDE_UNSUPPORTED_MANUAL_FALLBACK_ERROR: &str =
    "Claude setup-token fell back to manual code entry, which ctx does not support. Browser launch likely failed before Claude could receive the localhost callback.";

pub(super) fn claude_login_hit_unsupported_manual_fallback(text: &str) -> bool {
    text.to_ascii_lowercase()
        .contains("browser didn't open? use the url below to sign in")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ClaudeAuthUrlSource {
    BrowserOpenCapture,
    BrowserOpenMarker,
    Transcript,
}

fn extract_claude_browser_open_marker_url(text: &str) -> Option<String> {
    for raw_line in text.lines() {
        let line = raw_line.trim();
        let Some(marker_idx) = line.find(CLAUDE_BROWSER_OPEN_MARKER) else {
            continue;
        };
        let candidate = line[marker_idx + CLAUDE_BROWSER_OPEN_MARKER.len()..].trim();
        if candidate.is_empty() {
            continue;
        }
        if Url::parse(candidate).is_ok() {
            return Some(candidate.to_string());
        }
        if let Some(parsed) = extract_auth_url(candidate) {
            return Some(parsed);
        }
    }
    None
}

pub(super) fn extract_preferred_claude_auth_url(
    text: &str,
) -> Option<(String, ClaudeAuthUrlSource)> {
    if let Some(marker_url) = extract_claude_browser_open_marker_url(text) {
        return Some((marker_url, ClaudeAuthUrlSource::BrowserOpenMarker));
    }
    extract_auth_url(text).map(|value| (value, ClaudeAuthUrlSource::Transcript))
}

pub(super) fn should_replace_observed_claude_auth_url(
    current: Option<&str>,
    candidate: &str,
    source: ClaudeAuthUrlSource,
) -> bool {
    match source {
        ClaudeAuthUrlSource::BrowserOpenCapture | ClaudeAuthUrlSource::BrowserOpenMarker => {
            current != Some(candidate)
        }
        ClaudeAuthUrlSource::Transcript => match current {
            None => true,
            Some(existing) => {
                !auth_url_looks_complete(existing) && candidate.len() >= existing.len()
            }
        },
    }
}

pub(super) fn read_claude_browser_open_capture_url(path: &std::path::Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let candidate = raw.trim();
    if candidate.is_empty() {
        return None;
    }
    if Url::parse(candidate).is_ok() {
        return Some(candidate.to_string());
    }
    extract_auth_url(candidate)
}

pub(super) fn claude_manual_fallback_is_terminal(
    text: &str,
    browser_open_capture_path: &std::path::Path,
) -> bool {
    claude_login_hit_unsupported_manual_fallback(text)
        && read_claude_browser_open_capture_url(browser_open_capture_path).is_none()
}

pub(super) fn refresh_claude_auth_url_from_capture_path(
    observed_auth_url: &mut Option<String>,
    capture_path: &std::path::Path,
) -> bool {
    let Some(candidate) = read_claude_browser_open_capture_url(capture_path) else {
        return false;
    };
    if should_replace_observed_claude_auth_url(
        observed_auth_url.as_deref(),
        &candidate,
        ClaudeAuthUrlSource::BrowserOpenCapture,
    ) {
        *observed_auth_url = Some(candidate);
        return true;
    }
    false
}
