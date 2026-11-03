//! Shared admission bound before parsing untrusted Codex JavaScript wrappers.
pub(super) const MAX_STATIC_JS_BYTES: usize = 1024 * 1024;
pub(super) const MAX_STATIC_RAW_COMPLEXITY_ITEMS: usize = 512;

pub(super) fn has_bounded_parser_complexity(source: &str) -> bool {
    if source.len() > MAX_STATIC_JS_BYTES {
        return false;
    }
    let mut complexity_items = 0usize;
    let mut in_word_run = false;
    for byte in source.bytes() {
        let starts_item = if byte.is_ascii_alphanumeric() {
            let starts_item = !in_word_run;
            in_word_run = true;
            starts_item
        } else {
            in_word_run = false;
            byte.is_ascii_punctuation() || !byte.is_ascii_whitespace()
        };
        if starts_item {
            let Some(updated) = complexity_items.checked_add(1) else {
                return false;
            };
            complexity_items = updated;
            if complexity_items > MAX_STATIC_RAW_COMPLEXITY_ITEMS {
                return false;
            }
        }
    }
    true
}
