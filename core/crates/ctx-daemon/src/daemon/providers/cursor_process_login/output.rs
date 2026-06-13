use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::sync::mpsc;

#[derive(Debug)]
pub(super) struct CursorLoginOutputLine {
    pub(super) line: String,
    pub(super) is_stderr: bool,
}

const CURSOR_LOGIN_TIMEOUT_DEFAULT: Duration = Duration::from_secs(300);
pub(super) const CURSOR_LOGIN_POLL_INTERVAL: Duration = Duration::from_millis(700);

pub(super) fn cursor_login_timeout() -> Duration {
    let seconds = std::env::var("CTX_CURSOR_LOGIN_TIMEOUT_SECS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(CURSOR_LOGIN_TIMEOUT_DEFAULT.as_secs());
    Duration::from_secs(seconds)
}

pub(super) fn first_email_from_text(value: &str) -> Option<String> {
    value
        .split_whitespace()
        .map(|token| {
            token
                .trim_matches(|ch: char| {
                    !ch.is_ascii_alphanumeric()
                        && ch != '@'
                        && ch != '.'
                        && ch != '_'
                        && ch != '-'
                        && ch != '+'
                })
                .to_string()
        })
        .find(|token| {
            let Some((local, domain)) = token.split_once('@') else {
                return false;
            };
            !local.is_empty() && domain.contains('.')
        })
}

pub(super) fn spawn_cursor_login_reader<R>(
    reader: R,
    is_stderr: bool,
    tx: mpsc::UnboundedSender<CursorLoginOutputLine>,
) where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    if tx.send(CursorLoginOutputLine { line, is_stderr }).is_err() {
                        return;
                    }
                }
                Ok(None) | Err(_) => return,
            }
        }
    });
}
