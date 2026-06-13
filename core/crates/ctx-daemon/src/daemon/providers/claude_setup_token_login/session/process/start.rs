use super::*;

use ctx_provider_runtime::provider_login_runtime::ProviderLoginRuntimeCommand;

pub(in crate::daemon::providers::claude_setup_token_login) async fn start_claude_login_process(
    runtime: &ProviderLoginRuntimeCommand,
) -> anyhow::Result<ClaudeLoginProcess> {
    let ClaudeLoginSpawn {
        line_rx: mut rx,
        mut exit_rx,
        killer,
        browser_open_capture_path,
        browser_open_shim_dir,
    } = spawn_claude_setup_token_command(runtime)?;

    let mut buffered_lines = Vec::new();
    let mut auth_url = None;
    let mut hit_unsupported_manual_fallback = false;
    let mut transcript = String::new();
    let hard_deadline = Instant::now() + CLAUDE_LOGIN_URL_WAIT;
    let mut settle_deadline: Option<Instant> = None;
    let mut output_closed = false;
    loop {
        if refresh_claude_auth_url_from_capture_path(&mut auth_url, &browser_open_capture_path) {
            settle_deadline = Some(Instant::now() + CLAUDE_LOGIN_URL_SETTLE_WAIT);
        }
        let now = Instant::now();
        let remaining = if let Some(settle) = settle_deadline {
            std::cmp::min(
                hard_deadline.saturating_duration_since(now),
                settle.saturating_duration_since(now),
            )
        } else {
            hard_deadline.saturating_duration_since(now)
        };
        if remaining.is_zero() {
            break;
        }
        let wait = remaining.min(CLAUDE_LOGIN_CAPTURE_POLL_INTERVAL);
        match tokio::time::timeout(wait, rx.recv()).await {
            Ok(Some(line)) => {
                output_closed = false;
                transcript.push_str(&line);
                transcript.push('\n');
                hit_unsupported_manual_fallback |=
                    claude_login_hit_unsupported_manual_fallback(&line);
                buffered_lines.push(line);
                if refresh_claude_auth_url_from_capture_path(
                    &mut auth_url,
                    &browser_open_capture_path,
                ) {
                    settle_deadline = Some(Instant::now() + CLAUDE_LOGIN_URL_SETTLE_WAIT);
                }
                if let Some((candidate, source)) = extract_preferred_claude_auth_url(&transcript) {
                    if should_replace_observed_claude_auth_url(
                        auth_url.as_deref(),
                        &candidate,
                        source,
                    ) {
                        auth_url = Some(candidate);
                    }
                    settle_deadline = Some(Instant::now() + CLAUDE_LOGIN_URL_SETTLE_WAIT);
                }
            }
            Ok(None) => {
                if !output_closed {
                    // The setup-token process can tear down its PTY before the
                    // browser shim capture file becomes visible on disk. Keep
                    // polling for one short settle window instead of exiting
                    // immediately on EOF.
                    output_closed = true;
                    settle_deadline = Some(Instant::now() + CLAUDE_LOGIN_URL_SETTLE_WAIT);
                }
                tokio::time::sleep(wait.min(CLAUDE_LOGIN_CAPTURE_POLL_INTERVAL)).await;
                continue;
            }
            Err(_) => continue,
        }
    }
    refresh_claude_auth_url_from_capture_path(&mut auth_url, &browser_open_capture_path);

    if hit_unsupported_manual_fallback
        && claude_manual_fallback_is_terminal(&transcript, &browser_open_capture_path)
    {
        let _ = monitor::kill_claude_login_process(Arc::clone(&killer)).await;
        let _ = tokio::time::timeout(CLAUDE_LOGIN_EXIT_GRACE_WAIT, &mut exit_rx).await;
        anyhow::bail!(CLAUDE_UNSUPPORTED_MANUAL_FALLBACK_ERROR);
    }

    Ok(ClaudeLoginProcess {
        line_rx: rx,
        buffered_lines,
        auth_url,
        browser_open_capture_path,
        exit_rx,
        killer,
        _browser_open_shim_dir: browser_open_shim_dir,
    })
}
