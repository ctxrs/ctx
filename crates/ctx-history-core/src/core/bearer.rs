#[allow(unused_imports)]
use super::*;

pub(crate) fn bearer_token_regex() -> Option<&'static Regex> {
    static REGEX: OnceLock<Option<Regex>> = OnceLock::new();
    REGEX
        .get_or_init(|| Regex::new(r"(?i)\b(bearer\s+)[A-Za-z0-9._~+/=-]{12,}\b").ok())
        .as_ref()
}
