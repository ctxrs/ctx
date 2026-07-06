#[allow(unused_imports)]
use super::*;

pub(crate) fn credentialed_url_regex() -> Option<&'static Regex> {
    static REGEX: OnceLock<Option<Regex>> = OnceLock::new();
    REGEX
        .get_or_init(|| Regex::new(r#"(?i)\b((?:https?|ssh|git)://)[^/\s:@\[]+:[^/\s@\[]+@"#).ok())
        .as_ref()
}
