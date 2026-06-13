use std::process::Command;
use std::time::Duration;

use super::auth_url::{
    auth_url_looks_complete, extract_auth_url, extract_preferred_claude_auth_url,
    normalize_claude_login_line, read_claude_browser_open_capture_url,
    read_trailing_claude_login_lines, should_replace_observed_claude_auth_url, ClaudeAuthUrlSource,
    CLAUDE_BROWSER_OPEN_MARKER,
};
use super::runtime::{
    claude_browser_open_shim_script, claude_login_should_skip_browser_open,
    CLAUDE_BROWSER_AUTH_TIER,
};
use tokio::sync::mpsc;

#[test]
fn extract_auth_url_detects_urls_in_line() {
    let line = "Open this URL to continue: https://claude.ai/oauth/authorize?foo=bar";
    assert_eq!(
        extract_auth_url(line).as_deref(),
        Some("https://claude.ai/oauth/authorize?foo=bar")
    );
}

#[test]
fn extract_auth_url_detects_urls_in_browser_open_marker_line() {
    let line = "CTX_CLAUDE_AUTH_URL:https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A64111%2Fcallback&state=abc";
    assert_eq!(
        extract_auth_url(line).as_deref(),
        Some(
            "https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A64111%2Fcallback&state=abc"
        )
    );
}

#[test]
fn extract_auth_url_reconstructs_wrapped_url_lines() {
    let wrapped =
        "Open this URL: https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A\n64111%2Fauth%2Fcallback&state=abc";
    assert_eq!(
        extract_auth_url(wrapped).as_deref(),
        Some(
            "https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A64111%2Fauth%2Fcallback&state=abc"
        )
    );
}

#[test]
fn extract_auth_url_stops_before_duplicate_full_url() {
    let url = "https://claude.ai/oauth/authorize?code=true&client_id=test&response_type=code&redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback&scope=user%3Ainference&state=abc";
    let duplicated = format!("{url} {url}");
    assert_eq!(extract_auth_url(&duplicated).as_deref(), Some(url));
}

#[test]
fn normalize_claude_login_line_handles_osc_hyperlink_plus_visible_duplicate_url() {
    let url = "https://claude.ai/oauth/authorize?code=true&client_id=test&response_type=code&redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback&scope=user%3Ainference&state=abc";
    let raw = format!("\u{1b}]8;;{url}\u{7}{url}\u{1b}]8;;\u{7}\r");
    let normalized = normalize_claude_login_line(&raw);
    assert_eq!(extract_auth_url(&normalized).as_deref(), Some(url));
}

#[test]
fn extract_auth_url_reconstructs_wrapped_scheme_prefix() {
    let wrapped =
        "Open this URL: ht\ntps://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A64111%2Fauth%2Fcallback&state=abc";
    assert_eq!(
        extract_auth_url(wrapped).as_deref(),
        Some(
            "https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A64111%2Fauth%2Fcallback&state=abc"
        )
    );
}

#[test]
fn extract_auth_url_from_value_detects_embedded_url_in_message() {
    let payload = serde_json::json!({
        "message": "Visit this link to sign in: https://accounts.google.com/o/oauth2/auth?foo=bar"
    });
    assert_eq!(
        extract_auth_url_from_value(&payload).as_deref(),
        Some("https://accounts.google.com/o/oauth2/auth?foo=bar")
    );
}

#[test]
fn normalize_claude_login_line_strips_ansi_sequences() {
    let raw = "\u{1b}[90mOpen URL:\u{1b}[0m https://claude.ai/oauth/authorize?foo=bar\r";
    let normalized = normalize_claude_login_line(raw);
    assert_eq!(
        extract_auth_url(&normalized).as_deref(),
        Some("https://claude.ai/oauth/authorize?foo=bar")
    );
}

#[test]
fn normalize_claude_login_line_extracts_url_from_osc8_sequence() {
    let raw = "\u{1b}]8;;https://claude.ai/oauth/authorize?foo=bar\u{7}Sign in\u{1b}]8;;\u{7}\r";
    let normalized = normalize_claude_login_line(raw);
    assert_eq!(
        extract_auth_url(&normalized).as_deref(),
        Some("https://claude.ai/oauth/authorize?foo=bar")
    );
}

#[test]
fn auth_url_looks_complete_rejects_non_loopback_redirect_callback() {
    let url = "https://claude.ai/oauth/authorize?redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback";
    assert!(!auth_url_looks_complete(url));
}

#[test]
fn auth_url_looks_complete_requires_port_for_loopback_callback() {
    let url = "https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%2Fcallback";
    assert!(!auth_url_looks_complete(url));
    let with_port =
        "https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A5999%2Fcallback";
    assert!(auth_url_looks_complete(with_port));
}

#[tokio::test]
async fn read_trailing_claude_login_lines_waits_for_late_arrival() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(40));
        let _ = tx.send("sk-ant-oat01-late-token".to_string());
    });
    let lines = read_trailing_claude_login_lines(&mut rx, Duration::from_secs(2)).await;
    assert_eq!(lines, vec!["sk-ant-oat01-late-token".to_string()]);
}

fn extract_auth_url_from_value(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(raw) => {
            let trimmed = raw.trim();
            if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
                return Some(trimmed.to_string());
            }
            extract_auth_url(trimmed)
        }
        serde_json::Value::Object(map) => {
            let direct = [
                "auth_url",
                "authUrl",
                "url",
                "login_url",
                "loginUrl",
                "authorize_url",
            ]
            .into_iter()
            .find_map(|key| map.get(key))
            .and_then(extract_auth_url_from_value);
            direct.or_else(|| map.values().find_map(extract_auth_url_from_value))
        }
        serde_json::Value::Array(values) => values.iter().find_map(extract_auth_url_from_value),
        _ => None,
    }
}

