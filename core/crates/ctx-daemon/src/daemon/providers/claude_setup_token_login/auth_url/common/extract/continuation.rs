use super::super::*;

fn auth_scheme_can_continue_across_break(current: &str, next_fragment: &str) -> bool {
    let compact_current: String = current
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    let compact_next: String = next_fragment
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    if compact_current.is_empty() || compact_next.is_empty() {
        return false;
    }
    let combined = format!("{compact_current}{compact_next}");
    ["https://", "http://"]
        .into_iter()
        .any(|scheme| scheme.starts_with(&combined))
}

fn fragment_starts_full_auth_url(value: &str) -> bool {
    let compact = value
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    compact.starts_with("https://") || compact.starts_with("http://")
}

pub(super) fn should_continue_auth_url_after_break(current: &str, next_fragment: &str) -> bool {
    if next_fragment.is_empty() {
        return false;
    }
    if fragment_starts_full_auth_url(next_fragment) && Url::parse(current).is_ok() {
        return false;
    }
    if auth_scheme_can_continue_across_break(current, next_fragment) {
        return true;
    }
    if next_fragment.chars().any(|ch| !ch.is_ascii_alphanumeric()) {
        return true;
    }
    if next_fragment
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_digit())
    {
        return true;
    }
    matches!(
        current.chars().last(),
        Some('%' | ':' | '=' | '&' | '?' | '/' | '#' | '-' | '_')
    )
}
