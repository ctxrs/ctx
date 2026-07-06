#[allow(unused_imports)]
use super::*;

pub fn redact_preview(text: &str, max_chars: usize) -> String {
    let mut preview = String::new();
    for ch in text.chars().take(max_chars) {
        preview.push(ch);
    }
    redact_secret_markers(&preview)
}

pub fn redact_share_safe_preview(text: &str, max_chars: usize) -> String {
    let mut preview = String::new();
    for ch in text.chars().take(max_chars) {
        preview.push(ch);
    }
    redact_share_safe_markers(&preview)
}

pub fn redact_share_safe_markers(text: &str) -> String {
    redact_local_paths(&redact_secret_markers(text))
}

pub fn redact_secret_markers(text: &str) -> String {
    let mut value = text.to_owned();
    if let Some(regex) = database_url_password_regex() {
        value = regex
            .replace_all(&value, "$1[REDACTED_SECRET]@")
            .into_owned();
    }
    if let Some(regex) = credentialed_url_regex() {
        value = regex
            .replace_all(&value, "$1[REDACTED_CREDENTIAL]@")
            .into_owned();
    }
    if let Some(regex) = email_assignment_regex() {
        value = regex.replace_all(&value, "$1[REDACTED_EMAIL]").into_owned();
    }
    if let Some(regex) = authorization_bearer_regex() {
        value = regex
            .replace_all(&value, "$1[REDACTED_SECRET]")
            .into_owned();
    }
    if let Some(regex) = bearer_token_regex() {
        value = regex
            .replace_all(&value, "$1[REDACTED_SECRET]")
            .into_owned();
    }
    for regex in standalone_secret_regexes() {
        value = regex.replace_all(&value, "[REDACTED_SECRET]").into_owned();
    }
    if let Some(regex) = secret_assignment_regex() {
        value = regex
            .replace_all(&value, "$1[REDACTED_SECRET]")
            .into_owned();
    }
    if let Some(regex) = password_phrase_regex() {
        value = regex
            .replace_all(&value, "$1[REDACTED_SECRET]")
            .into_owned();
    }
    value
}
