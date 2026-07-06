#[allow(unused_imports)]
use super::*;

pub(crate) fn password_phrase_regex() -> Option<&'static Regex> {
    static REGEX: OnceLock<Option<Regex>> = OnceLock::new();
    REGEX
        .get_or_init(|| Regex::new(r#"(?i)\b(password\s+)[^\s,;"']{6,}"#).ok())
        .as_ref()
}
