use std::borrow::Cow;

use ctx_history_core::{compact_result_payload, Event, EventType};
use serde_json::Value;

use crate::{Result, StoreError};

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
        return compact_result_payload(payload);
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
        return compact_result_payload(payload);
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
    compact_wrapper.insert("body".to_owned(), compact_result_payload(payload));
    Value::Object(compact_wrapper)
}
