use std::time::Instant;

use super::super::output::{
    cursor_login_timeout, spawn_cursor_login_reader, CursorLoginOutputLine,
    CURSOR_LOGIN_POLL_INTERVAL,
};
use super::progress::record_cursor_login_output;
use ctx_provider_runtime::ProviderRuntime;

pub(super) struct CursorLoginOutputResult {
    pub(super) observed_auth_url: Option<String>,
    pub(super) observed_email: Option<String>,
    pub(super) timeout_error: Option<String>,
    pub(super) exit_result: std::io::Result<std::process::ExitStatus>,
}

pub(super) async fn collect_cursor_login_output(
    providers: &ProviderRuntime,
    login_id: &str,
    child: &mut tokio::process::Child,
) -> CursorLoginOutputResult {
    let mut transcript = String::new();
    let mut observed_auth_url = None::<String>;
    let mut observed_email = None::<String>;
    let (line_tx, mut line_rx) = tokio::sync::mpsc::unbounded_channel::<CursorLoginOutputLine>();
    if let Some(stdout) = child.stdout.take() {
        spawn_cursor_login_reader(stdout, false, line_tx.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_cursor_login_reader(stderr, true, line_tx.clone());
    }
    drop(line_tx);

    let started_at = Instant::now();
    let timeout = cursor_login_timeout();
    let mut timeout_error: Option<String> = None;
    let exit_result: std::io::Result<std::process::ExitStatus> = loop {
        if started_at.elapsed() >= timeout {
            timeout_error = Some("timed out waiting for Cursor OAuth completion".to_string());
            let _ = child.kill().await;
            break child.wait().await;
        }

        tokio::select! {
            maybe_line = line_rx.recv() => {
                if let Some(output_line) = maybe_line {
                    record_cursor_login_output(
                        providers,
                        login_id,
                        output_line,
                        &mut transcript,
                        &mut observed_email,
                        &mut observed_auth_url,
                    )
                    .await;
                }
            }
            wait = child.wait() => {
                break wait;
            }
            _ = tokio::time::sleep(CURSOR_LOGIN_POLL_INTERVAL) => {}
        }
    };

    let drain_deadline = Instant::now() + std::time::Duration::from_millis(200);
    loop {
        match tokio::time::timeout(std::time::Duration::from_millis(20), line_rx.recv()).await {
            Ok(Some(output_line)) => {
                record_cursor_login_output(
                    providers,
                    login_id,
                    output_line,
                    &mut transcript,
                    &mut observed_email,
                    &mut observed_auth_url,
                )
                .await;
            }
            Ok(None) => break,
            Err(_) if Instant::now() >= drain_deadline => break,
            Err(_) => {
                continue;
            }
        }
    }

    CursorLoginOutputResult {
        observed_auth_url,
        observed_email,
        timeout_error,
        exit_result,
    }
}
