#[allow(unused_imports)]
use super::*;

pub(crate) fn authorization_bearer_regex() -> Option<&'static Regex> {
    static REGEX: OnceLock<Option<Regex>> = OnceLock::new();
    REGEX
        .get_or_init(|| {
            Regex::new(r"(?i)\b(authorization\s*:\s*bearer\s+)[A-Za-z0-9._~+/=-]{3,}\b").ok()
        })
        .as_ref()
}
