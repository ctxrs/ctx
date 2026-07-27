use std::borrow::Cow;

use ctx_history_core::{compact_result_payload, Event, EventType};
use serde_json::Value;

use crate::{Result, StoreError};

/// Whether a normalized provider output is one of the sparse diagnostics Core
/// retains. Unknown and successful outputs belong only to the transient Pro
/// stream.
pub(crate) fn provider_output_is_retained_failure(event: &Event) -> bool {
    if !matches!(
        event.event_type,
        EventType::ToolOutput | EventType::CommandOutput
    ) {
        return true;
    }
    result_payload_is_failure(&event.payload)
}

fn result_payload_is_failure(payload: &Value) -> bool {
    let compact = compact_result_payload(payload);
    compact
        .get("result_outcome")
        .and_then(Value::as_str)
        .is_some_and(|outcome| outcome == "failure")
        || compact
            .get("timed_out")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        || compact
            .get("exit_code")
            .and_then(Value::as_i64)
            .is_some_and(|exit_code| exit_code != 0)
}

/// Returns the exact event shape that may cross a durable Store write boundary.
///
/// Result bodies are always reduced to the bounded typed contract. Blob-backed
/// result bodies are rejected because silently dropping the reference could
/// still admit the referenced bytes through an archive artifact record.
pub(crate) fn durable_event(event: &Event) -> Result<Cow<'_, Event>> {
    if !matches!(
        event.event_type,
        EventType::ToolOutput | EventType::CommandOutput
    ) {
        return Ok(Cow::Borrowed(event));
    }
    if event.payload_blob_id.is_some() {
        return Err(StoreError::ResultPayloadBlobUnsupported { id: event.id });
    }
    let mut durable = event.clone();
    durable.payload = compact_stored_result_event_payload(&durable.payload);
    Ok(Cow::Owned(durable))
}

/// Compacts both direct result payloads and provider-import wrappers while
/// retaining only the wrapper fields needed for canonical provenance.
pub(crate) fn compact_stored_result_event_payload(payload: &Value) -> Value {
    let Some(wrapper) = payload.as_object() else {
        return compact_result_diagnostic_payload(payload);
    };
    let is_import_wrapper = wrapper.contains_key("body")
        && [
            "provider",
            "provider_session_id",
            "provider_event_index",
            "provider_event_hash",
            "cursor",
            "artifacts",
        ]
        .iter()
        .any(|key| wrapper.contains_key(*key));
    if !is_import_wrapper {
        return compact_result_diagnostic_payload(payload);
    }

    let mut compact_wrapper = serde_json::Map::new();
    for key in [
        "provider",
        "provider_session_id",
        "provider_event_index",
        "provider_event_hash",
        "cursor",
        "artifacts",
    ] {
        if let Some(value) = wrapper.get(key) {
            compact_wrapper.insert(key.to_owned(), value.clone());
        }
    }
    compact_wrapper.insert(
        "body".to_owned(),
        compact_result_diagnostic_payload(payload),
    );
    Value::Object(compact_wrapper)
}

fn compact_result_diagnostic_payload(payload: &Value) -> Value {
    compact_result_payload(payload)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn failure_and_success_previews_are_never_durable() {
        let failure = json!({
            "body": {
                "result_outcome": "failure",
                "exit_code": 1,
                "output_preview": "private failure text"
            }
        });
        let compact = compact_stored_result_event_payload(&failure);
        assert!(compact.get("output_preview").is_none());

        let success = json!({
            "result_outcome": "success",
            "exit_code": 0,
            "output_preview": "must not persist"
        });
        assert!(compact_stored_result_event_payload(&success)
            .get("output_preview")
            .is_none());
    }
}
