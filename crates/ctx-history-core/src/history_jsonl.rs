use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    AgentScope, EventRole, EventType, Fidelity, ProviderArtifactDescriptor, ProviderCursorRange,
    ProviderNativeSessionRelationship, ProviderSourceTrust, SessionStatus,
};

pub const CTX_HISTORY_JSONL_SCHEMA_VERSION: &str = "ctx-history-jsonl-v2";

/// Optional lineage extension carried by a provider-owned Custom History file.
///
/// Absence means the producer declares no provider-native relationship or copy
/// contract. Core does not synthesize one from ordering or equal content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CtxHistoryJsonlLineageContract {
    ProviderNativeV1,
}

/// Exact proof kinds a generic Custom History producer may declare.
///
/// Prefix equality and content equality are deliberately not proof kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CtxHistoryJsonlCopyProofKind {
    NativeEventIdentity,
    NativeCopiedFromField,
    NativeCallResultIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CtxHistoryJsonlCopiedFromSelector {
    pub ancestor_provider_session_id: String,
    pub ancestor_event_id: String,
    pub proof: CtxHistoryJsonlCopyProofKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "record_type", rename_all = "snake_case")]
pub enum CtxHistoryJsonlRecord {
    Manifest(CtxHistoryJsonlManifestRecord),
    Source(CtxHistoryJsonlSourceRecord),
    Session(CtxHistoryJsonlSessionRecord),
    Event(CtxHistoryJsonlEventRecord),
    FileReference(CtxHistoryJsonlFileReferenceRecord),
    Edge(CtxHistoryJsonlEdgeRecord),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CtxHistoryJsonlManifestRecord {
    pub schema_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage_contract: Option<CtxHistoryJsonlLineageContract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exported_at: Option<DateTime<Utc>>,
    #[serde(default = "super::default_metadata")]
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    pub trust: ProviderSourceTrust,
    #[serde(default = "default_imported_fidelity")]
    pub fidelity: Fidelity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ProviderCursorRange>,
    #[serde(default = "super::default_metadata")]
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CtxHistoryJsonlSessionRecord {
    pub source_id: String,
    pub provider_session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_provider_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_provider_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_relationship: Option<ProviderNativeSessionRelationship>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_scope: Option<AgentScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_hint: Option<String>,
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
#[serde(deny_unknown_fields)]
pub struct CtxHistoryJsonlEventRecord {
    pub source_id: String,
    pub provider_session_id: String,
    pub event_index: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copied_from: Option<CtxHistoryJsonlCopiedFromSelector>,
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
#[serde(deny_unknown_fields)]
pub struct CtxHistoryJsonlFileReferenceRecord {
    pub source_id: String,
    pub provider_session_id: String,
    pub reference_index: u64,
    pub event_index: u64,
    /// Exact provider-declared file string; no normalization is performed.
    pub value: String,
    pub occurred_at: DateTime<Utc>,
    #[serde(default = "super::default_metadata")]
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CtxHistoryJsonlEdgeRecord {
    pub source_id: String,
    pub from_provider_session_id: String,
    pub to_provider_session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relationship: Option<ProviderNativeSessionRelationship>,
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
        let raw = r#"{"record_type":"event","source_id":"src-1","provider_session_id":"sess-1","event_index":2,"event_id":"evt-2","native_cursor":"line:3","event_type":"message","role":"assistant","occurred_at":"2026-07-01T12:00:02Z","payload":{"text":"hello"},"preview":"hello"}"#;
        let parsed: CtxHistoryJsonlRecord = serde_json::from_str(raw).unwrap();
        let CtxHistoryJsonlRecord::Event(event) = parsed else {
            panic!("expected event record");
        };
        assert_eq!(event.source_id, "src-1");
        assert_eq!(event.provider_session_id, "sess-1");
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
    fn ctx_history_jsonl_edge_relationship_is_optional_without_a_fallback() {
        let raw = r#"{"record_type":"edge","source_id":"src-1","from_provider_session_id":"root","to_provider_session_id":"child"}"#;
        let CtxHistoryJsonlRecord::Edge(edge) =
            serde_json::from_str::<CtxHistoryJsonlRecord>(raw).unwrap()
        else {
            panic!("expected edge record");
        };
        assert!(edge.relationship.is_none());
    }

    #[test]
    fn lineage_extension_is_typed_and_legacy_records_remain_unset() {
        let without_copy = r#"{"record_type":"event","source_id":"src-1","provider_session_id":"sess-1","event_index":2,"event_id":"evt-2","event_type":"message","occurred_at":"2026-07-01T12:00:02Z"}"#;
        let CtxHistoryJsonlRecord::Event(without_copy) =
            serde_json::from_str::<CtxHistoryJsonlRecord>(without_copy).unwrap()
        else {
            panic!("expected event record");
        };
        assert!(without_copy.copied_from.is_none());

        let copied = r#"{"record_type":"event","source_id":"src-1","provider_session_id":"fork","event_index":2,"event_id":"evt-2","copied_from":{"ancestor_provider_session_id":"root","ancestor_event_id":"evt-1","proof":"native_event_identity"},"event_type":"message","occurred_at":"2026-07-01T12:00:02Z"}"#;
        let CtxHistoryJsonlRecord::Event(copied) =
            serde_json::from_str::<CtxHistoryJsonlRecord>(copied).unwrap()
        else {
            panic!("expected event record");
        };
        assert_eq!(
            copied.copied_from.unwrap().proof,
            CtxHistoryJsonlCopyProofKind::NativeEventIdentity
        );
    }

    #[test]
    fn copied_from_selector_rejects_unknown_fields_and_proof_kinds() {
        for raw in [
            r#"{"record_type":"event","source_id":"src-1","provider_session_id":"fork","event_index":2,"event_id":"evt-2","copied_from":{"ancestor_provider_session_id":"root","ancestor_event_id":"evt-1","proof":"native_event_identity","text":"private"},"occurred_at":"2026-07-01T12:00:02Z"}"#,
            r#"{"record_type":"event","source_id":"src-1","provider_session_id":"fork","event_index":2,"event_id":"evt-2","copied_from":{"ancestor_provider_session_id":"root","ancestor_event_id":"evt-1","proof":"certified_ordered_prefix"},"occurred_at":"2026-07-01T12:00:02Z"}"#,
        ] {
            assert!(serde_json::from_str::<CtxHistoryJsonlRecord>(raw).is_err());
        }
    }

    #[test]
    fn neutral_session_and_file_reference_reject_removed_semantics() {
        let invented_relationship = r#"{"record_type":"session","source_id":"src-1","provider_session_id":"child","session_relationship":"related_unknown","status":"imported","started_at":"2026-07-01T12:00:00Z"}"#;
        assert!(serde_json::from_str::<CtxHistoryJsonlRecord>(invented_relationship).is_err());

        let invented_scope = r#"{"record_type":"session","source_id":"src-1","provider_session_id":"child","agent_scope":"reviewer","status":"imported","started_at":"2026-07-01T12:00:00Z"}"#;
        assert!(serde_json::from_str::<CtxHistoryJsonlRecord>(invented_scope).is_err());

        for removed in [
            r#"{"record_type":"file_reference","source_id":"src-1","provider_session_id":"sess-1","reference_index":0,"value":"src/lib.rs","change_kind":"modified","occurred_at":"2026-07-01T12:00:00Z"}"#,
            r#"{"record_type":"file_reference","source_id":"src-1","provider_session_id":"sess-1","reference_index":0,"value":"src/lib.rs","confidence":"high","occurred_at":"2026-07-01T12:00:00Z"}"#,
            r#"{"record_type":"file_reference","source_id":"src-1","provider_session_id":"sess-1","reference_index":0,"value":"src/lib.rs","repository_binding":"repo-1","occurred_at":"2026-07-01T12:00:00Z"}"#,
        ] {
            assert!(serde_json::from_str::<CtxHistoryJsonlRecord>(removed).is_err());
        }
    }

    #[test]
    fn v2_rejects_removed_session_identity_fields_even_with_provider_session_id() {
        for raw in [
            r#"{"record_type":"session","source_id":"src-1","provider_session_id":"child","session_id":"legacy","started_at":"2026-07-01T12:00:00Z"}"#,
            r#"{"record_type":"session","source_id":"src-1","provider_session_id":"child","native_session_id":"legacy","started_at":"2026-07-01T12:00:00Z"}"#,
            r#"{"record_type":"event","source_id":"src-1","provider_session_id":"child","session_id":"legacy","event_index":0,"occurred_at":"2026-07-01T12:00:00Z"}"#,
            r#"{"record_type":"edge","source_id":"src-1","from_provider_session_id":"root","to_provider_session_id":"child","from_session_id":"legacy-root"}"#,
        ] {
            assert!(serde_json::from_str::<CtxHistoryJsonlRecord>(raw).is_err());
        }
    }
}
