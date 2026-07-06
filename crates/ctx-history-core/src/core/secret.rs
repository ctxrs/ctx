#[allow(unused_imports)]
use super::*;

pub(crate) fn secret_assignment_regex() -> Option<&'static Regex> {
    static REGEX: OnceLock<Option<Regex>> = OnceLock::new();
    REGEX
        .get_or_init(|| {
            Regex::new(
                r#"(?i)\b((?:api[_-]?key|access[_-]?key|access[_-]?token|auth[_-]?token|token|secret|password|passwd|pwd)\s*[:=]\s*)([^\s,;"']{3,})"#,
            )
            .ok()
        })
        .as_ref()
}
