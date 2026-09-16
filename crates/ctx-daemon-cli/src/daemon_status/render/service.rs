use ctx_terminal::Token;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DaemonPresentation {
    Healthy,
    Partial,
    Failed,
    Completed,
    NotStarted,
    Unverified,
    Stopped,
    Disabled,
}

pub(super) fn service_state(
    enabled: bool,
    running: bool,
    recoverable: bool,
    status: &str,
) -> (&'static str, Token) {
    if status == "completed" && !enabled {
        ("completed", Token::Success)
    } else if !enabled || status == "disabled" {
        ("disabled", Token::Text)
    } else if recoverable {
        ("failed (recoverable)", Token::Error)
    } else if running {
        ("running", Token::Success)
    } else if status == "unverified" {
        ("unverified", Token::Warning)
    } else if status == "unknown" {
        ("not started", Token::Warning)
    } else {
        ("failed", Token::Error)
    }
}
