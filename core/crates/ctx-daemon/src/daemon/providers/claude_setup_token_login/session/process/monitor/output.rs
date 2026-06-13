use std::path::Path;

use super::super::line_observation::append_claude_login_line;
use super::*;
use ctx_provider_runtime::ProviderRuntime;

pub(super) struct ClaudeLoginLineOutcome {
    pub(super) auth_url_became_observed: bool,
    pub(super) terminal_error: Option<String>,
}

pub(super) enum ClaudeLoginOutputDrainMode {
    TrailingGrace,
    PendingOnly,
}

pub(super) async fn observe_claude_login_line(
    providers: &ProviderRuntime,
    login_id: &str,
    observed_auth_url: &mut Option<String>,
    transcript: &mut String,
    browser_open_capture_path: &Path,
    line: String,
) -> ClaudeLoginLineOutcome {
    let had_auth_url = observed_auth_url.is_some();
    append_and_refresh_claude_login_line(
        providers,
        login_id,
        observed_auth_url,
        transcript,
        browser_open_capture_path,
        line,
    )
    .await;
    ClaudeLoginLineOutcome {
        auth_url_became_observed: !had_auth_url && observed_auth_url.is_some(),
        terminal_error: claude_manual_fallback_is_terminal(transcript, browser_open_capture_path)
            .then(|| CLAUDE_UNSUPPORTED_MANUAL_FALLBACK_ERROR.to_string()),
    }
}

pub(super) async fn drain_claude_login_output(
    providers: &ProviderRuntime,
    login_id: &str,
    observed_auth_url: &mut Option<String>,
    transcript: &mut String,
    browser_open_capture_path: &Path,
    line_rx: &mut mpsc::UnboundedReceiver<String>,
    mode: ClaudeLoginOutputDrainMode,
) {
    match mode {
        ClaudeLoginOutputDrainMode::TrailingGrace => {
            for line in
                read_trailing_claude_login_lines(line_rx, CLAUDE_LOGIN_EXIT_GRACE_WAIT).await
            {
                append_and_refresh_claude_login_line(
                    providers,
                    login_id,
                    observed_auth_url,
                    transcript,
                    browser_open_capture_path,
                    line,
                )
                .await;
            }
        }
        ClaudeLoginOutputDrainMode::PendingOnly => {
            while let Ok(line) = line_rx.try_recv() {
                append_and_refresh_claude_login_line(
                    providers,
                    login_id,
                    observed_auth_url,
                    transcript,
                    browser_open_capture_path,
                    line,
                )
                .await;
            }
        }
    }
}

async fn append_and_refresh_claude_login_line(
    providers: &ProviderRuntime,
    login_id: &str,
    observed_auth_url: &mut Option<String>,
    transcript: &mut String,
    browser_open_capture_path: &Path,
    line: String,
) {
    append_claude_login_line(providers, login_id, observed_auth_url, transcript, line).await;
    let _ = refresh_claude_auth_url_from_capture_path(observed_auth_url, browser_open_capture_path);
}
