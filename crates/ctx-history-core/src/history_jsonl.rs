use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    AgentType, Confidence, EventRole, EventType, Fidelity, FileChangeKind,
    ProviderArtifactDescriptor, ProviderCursorRange, ProviderRawRetention,
    ProviderRedactionBoundary, ProviderSourceTrust, RedactionState, SessionEdgeType, SessionStatus,
};

pub const CTX_HISTORY_JSONL_V1_SCHEMA_VERSION: &str = "ctx-history-jsonl-v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "record_type", rename_all = "snake_case")]
pub enum CtxHistoryJsonlRecord {
    Manifest(CtxHistoryJsonlManifestRecord),
    Source(CtxHistoryJsonlSourceRecord),
    Session(CtxHistoryJsonlSessionRecord),
    Event(CtxHistoryJsonlEventRecord),
    FileTouch(CtxHistoryJsonlFileTouchRecord),
    Edge(CtxHistoryJsonlEdgeRecord),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CtxHistoryJsonlManifestRecord {
    pub schema_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exported_at: Option<DateTime<Utc>>,
    #[serde(default = "super::default_metadata")]
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CtxHistoryJsonlSourceRecord {
    pub source_id: String,
    pub provider_key: String,
    pub source_format: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_source_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub importer_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    #[serde(default)]
    pub raw_retention: ProviderRawRetention,
    #[serde(default)]
    pub redaction_boundary: ProviderRedactionBoundary,
    #[serde(default)]
    pub trust: ProviderSourceTrust,
    #[serde(default = "default_imported_fidelity")]
    pub fidelity: Fidelity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ProviderCursorRange>,
    #[serde(default = "super::default_metadata")]
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CtxHistoryJsonlSessionRecord {
    pub source_id: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_agent_id: Option<String>,
    #[serde(default)]
    pub agent_type: AgentType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_hint: Option<String>,
    #[serde(default)]
    pub is_primary: bool,
    #[serde(default = "default_imported_session_status")]
    pub status: SessionStatus,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default = "default_imported_fidelity")]
    pub fidelity: Fidelity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ProviderArtifactDescriptor>,
    #[serde(default = "super::default_metadata")]
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CtxHistoryJsonlEventRecord {
    pub source_id: String,
    pub session_id: String,
    pub event_index: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_hash: Option<String>,
    #[serde(default)]
    pub event_type: EventType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<EventRole>,
    pub occurred_at: DateTime<Utc>,
    #[serde(default = "default_imported_fidelity")]
    pub fidelity: Fidelity,
    #[serde(default)]
    pub redaction_state: RedactionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ProviderArtifactDescriptor>,
    #[serde(default = "super::default_metadata")]
    pub payload: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(default = "super::default_metadata")]
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CtxHistoryJsonlFileTouchRecord {
    pub source_id: String,
    pub session_id: String,
    pub touch_index: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_index: Option<u64>,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_kind: Option<FileChangeKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_count_delta: Option<i64>,
    #[serde(default)]
    pub confidence: Confidence,
    pub occurred_at: DateTime<Utc>,
    #[serde(default = "super::default_metadata")]
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CtxHistoryJsonlEdgeRecord {
    pub source_id: String,
    pub from_session_id: String,
    pub to_session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_id: Option<String>,
    pub edge_type: SessionEdgeType,
    #[serde(default)]
    pub confidence: Confidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurred_at: Option<DateTime<Utc>>,
    #[serde(default = "default_imported_fidelity")]
    pub fidelity: Fidelity,
    #[serde(default = "super::default_metadata")]
    pub metadata: Value,
}

const fn default_imported_session_status() -> SessionStatus {
    SessionStatus::Imported
}

const fn default_imported_fidelity() -> Fidelity {
    Fidelity::Imported
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctx_history_jsonl_records_round_trip() {
        let raw = r#"{"record_type":"event","source_id":"src-1","session_id":"sess-1","event_index":2,"event_id":"evt-2","native_cursor":"line:3","event_type":"message","role":"assistant","occurred_at":"2026-07-01T12:00:02Z","payload":{"text":"hello"},"preview":"hello"}"#;
        let parsed: CtxHistoryJsonlRecord = serde_json::from_str(raw).unwrap();
        let CtxHistoryJsonlRecord::Event(event) = parsed else {
            panic!("expected event record");
        };
        assert_eq!(event.source_id, "src-1");
        assert_eq!(event.session_id, "sess-1");
        assert_eq!(event.event_index, 2);
        assert_eq!(event.role, Some(EventRole::Assistant));
        assert_eq!(
            serde_json::to_value(CtxHistoryJsonlRecord::Event(event))
                .unwrap()
                .get("record_type")
                .and_then(Value::as_str),
            Some("event")
        );
    }

    #[test]
    fn ctx_history_jsonl_edge_type_is_required() {
        let raw = r#"{"record_type":"edge","source_id":"src-1","from_session_id":"root","to_session_id":"child"}"#;
        let err = serde_json::from_str::<CtxHistoryJsonlRecord>(raw).unwrap_err();
        assert!(
            err.to_string().contains("missing field `edge_type`"),
            "{err}"
        );
    }
}
