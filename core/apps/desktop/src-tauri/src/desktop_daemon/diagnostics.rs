use super::*;

pub(super) fn ssh_log_snippet(stderr_log: &std::sync::Arc<std::sync::Mutex<String>>) -> String {
    let log = stderr_log.lock().ok();
    let Some(log) = log.as_ref() else {
        return String::new();
    };
    let snippet = log.trim();
    if snippet.is_empty() {
        String::new()
    } else {
        snippet.to_string()
    }
}

fn truncate_tail_chars(value: &str, max_chars: usize) -> String {
    let total = value.chars().count();
    if total <= max_chars {
        return value.to_string();
    }
    let skip = total - max_chars;
    let mut idx = 0;
    let mut seen = 0;
    for (i, _) in value.char_indices() {
        if seen == skip {
            idx = i;
            break;
        }
        seen += 1;
    }
    value[idx..].to_string()
}

pub(super) fn daemon_stderr_snippet(path: Option<&Path>) -> String {
    let Some(path) = path else {
        return String::new();
    };
    let Ok(bytes) = std::fs::read(path) else {
        return String::new();
    };
    let text = String::from_utf8_lossy(&bytes);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        String::new()
    } else {
        truncate_tail_chars(trimmed, 1200)
    }
}
