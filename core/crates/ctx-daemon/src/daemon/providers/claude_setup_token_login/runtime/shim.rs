use std::path::PathBuf;

use anyhow::Context;

pub(in crate::daemon::providers::claude_setup_token_login) const CLAUDE_BROWSER_AUTH_TIER: &str =
    "provider-browser-auth";

pub(in crate::daemon::providers::claude_setup_token_login) fn claude_login_should_skip_browser_open(
    raw_tier: Option<&str>,
) -> bool {
    matches!(
        raw_tier.map(str::trim),
        Some(tier) if tier.eq_ignore_ascii_case(CLAUDE_BROWSER_AUTH_TIER)
    )
}

pub(in crate::daemon::providers::claude_setup_token_login) fn claude_browser_open_shim_script(
    skip_browser_open: bool,
) -> &'static str {
    if skip_browser_open {
        r#"#!/bin/sh
url="${1:-}"
capture_path="${CTX_CLAUDE_AUTH_URL_CAPTURE_PATH:-}"
if [ -n "$url" ] && [ -n "$capture_path" ]; then
  printf '%s\n' "$url" > "$capture_path"
fi
if [ -n "$url" ]; then
  printf 'CTX_CLAUDE_AUTH_URL:%s\n' "$url"
fi
exit 0
"#
    } else {
        r#"#!/bin/sh
url="${1:-}"
capture_path="${CTX_CLAUDE_AUTH_URL_CAPTURE_PATH:-}"
if [ -n "$url" ] && [ -n "$capture_path" ]; then
  printf '%s\n' "$url" > "$capture_path"
fi
if [ -n "$url" ]; then
  printf 'CTX_CLAUDE_AUTH_URL:%s\n' "$url"
fi
if [ -z "$url" ]; then
  exit 1
fi
if command -v open >/dev/null 2>&1; then
  exec open "$url"
fi
if command -v xdg-open >/dev/null 2>&1; then
  exec xdg-open "$url"
fi
exit 1
"#
    }
}

pub(super) fn create_claude_browser_open_shim(
) -> anyhow::Result<(tempfile::TempDir, PathBuf, PathBuf)> {
    let temp_dir = tempfile::Builder::new()
        .prefix("ctx-claude-browser-open-")
        .tempdir()
        .context("creating Claude browser-open shim tempdir")?;
    let script_path = temp_dir.path().join("open-browser");
    let capture_path = temp_dir.path().join("auth-url");
    let skip_browser_open =
        claude_login_should_skip_browser_open(std::env::var("CTX_E2E_TIER").ok().as_deref());
    // This script is the browser-open golden path for Claude setup-token, so
    // it must stay POSIX `sh` compatible.
    let script_body = claude_browser_open_shim_script(skip_browser_open);
    std::fs::write(&script_path, script_body)
        .with_context(|| format!("writing Claude browser-open shim {}", script_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o700))
            .with_context(|| {
                format!(
                    "marking Claude browser-open shim executable {}",
                    script_path.display()
                )
            })?;
    }
    Ok((temp_dir, script_path, capture_path))
}
