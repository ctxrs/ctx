use ctx_history_core::{CtxHistoryJsonlSourceRecord, CTX_HISTORY_JSONL_V1_SCHEMA_VERSION};
use serde_json::{json, Value};

use crate::stable_capture_uuid;

use crate::{ProviderImportFailure, ProviderImportSummary};

pub(crate) const CUSTOM_HISTORY_IDENTIFIER_MAX_BYTES: usize = 512;

mod nativepath;

pub(crate) use nativepath::{
    observe_custom_history_source_backed_explicit, revalidate_custom_history_source_backed,
    scan_custom_history_source_backed_explicit, CustomHistorySourceBackedDisposition,
    CustomHistorySourceBackedError, CustomHistorySourceBackedInput,
    CustomHistorySourceBackedOutcome,
};

pub(crate) fn push_provider_import_failure(
    summary: &mut ProviderImportSummary,
    line: usize,
    error: String,
) {
    summary.failed += 1;
    summary.failures.push(ProviderImportFailure { line, error });
}

pub(crate) fn validate_custom_source_record(
    summary: &mut ProviderImportSummary,
    line_number: usize,
    source: &CtxHistoryJsonlSourceRecord,
) {
    validate_custom_history_identifier(summary, line_number, "source_id", &source.source_id);
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

pub(crate) fn custom_history_internal_session_id(
    provider_key: &str,
    source_id: &str,
    session_id: &str,
) -> String {
    let key = custom_history_key(json!({
        "schema": CTX_HISTORY_JSONL_V1_SCHEMA_VERSION,
        "kind": "session",
        "provider_key": provider_key,
        "source_id": source_id,
        "session_id": session_id,
    }));
    let id = stable_capture_uuid(&key, "custom-provider-session-id");
    format!("ctx-history-jsonl-v1-{id}")
}

pub(crate) fn custom_history_key(value: Value) -> String {
    serde_json::to_string(&value).expect("custom history identity key is serializable")
}
