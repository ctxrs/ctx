use ctx_protocol::{
    camel_alias_object, camelize_object_keys, AgentHistoryEnvelope, AgentHistoryErrorCode,
    AgentHistoryOperation, AgentHistoryStatus, BackendInfo, EventResult, ImportResult,
    LocationResult, SearchResult, SessionResult,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::AgentHistoryError;

pub(crate) fn normalize(
    operation: AgentHistoryOperation,
    backend: BackendInfo,
    raw: Value,
) -> Result<AgentHistoryEnvelope, AgentHistoryError> {
    let mut envelope = AgentHistoryEnvelope::new(operation.clone(), Some(backend));
    match operation {
        AgentHistoryOperation::Status => envelope.status = Some(normalize_status(&raw)?),
        AgentHistoryOperation::Init => envelope.status = Some(normalize_status(&raw)?),
        AgentHistoryOperation::Sources => {
            envelope.sources = Some(decode_payload(
                camelize_object_keys(&raw.get("sources").cloned().unwrap_or_else(|| json!([]))),
                "sources",
            )?)
        }
        AgentHistoryOperation::Import | AgentHistoryOperation::Sync => {
            envelope.import_result = Some(normalize_import(&raw)?)
        }
        AgentHistoryOperation::Search => envelope.search = Some(normalize_search(&raw)?),
        AgentHistoryOperation::ShowEvent => envelope.event = Some(normalize_event(&raw)?),
        AgentHistoryOperation::ShowSession => envelope.session = Some(normalize_session(&raw)?),
        AgentHistoryOperation::LocateEvent | AgentHistoryOperation::LocateSession => {
            envelope.location = Some(normalize_location(&raw)?)
        }
        AgentHistoryOperation::Error => {}
    }
    Ok(envelope)
}

fn decode_payload<T: DeserializeOwned>(
    value: Value,
    payload: &str,
) -> Result<T, AgentHistoryError> {
    serde_json::from_value(value).map_err(|err| {
        AgentHistoryError::new(
            AgentHistoryErrorCode::DecodeError,
            format!("failed to decode agent-history-v1 {payload} payload"),
            false,
        )
        .with_cause(err.to_string())
    })
}

fn normalize_status(raw: &Value) -> Result<AgentHistoryStatus, AgentHistoryError> {
    let mut value = camel_alias_object(
        raw,
        &[
            ("schema_version", "schemaVersion"),
            ("data_root", "dataRoot"),
            ("indexed_items", "indexedItems"),
            ("indexed_sources", "indexedSources"),
            ("cataloged_sessions", "catalogedSessions"),
            ("indexed_catalog_sessions", "indexedCatalogSessions"),
            ("pending_catalog_sessions", "pendingCatalogSessions"),
            ("failed_catalog_sessions", "failedCatalogSessions"),
            ("stale_catalog_sessions", "staleCatalogSessions"),
            ("local_only", "localOnly"),
        ],
    );
    if let Some(object) = value.as_object_mut() {
        if !object.contains_key("initialized") {
            let initialized = object
                .get("mode")
                .and_then(Value::as_str)
                .map(|mode| matches!(mode, "ready" | "catalog_only"))
                .unwrap_or(true);
            object.insert("initialized".to_owned(), Value::Bool(initialized));
        }
        if !object.contains_key("localOnly") {
            object.insert("localOnly".to_owned(), Value::Bool(true));
        }
    }
    decode_payload(camelize_object_keys(&value), "status")
}

fn normalize_import(raw: &Value) -> Result<ImportResult, AgentHistoryError> {
    let value = camel_alias_object(raw, &[("resume_mode", "resumeMode")]);
    decode_payload(camelize_object_keys(&value), "import")
}

fn normalize_search(raw: &Value) -> Result<SearchResult, AgentHistoryError> {
    let value = camel_alias_object(raw, &[("generated_at", "generatedAt")]);
    decode_payload(camelize_object_keys(&value), "search")
}

fn normalize_event(raw: &Value) -> Result<EventResult, AgentHistoryError> {
    let value = json!({
        "event": raw.get("event").cloned(),
        "events": raw.get("events").cloned().unwrap_or_else(|| json!([])),
        "source": raw.get("source").cloned()
    });
    decode_payload(camelize_object_keys(&value), "event")
}

fn normalize_session(raw: &Value) -> Result<SessionResult, AgentHistoryError> {
    let value = json!({
        "session": raw.get("session").cloned(),
        "events": raw.get("events").cloned().unwrap_or_else(|| json!([])),
        "source": raw.get("source").cloned(),
        "mode": raw.get("mode").cloned(),
        "format": raw.get("format").cloned()
    });
    decode_payload(camelize_object_keys(&value), "session")
}

fn normalize_location(raw: &Value) -> Result<LocationResult, AgentHistoryError> {
    let value = camel_alias_object(
        raw,
        &[
            ("ctx_session_id", "ctxSessionId"),
            ("ctx_event_id", "ctxEventId"),
            ("provider_session_id", "providerSessionId"),
        ],
    );
    decode_payload(camelize_object_keys(&value), "location")
}
