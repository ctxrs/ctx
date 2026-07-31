//! Experimental `agent-history-v1` contract types shared by in-repo ctx SDKs.
//!
//! These types describe the SDK product contract. They are not SQLite schema
//! types and are not a promise to preserve current CLI JSON internals.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const CONTRACT_VERSION: &str = "agent-history-v1";
pub const SCHEMA_VERSION: u16 = 1;

/// Extensible JSON object used where `agent-history-v1` intentionally leaves room for
/// backend-specific additive fields.
pub type JsonObject = BTreeMap<String, Value>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BackendKind {
    Local,
    Hosted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendInfo {
    pub kind: BackendKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

impl BackendInfo {
    pub fn local(data_root: Option<String>) -> Self {
        Self {
            kind: BackendKind::Local,
            data_root,
            base_url: None,
            extra: JsonObject::new(),
        }
    }

    pub fn hosted(base_url: Option<String>) -> Self {
        Self {
            kind: BackendKind::Hosted,
            data_root: None,
            base_url,
            extra: JsonObject::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentHistoryOperation {
    Status,
    Init,
    Sources,
    Import,
    Sync,
    Search,
    ShowEvent,
    ShowSession,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentHistoryErrorCode {
    InvalidRequest,
    NotFound,
    NotInitialized,
    BackendUnavailable,
    Timeout,
    Cancelled,
    NotSupported,
    AdapterError,
    DecodeError,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHistoryErrorBody {
    pub code: AgentHistoryErrorCode,
    pub message: String,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonObject>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

impl AgentHistoryErrorBody {
    pub fn new(code: AgentHistoryErrorCode, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code,
            message: message.into(),
            retryable,
            details: None,
            cause: None,
            extra: JsonObject::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Totals {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_files: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_sources: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_sources: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_sessions: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_events: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_edges: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<u64>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Freshness {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub totals: Option<Totals>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHistoryStatus {
    pub initialized: bool,
    pub local_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexed_items: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexed_sources: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cataloged_sessions: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexed_catalog_sessions: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_catalog_sessions: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_catalog_sessions: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_catalog_sessions: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness: Option<Freshness>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSource {
    pub provider: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exists: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_format: Option<String>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_support: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_import: Option<bool>,
    pub importable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unsupported_reason: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub resume: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_mode: Option<String>,
    pub totals: Totals,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<JsonObject>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filters: Option<JsonObject>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness: Option<Freshness>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval: Option<SearchRetrieval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub results: Vec<SearchHit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_window: Option<SearchResultWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pagination: Option<JsonObject>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation: Option<JsonObject>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResultWindow {
    pub limit: u64,
    pub returned: u64,
    pub more_available: bool,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchRetrieval {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_weight: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_fallback_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_fallback: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<SearchRetrievalCoverage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<JsonObject>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<JsonObject>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchRetrievalCoverage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedded_items: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedded_chunks: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub searchable_items: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexed_now: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dirty_items: Option<u64>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctx_event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctx_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_type: Option<String>,
    pub result_scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_exists: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub why_matched: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<Citation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggested_next_commands: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Citation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctx_event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctx_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_exists: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHistoryEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctx_event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctx_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurred_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<Citation>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceLocation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exists: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_format: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<AgentHistoryEvent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<AgentHistoryEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceLocation>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<JsonObject>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<AgentHistoryEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceLocation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHistoryEnvelope {
    pub contract_version: String,
    pub schema_version: u16,
    pub operation: AgentHistoryOperation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<BackendInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<AgentHistoryStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sources: Option<Vec<ProviderSource>>,
    #[serde(rename = "import", default, skip_serializing_if = "Option::is_none")]
    pub import_result: Option<ImportResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<SearchResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<EventResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<AgentHistoryErrorBody>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

impl AgentHistoryEnvelope {
    pub fn new(operation: AgentHistoryOperation, backend: Option<BackendInfo>) -> Self {
        Self {
            contract_version: CONTRACT_VERSION.to_owned(),
            schema_version: SCHEMA_VERSION,
            operation,
            backend,
            status: None,
            sources: None,
            import_result: None,
            search: None,
            event: None,
            session: None,
            error: None,
            extra: JsonObject::new(),
        }
    }

    pub fn error(backend: Option<BackendInfo>, error: AgentHistoryErrorBody) -> Self {
        let mut envelope = Self::new(AgentHistoryOperation::Error, backend);
        envelope.error = Some(error);
        envelope
    }
}

pub fn camel_alias_object(value: &Value, aliases: &[(&str, &str)]) -> Value {
    let mut out = value.clone();
    if let Some(object) = out.as_object_mut() {
        for (from, to) in aliases {
            if let Some(item) = object.remove(*from) {
                object.insert((*to).to_owned(), item);
            }
        }
    }
    out
}

/// Recursively converts snake_case object keys from private CLI JSON into the
/// camelCase keys used by the public `agent-history-v1` contract.
pub fn camelize_object_keys(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(camelize_object_keys).collect()),
        Value::Object(object) => {
            let mut out = Map::new();
            for (key, item) in object {
                let camel_key = snake_to_camel(key);
                if omitted_public_key(&camel_key) {
                    continue;
                }
                out.insert(camel_key, camelize_object_keys(item));
            }
            Value::Object(out)
        }
        _ => value.clone(),
    }
}

fn omitted_public_key(key: &str) -> bool {
    matches!(
        key,
        "itemType" | "payloadType" | "recordType" | "databasePath" | "configPath"
    )
}

fn snake_to_camel(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    let mut uppercase_next = false;
    for ch in key.chars() {
        if ch == '_' {
            uppercase_next = true;
        } else if uppercase_next {
            out.extend(ch.to_uppercase());
            uppercase_next = false;
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    use super::*;

    fn fixture_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/agent-history-v1/fixtures")
    }

    #[test]
    fn parses_all_shared_fixtures_into_typed_envelopes() {
        let mut seen = 0;
        for entry in fs::read_dir(fixture_root()).unwrap() {
            let entry = entry.unwrap();
            if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let fixture = fs::read_to_string(entry.path()).unwrap();
            let envelope: AgentHistoryEnvelope = serde_json::from_str(&fixture).unwrap();
            assert_eq!(envelope.contract_version, CONTRACT_VERSION);
            assert_eq!(envelope.schema_version, SCHEMA_VERSION);
            match envelope.operation {
                AgentHistoryOperation::Status | AgentHistoryOperation::Init => {
                    assert!(envelope.status.is_some(), "{:?}", entry.path());
                }
                AgentHistoryOperation::Sources => {
                    assert!(envelope.sources.is_some(), "{:?}", entry.path())
                }
                AgentHistoryOperation::Import | AgentHistoryOperation::Sync => {
                    assert!(envelope.import_result.is_some(), "{:?}", entry.path());
                }
                AgentHistoryOperation::Search => {
                    let search = envelope.search.as_ref().expect("search fixture payload");
                    let result_window = search
                        .result_window
                        .as_ref()
                        .expect("search fixture resultWindow");
                    assert_eq!(result_window.returned, search.results.len() as u64);
                    assert!(search.pagination.is_some(), "{:?}", entry.path());
                }
                AgentHistoryOperation::ShowEvent => {
                    assert!(envelope.event.is_some(), "{:?}", entry.path())
                }
                AgentHistoryOperation::ShowSession => {
                    assert!(envelope.session.is_some(), "{:?}", entry.path());
                }
                AgentHistoryOperation::Error => {
                    assert!(envelope.error.is_some(), "{:?}", entry.path())
                }
            }
            seen += 1;
        }
        assert!(seen > 0, "expected shared agent-history-v1 fixtures");
    }

    #[test]
    fn preserves_additive_fields() {
        let fixture = r#"{
            "contractVersion": "agent-history-v1",
            "schemaVersion": 1,
            "operation": "status",
            "status": {
                "initialized": true,
                "localOnly": true,
                "futureField": {"enabled": true}
            },
            "futureEnvelopeField": "kept"
        }"#;
        let envelope: AgentHistoryEnvelope = serde_json::from_str(fixture).unwrap();
        let status = envelope.status.unwrap();
        assert_eq!(status.extra["futureField"]["enabled"], true);
        assert_eq!(envelope.extra["futureEnvelopeField"], "kept");
    }

    #[test]
    fn camelizes_private_cli_keys_recursively() {
        let raw = serde_json::json!({
            "payload_type": "search_results",
            "generated_at": "now",
            "results": [{
                "record_type": "event",
                "item_type": "event",
                "ctx_event_id": "event",
                "result_type": "event",
                "result_scope": "event",
                "citations": [{"target_type": "event", "source_path": "/tmp/session.jsonl"}]
            }]
        });
        let camel = camelize_object_keys(&raw);
        assert!(camel.get("payloadType").is_none());
        assert_eq!(camel["generatedAt"], "now");
        assert!(camel["results"][0].get("recordType").is_none());
        assert!(camel["results"][0].get("itemType").is_none());
        assert_eq!(camel["results"][0]["ctxEventId"], "event");
        assert_eq!(camel["results"][0]["resultType"], "event");
        assert_eq!(camel["results"][0]["citations"][0]["targetType"], "event");
        assert_eq!(
            camel["results"][0]["citations"][0]["sourcePath"],
            "/tmp/session.jsonl"
        );
    }
}
