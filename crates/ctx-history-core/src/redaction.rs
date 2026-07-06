use std::{fmt, str::FromStr, sync::OnceLock};

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::CoreError;

text_enum! {
    /// Payload handling state.
    ///
    /// The serialized value `safe_preview` is legacy contract spelling for a
    /// local searchable preview. It is not a promise that output is share-safe.
    pub enum RedactionState {
        Raw => "raw",
        Redacted => "redacted",
        LocalPreview => "safe_preview",
        Withheld => "withheld",
    }
    default LocalPreview
}

impl RedactionState {
    /// Compatibility alias for the legacy Rust API name.
    ///
    /// New code should prefer `LocalPreview`, which better matches the local
    /// search contract while preserving the serialized `safe_preview` value.
    #[allow(non_upper_case_globals)]
    pub const SafePreview: Self = Self::LocalPreview;
}

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

fn redact_local_paths(text: &str) -> String {
    let mut value = text.to_owned();
    if let Some(regex) = private_path_prefix_regex() {
        value = regex.replace_all(&value, "$1[REDACTED_PATH]").into_owned();
    }
    for regex in local_path_regexes() {
        value = regex.replace_all(&value, "$1[REDACTED_PATH]").into_owned();
    }
    value
}

fn secret_assignment_regex() -> Option<&'static Regex> {
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

fn credentialed_url_regex() -> Option<&'static Regex> {
    static REGEX: OnceLock<Option<Regex>> = OnceLock::new();
    REGEX
        .get_or_init(|| Regex::new(r#"(?i)\b((?:https?|ssh|git)://)[^/\s:@\[]+:[^/\s@\[]+@"#).ok())
        .as_ref()
}

fn database_url_password_regex() -> Option<&'static Regex> {
    static REGEX: OnceLock<Option<Regex>> = OnceLock::new();
    REGEX
        .get_or_init(|| {
            Regex::new(
                r#"(?i)\b((?:postgres|postgresql|mysql|mariadb|mongodb|redis)://[^/\s:@]+:)[^/\s@]+@"#,
            )
            .ok()
        })
        .as_ref()
}

fn email_assignment_regex() -> Option<&'static Regex> {
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

fn bearer_token_regex() -> Option<&'static Regex> {
    static REGEX: OnceLock<Option<Regex>> = OnceLock::new();
    REGEX
        .get_or_init(|| Regex::new(r"(?i)\b(bearer\s+)[A-Za-z0-9._~+/=-]{12,}\b").ok())
        .as_ref()
}

fn authorization_bearer_regex() -> Option<&'static Regex> {
    static REGEX: OnceLock<Option<Regex>> = OnceLock::new();
    REGEX
        .get_or_init(|| {
            Regex::new(r"(?i)\b(authorization\s*:\s*bearer\s+)[A-Za-z0-9._~+/=-]{3,}\b").ok()
        })
        .as_ref()
}

fn password_phrase_regex() -> Option<&'static Regex> {
    static REGEX: OnceLock<Option<Regex>> = OnceLock::new();
    REGEX
        .get_or_init(|| Regex::new(r#"(?i)\b(password\s+)[^\s,;"']{6,}"#).ok())
        .as_ref()
}

fn private_path_prefix_regex() -> Option<&'static Regex> {
    static REGEX: OnceLock<Option<Regex>> = OnceLock::new();
    REGEX
        .get_or_init(|| {
            Regex::new(
                r#"(?i)(^|[\s"'(=\[])(/(?:home|Users)/[^\s/,;"'<>)\]]+/(?:src|code|work|repo|repos)/[^\s/,;"'<>)\]]*secret[^\s/,;"'<>)\]]*)"#,
            )
            .ok()
        })
        .as_ref()
}

fn standalone_secret_regexes() -> &'static [Regex] {
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

fn local_path_regexes() -> &'static [Regex] {
    static REGEXES: OnceLock<Vec<Regex>> = OnceLock::new();
    REGEXES
        .get_or_init(|| {
            [
                r#"(^|[\s"'(=\[])(/(?:home|Users|tmp|var/tmp|private/tmp|Volumes|mnt|workspace|workspaces|repo|repos|code)(?:/[^\s,;"'<>)\]]*)?)"#,
                r#"(^|[\s"'(=\[])(/(?:[A-Za-z0-9._-]+/)+[^\s,;"'<>)\]]*)"#,
                r#"(?i)(^|[\s"'(=\[])(?:[A-Z]:\\|\\\\)[^\s,;"'<>)\]]+"#,
            ]
            .into_iter()
            .filter_map(|pattern| Regex::new(pattern).ok())
            .collect()
        })
        .as_slice()
}
