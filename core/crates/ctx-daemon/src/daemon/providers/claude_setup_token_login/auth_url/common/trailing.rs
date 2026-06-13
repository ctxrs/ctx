use super::*;

pub(in crate::daemon::providers::claude_setup_token_login) async fn read_trailing_claude_login_lines(
    line_rx: &mut mpsc::UnboundedReceiver<String>,
    grace: Duration,
) -> Vec<String> {
    let mut lines = Vec::new();
    let mut deadline = Instant::now() + grace;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, line_rx.recv()).await {
            Ok(Some(line)) => {
                lines.push(line);
                deadline = Instant::now() + grace;
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }
    lines
}
