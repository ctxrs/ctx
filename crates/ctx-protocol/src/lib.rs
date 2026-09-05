//! Experimental `agent-history-v1` contract types shared by in-repo ctx SDKs.
//!
//! These types describe the SDK product contract. They are not SQLite schema
//! types and are not a promise to preserve current CLI JSON internals.

use std::collections::{BTreeMap, BTreeSet};

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

pub const CONTRACT_VERSION: &str = "agent-history-v1";
pub const SCHEMA_VERSION: u16 = 1;
pub const MAX_SAFE_INTEGER: u64 = (1_u64 << 53) - 1;
pub const MAX_SAFE_STATUS_COUNTER: u64 = MAX_SAFE_INTEGER;
pub const MAX_MCP_TOOL_CALL_COMPONENT_BYTES: usize = 64 * 1024;
pub const MAX_MCP_EXCHANGE_IDENTITY_BYTES: usize = 64 * 1024;

fn deserialize_optional_status_counter<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<u64>::deserialize(deserializer)?;
    if value.is_some_and(|value| value > MAX_SAFE_STATUS_COUNTER) {
        return Err(serde::de::Error::custom(format!(
            "status counter exceeds maximum {MAX_SAFE_STATUS_COUNTER}"
        )));
    }
    Ok(value)
}

fn serialize_optional_status_counter<S>(
    value: &Option<u64>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if value.is_some_and(|value| value > MAX_SAFE_STATUS_COUNTER) {
        return Err(serde::ser::Error::custom(format!(
            "status counter exceeds maximum {MAX_SAFE_STATUS_COUNTER}"
        )));
    }
    value.serialize(serializer)
}

fn deserialize_mcp_tool_call_component<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    validate_mcp_tool_call_component(&value).map_err(serde::de::Error::custom)?;
    Ok(value)
}

fn serialize_mcp_tool_call_component<S>(value: &str, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    validate_mcp_tool_call_component(value).map_err(serde::ser::Error::custom)?;
    value.serialize(serializer)
}

fn validate_mcp_tool_call_component(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("MCP tool-call component must be nonempty".to_owned());
    }
    if value.len() > MAX_MCP_TOOL_CALL_COMPONENT_BYTES {
        return Err(format!(
            "MCP tool-call component exceeds {MAX_MCP_TOOL_CALL_COMPONENT_BYTES} decoded UTF-8 bytes"
        ));
    }
    Ok(())
}

fn deserialize_present_mcp_tool_call<'de, D>(
    deserializer: D,
) -> Result<Option<McpToolCall>, D::Error>
where
    D: Deserializer<'de>,
{
    McpToolCall::deserialize(deserializer).map(Some)
}

fn deserialize_present_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

fn deserialize_present_mcp_exchange<'de, D>(
    deserializer: D,
) -> Result<Option<McpExchange>, D::Error>
where
    D: Deserializer<'de>,
{
    McpExchange::deserialize(deserializer).map(Some)
}

fn validate_mcp_exchange_identity(field: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{field} must be nonempty"));
    }
    if value.len() > MAX_MCP_EXCHANGE_IDENTITY_BYTES {
        return Err(format!(
            "{field} exceeds {MAX_MCP_EXCHANGE_IDENTITY_BYTES} decoded UTF-8 bytes"
        ));
    }
    Ok(())
}

fn validate_optional_safe_integer(field: &str, value: Option<u64>) -> Result<(), String> {
    if value.is_some_and(|value| value > MAX_SAFE_INTEGER) {
        return Err(format!("{field} exceeds maximum {MAX_SAFE_INTEGER}"));
    }
    Ok(())
}

struct ExactJsonValue(Value);

impl<'de> Deserialize<'de> for ExactJsonValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(ExactJsonValueVisitor)
    }
}

struct ExactJsonValueVisitor;

