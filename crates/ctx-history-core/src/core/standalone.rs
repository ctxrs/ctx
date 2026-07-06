#[allow(unused_imports)]
use super::*;

pub(crate) fn standalone_secret_regexes() -> &'static [Regex] {
    static REGEXES: OnceLock<Vec<Regex>> = OnceLock::new();
    REGEXES
        .get_or_init(|| {
            [
                r"\bsk-[A-Za-z0-9][A-Za-z0-9_-]{12,}\b",
                r"\bgh[pousr]_[A-Za-z0-9_]{16,}\b",
                r"\bAKIA[0-9A-Z]{16}\b",
            ]
            .into_iter()
            .filter_map(|pattern| Regex::new(pattern).ok())
            .collect()
        })
        .as_slice()
}
