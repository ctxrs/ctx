#[allow(unused_imports)]
use super::*;

pub(crate) fn email_assignment_regex() -> Option<&'static Regex> {
    static REGEX: OnceLock<Option<Regex>> = OnceLock::new();
    REGEX
        .get_or_init(|| {
            Regex::new(
                r#"(?i)\b((?:customer[_-]?email|email)\s*[:=]\s*)[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b"#,
            )
            .ok()
        })
        .as_ref()
}