impl<'de> Visitor<'de> for ExactJsonValueVisitor {
    type Value = ExactJsonValue;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value without duplicate object members")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(ExactJsonValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(ExactJsonValue(Value::Number(value.into())))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(ExactJsonValue(Value::Number(value.into())))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .map(ExactJsonValue)
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(ExactJsonValue(Value::String(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(ExactJsonValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(ExactJsonValue(Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(ExactJsonValue(Value::Null))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(ExactJsonValue(value)) = sequence.next_element()? {
            values.push(value);
        }
        Ok(ExactJsonValue(Value::Array(values)))
    }

    fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut members = BTreeSet::new();
        let mut values = Map::new();
        while let Some(member) = object.next_key::<String>()? {
            if !members.insert(member.clone()) {
                return Err(de::Error::custom(format!(
                    "duplicate JSON object member {member:?}"
                )));
            }
            let ExactJsonValue(value) = object.next_value()?;
            values.insert(member, value);
        }
        Ok(ExactJsonValue(Value::Object(values)))
    }
}

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
    pub read_only: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_root: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_status_counter",
        serialize_with = "serialize_optional_status_counter",
        skip_serializing_if = "Option::is_none"
    )]
    pub indexed_items: Option<u64>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_status_counter",
        serialize_with = "serialize_optional_status_counter",
        skip_serializing_if = "Option::is_none"
    )]
    pub indexed_sessions: Option<u64>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_status_counter",
        serialize_with = "serialize_optional_status_counter",
        skip_serializing_if = "Option::is_none"
    )]
    pub indexed_events: Option<u64>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_status_counter",
        serialize_with = "serialize_optional_status_counter",
        skip_serializing_if = "Option::is_none"
    )]
    pub indexed_sources: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_epoch: Option<JsonObject>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lexical: Option<JsonObject>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<JsonObject>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic: Option<JsonObject>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon: Option<JsonObject>,
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
    pub status_reason: Option<String>,
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
    pub source_format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
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
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreContentPolicyStatus {
    Selected,
    Redacted,
    Omitted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoreContentMetadata {
    pub complete: bool,
    pub policy_status: CoreContentPolicyStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_reason: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

mod mcp;
pub use mcp::*;
#[derive(Debug, Clone, PartialEq)]
pub struct AgentHistoryEvent {
    pub ctx_event_id: Option<String>,
    pub ctx_session_id: Option<String>,
    pub provider: Option<String>,
    pub provider_session_id: Option<String>,
    pub source_format: Option<String>,
    pub sequence: Option<u64>,
    pub event_type: Option<String>,
    pub role: Option<String>,
    pub occurred_at: Option<String>,
    pub text: Option<String>,
    pub mcp_tool_call: Option<McpToolCall>,
    pub mcp_exchange: Option<McpExchange>,
    pub structured_content: Option<Value>,
    pub content: Option<CoreContentMetadata>,
    pub citations: Vec<Citation>,
    pub extra: JsonObject,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentHistoryEventWire {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ctx_event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ctx_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sequence: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    event_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    occurred_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_present_mcp_tool_call",
        skip_serializing_if = "Option::is_none"
    )]
    mcp_tool_call: Option<McpToolCall>,
    #[serde(
        default,
        deserialize_with = "deserialize_present_mcp_exchange",
        skip_serializing_if = "Option::is_none"
    )]
    mcp_exchange: Option<McpExchange>,
    #[serde(
        default,
        deserialize_with = "deserialize_present_json",
        skip_serializing_if = "Option::is_none"
    )]
    structured_content: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content: Option<CoreContentMetadata>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    citations: Vec<Citation>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    extra: JsonObject,
}

fn deserialize_present_json<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> Result<Option<Value>, D::Error> {
    Value::deserialize(decoder).map(Some)
}

impl AgentHistoryEvent {
    fn validate(&self) -> Result<(), String> {
        let has_normalized_body = self
            .mcp_exchange
            .as_ref()
            .and_then(|exchange| exchange.response.as_ref())
            .is_some_and(|response| response.text == McpTextCapture::NormalizedBody);
        if has_normalized_body && self.text.as_deref().is_none_or(|text| text.is_empty()) {
            return Err("normalized MCP response body requires nonempty event text".to_owned());
        }
        Ok(())
    }
}

impl From<AgentHistoryEventWire> for AgentHistoryEvent {
    fn from(wire: AgentHistoryEventWire) -> Self {
        Self {
            ctx_event_id: wire.ctx_event_id,
            ctx_session_id: wire.ctx_session_id,
            provider: wire.provider,
            provider_session_id: wire.provider_session_id,
            source_format: wire.source_format,
            sequence: wire.sequence,
            event_type: wire.event_type,
            role: wire.role,
            occurred_at: wire.occurred_at,
            text: wire.text,
            mcp_tool_call: wire.mcp_tool_call,
            mcp_exchange: wire.mcp_exchange,
            structured_content: wire.structured_content,
            content: wire.content,
            citations: wire.citations,
            extra: wire.extra,
        }
    }
}

impl From<&AgentHistoryEvent> for AgentHistoryEventWire {
    fn from(event: &AgentHistoryEvent) -> Self {
        Self {
            ctx_event_id: event.ctx_event_id.clone(),
            ctx_session_id: event.ctx_session_id.clone(),
            provider: event.provider.clone(),
            provider_session_id: event.provider_session_id.clone(),
            source_format: event.source_format.clone(),
            sequence: event.sequence,
            event_type: event.event_type.clone(),
            role: event.role.clone(),
            occurred_at: event.occurred_at.clone(),
            text: event.text.clone(),
            mcp_tool_call: event.mcp_tool_call.clone(),
            mcp_exchange: event.mcp_exchange.clone(),
            structured_content: event.structured_content.clone(),
            content: event.content.clone(),
            citations: event.citations.clone(),
            extra: event.extra.clone(),
        }
    }
}

impl<'de> Deserialize<'de> for AgentHistoryEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let event = Self::from(AgentHistoryEventWire::deserialize(deserializer)?);
        event.validate().map_err(serde::de::Error::custom)?;
        Ok(event)
    }
}

impl Serialize for AgentHistoryEvent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.validate().map_err(serde::ser::Error::custom)?;
        AgentHistoryEventWire::from(self).serialize(serializer)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<AgentHistoryEvent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<AgentHistoryEvent>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctx_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_format: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: JsonObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<AgentHistoryEvent>,
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

/// Converts only the ctx-owned outer keys, leaving values opaque.
pub fn camelize_envelope_keys(object: &Map<String, Value>) -> Map<String, Value> {
    object
        .iter()
        .filter_map(|(key, value)| {
            let key = snake_to_camel(key);
            (!omitted_public_key(&key)).then(|| (key, value.clone()))
        })
        .collect()
}

fn omitted_public_key(key: &str) -> bool {
    matches!(
        key,
        "itemType" | "payloadType" | "recordType" | "configPath"
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
mod tests;
