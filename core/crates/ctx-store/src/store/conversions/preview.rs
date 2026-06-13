pub(super) const MESSAGE_PREVIEW_MAX_CHARS: usize = 160;

pub(super) fn derive_message_preview(content: &str) -> String {
    let trimmed = content.trim();
    let line = trimmed.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        return String::new();
    }
    let mut out: String = line.chars().take(MESSAGE_PREVIEW_MAX_CHARS).collect();
    if line.chars().count() > MESSAGE_PREVIEW_MAX_CHARS {
        out.push_str("...");
    }
    out
}

pub(super) fn derive_activity_from_status(
    last_status: Option<SessionTurnStatus>,
    has_running_turn: bool,
) -> SessionActivityState {
    ctx_core::session_projection::derive_activity_from_status(last_status, has_running_turn)
}
