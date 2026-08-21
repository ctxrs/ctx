use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentScope, CoreActivity, EventRole, EventType, ProviderDeclaredFact,
    ProviderNativeSessionRelationship, SessionStatus,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct DirectJsonlSession {
    pub(crate) native_session_id: String,
    pub(crate) provider_session_id: String,
    pub(crate) parent_provider_session_id: Option<String>,
    pub(crate) root_provider_session_id: Option<String>,
    pub(crate) external_agent_id: Option<String>,
    pub(crate) agent_scope: Option<AgentScope>,
    pub(crate) session_relationship: Option<ProviderNativeSessionRelationship>,
    pub(crate) status: SessionStatus,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) ended_at: Option<DateTime<Utc>>,
    pub(crate) cwd: Option<String>,
    pub(crate) metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct DirectJsonlEvent {
    pub(crate) raw_ordinal: u64,
    pub(crate) sub_ordinal: u32,
    pub(crate) native_record_id: Option<String>,
    #[serde(default)]
    pub(crate) native_parent_id: Option<String>,
    pub(crate) stable_retry_discriminator: Option<DirectJsonlRetryDiscriminator>,
    pub(crate) provider_event_sequence_index: u64,
    pub(crate) provider_event_hash: String,
    pub(crate) event_type: EventType,
    pub(crate) role: EventRole,
    pub(crate) occurred_at: DateTime<Utc>,
    #[serde(skip, default)]
    pub(crate) activity: Option<CoreActivity>,
    #[serde(skip)]
    pub(crate) lexical_text: String,
    pub(crate) metadata: Value,
    pub(crate) facts: Vec<ProviderDeclaredFact>,
    pub(crate) native_value: Value,
    #[serde(skip, default)]
    pub(crate) source_record: DirectJsonlSourceRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum DirectJsonlRetryDiscriminator {
    FactoryDroidToolResult { tool_use_id: String },
    FactoryDroidRepeatedRecord {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_id: Option<String>,
        #[serde(default, skip_serializing_if = "is_zero")]
        occurrence: u32,
    },
}

const fn is_zero(value: &u32) -> bool {
    *value == 0
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DirectJsonlSourceRecord {
    pub(crate) byte_start: u64,
    pub(crate) byte_end_exclusive: u64,
    pub(crate) record_digest: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DirectJsonlRejection {
    pub(crate) raw_ordinal: u64,
    pub(crate) byte_start: u64,
    pub(crate) byte_end_exclusive: u64,
    pub(crate) reason: String,
}