#[test]
fn preferred_claude_auth_url_uses_browser_open_marker_over_scraped_url() {
    let expected = "https://claude.ai/oauth/authorize?code=true&client_id=cid&response_type=code&redirect_uri=http%3A%2F%2Flocalhost%3A58215%2Fcallback&scope=user%3Ainference&code_challenge=abc&code_challenge_method=S256&state=good-state";
    let corrupted = "https://claude.ai/oauth/authorize?code=true&client_id=cid&response_type=code&redirect_uri=https:/platform.claude.com/oauth/code/callback&scope=user:inference&code_challenge=abc&code_challenge_method=S256&state=bad-statePastecodehereifprompted%3E";
    let transcript = format!(
        "{CLAUDE_BROWSER_OPEN_MARKER}{expected}\nBrowser didn't open? Use the URL below to sign in\n{corrupted}\nPaste code here if prompted >"
    );

    assert_eq!(
        extract_preferred_claude_auth_url(&transcript)
            .map(|(value, _)| value)
            .as_deref(),
        Some(expected)
    );
}

#[test]
fn browser_open_marker_replaces_longer_incomplete_transcript_url() {
    let current = "https://claude.ai/oauth/authorize?code=true&client_id=cid&response_type=code&redirect_uri=https:/platform.claude.com/oauth/code/callback&scope=user:inference&code_challenge=abc&code_challenge_method=S256&state=bad-state";
    let candidate = "https://claude.ai/oauth/authorize?code=true&client_id=cid&response_type=code&redirect_uri=http%3A%2F%2Flocalhost%3A58215%2Fcallback&scope=user%3Ainference&code_challenge=abc&code_challenge_method=S256&state=good-state";

    assert!(should_replace_observed_claude_auth_url(
        Some(current),
        candidate,
        ClaudeAuthUrlSource::BrowserOpenMarker,
    ));
}

#[test]
fn provider_browser_auth_tier_skips_os_browser_launch() {
    assert!(claude_login_should_skip_browser_open(Some(
        CLAUDE_BROWSER_AUTH_TIER
    )));
    assert!(claude_login_should_skip_browser_open(Some(
        " Provider-Browser-Auth "
    )));
    assert!(!claude_login_should_skip_browser_open(Some(
        "provider-api-auth"
    )));
    assert!(!claude_login_should_skip_browser_open(None));
}

#[test]
fn reads_captured_browser_open_url_from_side_channel_file() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let capture_path = temp_dir.path().join("auth-url");
    let expected = "https://claude.ai/oauth/authorize?code=true&client_id=cid&response_type=code&redirect_uri=http%3A%2F%2Flocalhost%3A58215%2Fcallback&scope=user%3Ainference&code_challenge=abc&code_challenge_method=S256&state=good-state";
    std::fs::write(&capture_path, format!("{expected}\n")).expect("write capture file");

    assert_eq!(
        read_claude_browser_open_capture_url(&capture_path).as_deref(),
        Some(expected)
    );
}

fn write_executable_script(path: &std::path::Path, body: &str) {
    std::fs::write(path, body).expect("write script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .expect("chmod script");
    }
}

#[test]
fn browser_open_shim_capture_only_writes_auth_url() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let script_path = temp_dir.path().join("open-browser");
    write_executable_script(&script_path, claude_browser_open_shim_script(true));
    let capture_path = temp_dir.path().join("auth-url");
    let auth_url =
        "https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A5999%2Fcallback&state=test";

    let output = Command::new("/bin/sh")
        .arg(&script_path)
        .arg(auth_url)
        .env("CTX_CLAUDE_AUTH_URL_CAPTURE_PATH", &capture_path)
        .output()
        .expect("run capture-only shim");

    assert!(output.status.success());
    assert_eq!(
        std::fs::read_to_string(&capture_path).expect("read capture path"),
        format!("{auth_url}\n")
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("{CLAUDE_BROWSER_OPEN_MARKER}{auth_url}\n")
    );
}

#[test]
fn browser_open_shim_invokes_open_and_captures_auth_url() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let script_path = temp_dir.path().join("open-browser");
    write_executable_script(&script_path, claude_browser_open_shim_script(false));
    let capture_path = temp_dir.path().join("auth-url");
    let open_log_path = temp_dir.path().join("open.log");
    let open_path = temp_dir.path().join("open");
    write_executable_script(
        &open_path,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$1\" > \"{}\"\nexit 0\n",
            open_log_path.display()
        ),
    );
    let auth_url =
        "https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A6001%2Fcallback&state=test";
    let path_env = format!("{}:/usr/bin:/bin", temp_dir.path().display());

    let output = Command::new("/bin/sh")
        .arg(&script_path)
        .arg(auth_url)
        .env("CTX_CLAUDE_AUTH_URL_CAPTURE_PATH", &capture_path)
        .env("PATH", path_env)
        .output()
        .expect("run browser-open shim");

    assert!(output.status.success());
    assert_eq!(
        std::fs::read_to_string(&capture_path).expect("read capture path"),
        format!("{auth_url}\n")
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("{CLAUDE_BROWSER_OPEN_MARKER}{auth_url}\n")
    );
    assert_eq!(
        std::fs::read_to_string(&open_log_path).expect("read open log"),
        format!("{auth_url}\n")
    );
}
