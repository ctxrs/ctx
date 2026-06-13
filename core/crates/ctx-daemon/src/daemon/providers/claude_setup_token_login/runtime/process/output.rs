use tokio::sync::mpsc;

use super::super::super::auth_url::normalize_claude_login_line;

pub(super) fn pump_claude_login_output<R>(mut reader: R, tx: mpsc::UnboundedSender<String>)
where
    R: std::io::Read,
{
    let mut pending = String::new();
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                pending.push_str(&String::from_utf8_lossy(&buf[..n]));
                while let Some(newline_idx) = pending.find('\n') {
                    let raw = pending[..newline_idx].to_string();
                    pending.drain(..=newline_idx);
                    if tx.send(normalize_claude_login_line(&raw)).is_err() {
                        return;
                    }
                }
            }
            Err(_) => break,
        }
    }
    if !pending.is_empty() {
        let _ = tx.send(normalize_claude_login_line(&pending));
    }
}
