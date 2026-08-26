use ctx_history_core::CtxHistoryJsonlSourceRecord;

use ctx_history_capture_model::push_provider_import_failure;

use crate::ProviderImportSummary;

pub(crate) const CUSTOM_HISTORY_IDENTIFIER_MAX_BYTES: usize = 512;

mod nativepath;

pub(crate) use nativepath::{custom_history_jsonl_family_adapter, CustomHistorySourceBackedInput};

pub(crate) fn validate_custom_source_record(
    summary: &mut ProviderImportSummary,
    line_number: usize,
    source: &CtxHistoryJsonlSourceRecord,
) {
    validate_custom_history_identifier(summary, line_number, "source_id", &source.source_id);
    if source.source_id.contains('/') {
        push_provider_import_failure(
            summary,
            line_number,
            "source_id must not contain '/' because provider_key/source_id is the route selector"
                .to_owned(),
        );
    }
    validate_custom_history_identifier(
        summary,
        line_number,
        "source_format",
        &source.source_format,
    );
    let valid = !source.provider_key.is_empty()
        && source.provider_key.len() <= 128
        && source.provider_key.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
        && source
            .provider_key
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    if !valid {
        push_provider_import_failure(
            summary,
            line_number,
            "provider_key must be 1 to 128 bytes, start with a lowercase ASCII letter or digit, and use only lowercase ASCII letters, digits, '.', '_', or '-'".to_owned(),
        );
    }
}

pub(crate) fn validate_custom_history_identifier(
    summary: &mut ProviderImportSummary,
    line_number: usize,
    field: &str,
    value: &str,
) {
    let error = if value.trim().is_empty() {
        Some(format!("{field} must not be empty"))
    } else if value.trim() != value {
        Some(format!(
            "{field} must not have leading or trailing whitespace"
        ))
    } else if value.len() > CUSTOM_HISTORY_IDENTIFIER_MAX_BYTES {
        Some(format!(
            "{field} must be at most {CUSTOM_HISTORY_IDENTIFIER_MAX_BYTES} bytes"
        ))
    } else if value.chars().any(char::is_control) {
        Some(format!("{field} must not contain control characters"))
    } else {
        None
    };
    if let Some(error) = error {
        push_provider_import_failure(summary, line_number, error);
    }
}
