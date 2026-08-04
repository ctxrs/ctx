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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpToolCall {
    #[serde(
        deserialize_with = "deserialize_mcp_tool_call_component",
        serialize_with = "serialize_mcp_tool_call_component"
    )]
    pub server: String,
    #[serde(
        deserialize_with = "deserialize_mcp_tool_call_component",
        serialize_with = "serialize_mcp_tool_call_component"
    )]
    pub tool: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpExchange {
    pub provider_call_id: String,
    pub invocation: Option<McpInvocation>,
    pub response: Option<McpResponse>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpInvocation {
    pub server: String,
    pub tool: String,
    pub arguments: McpJsonCapture,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpResponse {
    pub status: McpResponseStatus,
    pub failure_kind: Option<McpFailureKind>,
    pub duration_ns: Option<u64>,
    pub text: McpTextCapture,
    pub payload: McpJsonCapture,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpJsonCapture {
    Present {
        value: Value,
    },
    Absent,
    Unavailable,
    Omitted {
        reason: McpPayloadOmissionReason,
        observed_encoded_bytes: Option<u64>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpTextCapture {
    NormalizedBody,
    Absent,
    Unavailable,
    Omitted {
        reason: McpPayloadOmissionReason,
        observed_encoded_bytes: Option<u64>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpPayloadOmissionReason {
    SizeLimit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpResponseStatus {
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpFailureKind {
    ToolReported,
    Invocation,
    Unknown,
}

impl<'de> Deserialize<'de> for McpJsonCapture {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        parse_mcp_json_capture(ExactJsonValue::deserialize(deserializer)?.0)
            .map_err(serde::de::Error::custom)
    }
}

impl Serialize for McpJsonCapture {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        validate_mcp_json_capture(self).map_err(serde::ser::Error::custom)?;
        mcp_json_capture_value(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for McpTextCapture {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        parse_mcp_text_capture(ExactJsonValue::deserialize(deserializer)?.0)
            .map_err(serde::de::Error::custom)
    }
}

impl Serialize for McpTextCapture {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        validate_mcp_text_capture(self).map_err(serde::ser::Error::custom)?;
        mcp_text_capture_value(self).serialize(serializer)
    }
}

fn parse_mcp_json_capture(value: Value) -> Result<McpJsonCapture, String> {
    let mut object = mcp_capture_object(value)?;
    let status = take_capture_status(&mut object)?;
    let capture = match status.as_str() {
        "present" => McpJsonCapture::Present {
            value: object
                .remove("value")
                .ok_or_else(|| "present MCP JSON capture requires value".to_owned())?,
        },
        "absent" => McpJsonCapture::Absent,
        "unavailable" => McpJsonCapture::Unavailable,
        "omitted" => {
            let (reason, observed_encoded_bytes) = take_omission_fields(&mut object)?;
            McpJsonCapture::Omitted {
                reason,
                observed_encoded_bytes,
            }
        }
        _ => return Err(format!("unknown MCP JSON captureStatus {status:?}")),
    };
    reject_remaining_capture_fields(&object)?;
    Ok(capture)
}

fn parse_mcp_text_capture(value: Value) -> Result<McpTextCapture, String> {
    let mut object = mcp_capture_object(value)?;
    let status = take_capture_status(&mut object)?;
    let capture = match status.as_str() {
        "normalized_body" => McpTextCapture::NormalizedBody,
        "absent" => McpTextCapture::Absent,
        "unavailable" => McpTextCapture::Unavailable,
        "omitted" => {
            let (reason, observed_encoded_bytes) = take_omission_fields(&mut object)?;
            McpTextCapture::Omitted {
                reason,
                observed_encoded_bytes,
            }
        }
        _ => return Err(format!("unknown MCP text captureStatus {status:?}")),
    };
    reject_remaining_capture_fields(&object)?;
    Ok(capture)
}

fn mcp_capture_object(value: Value) -> Result<Map<String, Value>, String> {
    match value {
        Value::Object(object) => Ok(object),
        _ => Err("MCP capture must be an object".to_owned()),
    }
}

fn take_capture_status(object: &mut Map<String, Value>) -> Result<String, String> {
    object
        .remove("captureStatus")
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| "MCP captureStatus must be a string".to_owned())
}

fn take_omission_fields(
    object: &mut Map<String, Value>,
) -> Result<(McpPayloadOmissionReason, Option<u64>), String> {
    let reason = match object
        .remove("reason")
        .and_then(|value| value.as_str().map(str::to_owned))
    {
        Some(reason) if reason == "size_limit" => McpPayloadOmissionReason::SizeLimit,
        Some(reason) => return Err(format!("unknown MCP capture omission reason {reason:?}")),
        None => return Err("omitted MCP capture requires string reason".to_owned()),
    };
    let observed_encoded_bytes = object
        .remove("observedEncodedBytes")
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| "observedEncodedBytes must be an unsigned integer".to_owned())
        })
        .transpose()?;
    validate_optional_safe_integer("observedEncodedBytes", observed_encoded_bytes)?;
    Ok((reason, observed_encoded_bytes))
}

fn validate_mcp_json_capture(capture: &McpJsonCapture) -> Result<(), String> {
    if let McpJsonCapture::Omitted {
        observed_encoded_bytes,
        ..
    } = capture
    {
        validate_optional_safe_integer("observedEncodedBytes", *observed_encoded_bytes)?;
    }
    Ok(())
}

fn validate_mcp_text_capture(capture: &McpTextCapture) -> Result<(), String> {
    if let McpTextCapture::Omitted {
        observed_encoded_bytes,
        ..
    } = capture
    {
        validate_optional_safe_integer("observedEncodedBytes", *observed_encoded_bytes)?;
    }
    Ok(())
}

fn reject_remaining_capture_fields(object: &Map<String, Value>) -> Result<(), String> {
    match object.keys().next() {
        Some(key) => Err(format!("MCP capture contains unknown member {key:?}")),
        None => Ok(()),
    }
}

fn mcp_json_capture_value(capture: &McpJsonCapture) -> Value {
    let mut object = Map::new();
    match capture {
        McpJsonCapture::Present { value } => {
            object.insert(
                "captureStatus".to_owned(),
                Value::String("present".to_owned()),
            );
            object.insert("value".to_owned(), value.clone());
        }
        McpJsonCapture::Absent => {
            object.insert(
                "captureStatus".to_owned(),
                Value::String("absent".to_owned()),
            );
        }
        McpJsonCapture::Unavailable => {
            object.insert(
                "captureStatus".to_owned(),
                Value::String("unavailable".to_owned()),
            );
        }
        McpJsonCapture::Omitted {
            reason,
            observed_encoded_bytes,
        } => insert_omitted_capture_fields(&mut object, *reason, *observed_encoded_bytes),
    }
    Value::Object(object)
}

fn mcp_text_capture_value(capture: &McpTextCapture) -> Value {
    let mut object = Map::new();
    match capture {
        McpTextCapture::NormalizedBody => {
            object.insert(
                "captureStatus".to_owned(),
                Value::String("normalized_body".to_owned()),
            );
        }
        McpTextCapture::Absent => {
            object.insert(
                "captureStatus".to_owned(),
                Value::String("absent".to_owned()),
            );
        }
        McpTextCapture::Unavailable => {
            object.insert(
                "captureStatus".to_owned(),
                Value::String("unavailable".to_owned()),
            );
        }
        McpTextCapture::Omitted {
            reason,
            observed_encoded_bytes,
        } => insert_omitted_capture_fields(&mut object, *reason, *observed_encoded_bytes),
    }
    Value::Object(object)
}

fn insert_omitted_capture_fields(
    object: &mut Map<String, Value>,
    reason: McpPayloadOmissionReason,
    observed_encoded_bytes: Option<u64>,
) {
    object.insert(
        "captureStatus".to_owned(),
        Value::String("omitted".to_owned()),
    );
    let reason = match reason {
        McpPayloadOmissionReason::SizeLimit => "size_limit",
    };
    object.insert("reason".to_owned(), Value::String(reason.to_owned()));
    if let Some(observed_encoded_bytes) = observed_encoded_bytes {
        object.insert(
            "observedEncodedBytes".to_owned(),
            Value::Number(observed_encoded_bytes.into()),
        );
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct McpExchangeWire {
    provider_call_id: String,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    invocation: Option<McpInvocation>,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    response: Option<McpResponse>,
}

impl McpExchange {
    fn validate(&self) -> Result<(), String> {
        validate_mcp_exchange_identity("MCP exchange providerCallId", &self.provider_call_id)?;
        if self.invocation.is_none() && self.response.is_none() {
            return Err("MCP exchange requires invocation, response, or both".to_owned());
        }
        if let Some(invocation) = &self.invocation {
            invocation.validate()?;
        }
        if let Some(response) = &self.response {
            response.validate()?;
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for McpExchange {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = McpExchangeWire::deserialize(deserializer)?;
        let value = Self {
            provider_call_id: wire.provider_call_id,
            invocation: wire.invocation,
            response: wire.response,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

impl Serialize for McpExchange {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.validate().map_err(serde::ser::Error::custom)?;
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire<'a> {
            provider_call_id: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            invocation: &'a Option<McpInvocation>,
            #[serde(skip_serializing_if = "Option::is_none")]
            response: &'a Option<McpResponse>,
        }
        Wire {
            provider_call_id: &self.provider_call_id,
            invocation: &self.invocation,
            response: &self.response,
        }
        .serialize(serializer)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct McpInvocationWire {
    server: String,
    tool: String,
    arguments: McpJsonCapture,
}

impl McpInvocation {
    fn validate(&self) -> Result<(), String> {
        validate_mcp_exchange_identity("MCP invocation server", &self.server)?;
        validate_mcp_exchange_identity("MCP invocation tool", &self.tool)?;
        if matches!(
            &self.arguments,
            McpJsonCapture::Present { value } if !value.is_object()
        ) {
            return Err("present MCP invocation arguments must be a JSON object".to_owned());
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for McpInvocation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = McpInvocationWire::deserialize(deserializer)?;
        let value = Self {
            server: wire.server,
            tool: wire.tool,
            arguments: wire.arguments,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

impl Serialize for McpInvocation {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.validate().map_err(serde::ser::Error::custom)?;
        #[derive(Serialize)]
        struct Wire<'a> {
            server: &'a str,
            tool: &'a str,
            arguments: &'a McpJsonCapture,
        }
        Wire {
            server: &self.server,
            tool: &self.tool,
            arguments: &self.arguments,
        }
        .serialize(serializer)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct McpResponseWire {
    status: McpResponseStatus,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    failure_kind: Option<McpFailureKind>,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    duration_ns: Option<u64>,
    text: McpTextCapture,
    payload: McpJsonCapture,
}

impl McpResponse {
    fn validate(&self) -> Result<(), String> {
        if (self.status == McpResponseStatus::Failed) != self.failure_kind.is_some() {
            return Err(
                "MCP response failureKind must be present exactly when status is failed".to_owned(),
            );
        }
        validate_optional_safe_integer("MCP response durationNs", self.duration_ns)?;
        validate_mcp_text_capture(&self.text)?;
        validate_mcp_json_capture(&self.payload)?;
        Ok(())
    }
}

impl<'de> Deserialize<'de> for McpResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = McpResponseWire::deserialize(deserializer)?;
        let value = Self {
            status: wire.status,
            failure_kind: wire.failure_kind,
            duration_ns: wire.duration_ns,
            text: wire.text,
            payload: wire.payload,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

impl Serialize for McpResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.validate().map_err(serde::ser::Error::custom)?;
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire<'a> {
            status: McpResponseStatus,
            #[serde(skip_serializing_if = "Option::is_none")]
            failure_kind: &'a Option<McpFailureKind>,
            #[serde(skip_serializing_if = "Option::is_none")]
            duration_ns: &'a Option<u64>,
            text: &'a McpTextCapture,
            payload: &'a McpJsonCapture,
        }
        Wire {
            status: self.status,
            failure_kind: &self.failure_kind,
            duration_ns: &self.duration_ns,
            text: &self.text,
            payload: &self.payload,
        }
        .serialize(serializer)
    }
}

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    structured_content: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content: Option<CoreContentMetadata>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    citations: Vec<Citation>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    extra: JsonObject,
}

impl AgentHistoryEvent {
    fn validate(&self) -> Result<(), String> {
        let has_normalized_body = self
            .mcp_exchange
            .as_ref()
            .and_then(|exchange| exchange.response.as_ref())
            .is_some_and(|response| response.text == McpTextCapture::NormalizedBody);
        if has_normalized_body && !self.text.as_deref().is_some_and(|text| !text.is_empty()) {
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
                    if let Some(hit) = search.results.first() {
                        assert_eq!(hit.provider.as_deref(), Some("codex"));
                        assert_eq!(
                            hit.provider_session_id.as_deref(),
                            Some("codex-fixture-session")
                        );
                        assert_eq!(hit.source_format.as_deref(), Some("codex_session_jsonl"));
                    }
                }
                AgentHistoryOperation::ShowEvent => {
                    let event = envelope
                        .event
                        .as_ref()
                        .and_then(|result| result.event.as_ref())
                        .expect("show-event fixture selected event");
                    assert_eq!(event.provider.as_deref(), Some("codex"));
                    assert_eq!(
                        event.provider_session_id.as_deref(),
                        Some("codex-fixture-session")
                    );
                    assert_eq!(event.source_format.as_deref(), Some("codex_session_jsonl"));
                    assert_eq!(
                        event.structured_content.as_ref().unwrap()["kind"],
                        "toolResult"
                    );
                    assert_eq!(
                        event.structured_content.as_ref().unwrap()["payload"]["items"][2]["nested"]
                            [1],
                        false
                    );
                    assert_eq!(
                        event
                            .content
                            .as_ref()
                            .map(|content| (&content.policy_status, content.complete)),
                        Some((&CoreContentPolicyStatus::Selected, true))
                    );
                }
                AgentHistoryOperation::ShowSession => {
                    let summary = envelope
                        .session
                        .as_ref()
                        .and_then(|result| result.session.as_ref())
                        .expect("show-session fixture summary");
                    assert_eq!(summary.provider.as_deref(), Some("codex"));
                    assert_eq!(
                        summary.provider_session_id.as_deref(),
                        Some("codex-fixture-session")
                    );
                    assert_eq!(
                        summary.source_format.as_deref(),
                        Some("codex_session_jsonl")
                    );
                    assert_eq!(
                        envelope.session.as_ref().unwrap().events[0]
                            .structured_content
                            .as_ref()
                            .unwrap()[1]["complete"],
                        true
                    );
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
    fn mcp_tool_call_is_exact_bounded_and_omitted_when_absent() {
        let fixture =
            fs::read_to_string(fixture_root().join("show-event.mcp-tool-call.json")).unwrap();
        let envelope: AgentHistoryEnvelope = serde_json::from_str(&fixture).unwrap();
        let result = envelope.event.unwrap();
        let selected = result.event.unwrap();
        let mcp_tool_call = selected.mcp_tool_call.as_ref().unwrap();

        assert_eq!(mcp_tool_call.server, "mcp-サーバー-🦀");
        assert_eq!(mcp_tool_call.tool, "検索/工具/🛠️");
        assert_eq!(selected.extra["futureEventField"]["preserved"], true);

        let encoded = serde_json::to_value(&selected).unwrap();
        assert_eq!(encoded["mcpToolCall"]["server"], "mcp-サーバー-🦀");
        assert_eq!(encoded["mcpToolCall"]["tool"], "検索/工具/🛠️");
        assert!(encoded["mcpToolCall"].get("futureLabel").is_none());
        assert_eq!(encoded["futureEventField"]["preserved"], true);

        let without_metadata = serde_json::to_value(&result.events[0]).unwrap();
        assert!(without_metadata.get("mcpToolCall").is_none());

        for invalid in [
            serde_json::json!({"server": "only-server"}),
            serde_json::json!({"tool": "only-tool"}),
            serde_json::json!({"server": "", "tool": "tool"}),
            serde_json::json!({"server": "server", "tool": ""}),
            serde_json::json!({"server": "server", "tool": "tool", "futureLabel": true}),
            serde_json::json!({
                "server": "server",
                "tool": "a".repeat(MAX_MCP_TOOL_CALL_COMPONENT_BYTES + 1)
            }),
        ] {
            assert!(serde_json::from_value::<McpToolCall>(invalid).is_err());
        }
        assert!(serde_json::from_value::<AgentHistoryEvent>(
            serde_json::json!({"mcpToolCall": null})
        )
        .is_err());

        let exact = serde_json::json!({
            "server": " ",
            "tool": "🦀".repeat(MAX_MCP_TOOL_CALL_COMPONENT_BYTES / 4)
        });
        let exact: McpToolCall = serde_json::from_value(exact).unwrap();
        assert_eq!(exact.tool.len(), MAX_MCP_TOOL_CALL_COMPONENT_BYTES);

        let invalid_for_encoding = McpToolCall {
            server: "server".to_owned(),
            tool: String::new(),
        };
        assert!(serde_json::to_value(invalid_for_encoding).is_err());
    }

    #[test]
    fn mcp_exchange_is_typed_lossless_bounded_and_shape_validated() {
        let fixture =
            fs::read_to_string(fixture_root().join("show-event.mcp-tool-call.json")).unwrap();
        let envelope: AgentHistoryEnvelope = serde_json::from_str(&fixture).unwrap();
        let result = envelope.event.unwrap();
        let selected = result.event.unwrap();
        let exchange = selected.mcp_exchange.as_ref().unwrap();
        assert_eq!(exchange.provider_call_id, "native-call-呼び出し-🦀");

        let invocation = exchange.invocation.as_ref().unwrap();
        let McpJsonCapture::Present { value: arguments } = &invocation.arguments else {
            panic!("fixture arguments must be present");
        };
        assert_eq!(arguments["snake_key"][0], "雪");
        assert!(arguments["snake_key"][1].is_null());
        assert!(arguments["nested"]["items"][1]["deep_null"].is_null());

        let response = exchange.response.as_ref().unwrap();
        assert_eq!(response.status, McpResponseStatus::Succeeded);
        assert_eq!(response.duration_ns, Some(MAX_SAFE_INTEGER));
        assert_eq!(response.text, McpTextCapture::NormalizedBody);
        let McpJsonCapture::Present { value: payload } = &response.payload else {
            panic!("fixture payload must be present");
        };
        assert_eq!(payload["result_key"][0], "完了");
        assert!(payload["result_key"][1].is_null());

        let encoded = serde_json::to_value(&selected).unwrap();
        assert_eq!(
            encoded["mcpExchange"]["invocation"]["arguments"]["value"],
            *arguments
        );
        assert_eq!(
            encoded["mcpExchange"]["response"]["payload"]["value"],
            *payload
        );
        assert!(encoded.get("mcp_exchange").is_none());

        assert!(result.events[0].mcp_exchange.is_none());
        let capture_states = result.events[1].mcp_exchange.as_ref().unwrap();
        assert_eq!(
            capture_states.invocation.as_ref().unwrap().arguments,
            McpJsonCapture::Absent
        );
        let capture_response = capture_states.response.as_ref().unwrap();
        assert_eq!(capture_response.text, McpTextCapture::Absent);
        assert_eq!(capture_response.payload, McpJsonCapture::Unavailable);

        let omitted = result.events[2].mcp_exchange.as_ref().unwrap();
        let omitted_response = omitted.response.as_ref().unwrap();
        assert_eq!(omitted_response.status, McpResponseStatus::Failed);
        assert_eq!(
            omitted_response.failure_kind,
            Some(McpFailureKind::ToolReported)
        );
        assert_eq!(
            omitted_response.text,
            McpTextCapture::Omitted {
                reason: McpPayloadOmissionReason::SizeLimit,
                observed_encoded_bytes: Some(MAX_SAFE_INTEGER),
            }
        );
        assert_eq!(
            omitted_response.payload,
            McpJsonCapture::Omitted {
                reason: McpPayloadOmissionReason::SizeLimit,
                observed_encoded_bytes: None,
            }
        );
        let encoded_omitted = serde_json::to_value(&result.events[2]).unwrap();
        assert_eq!(
            encoded_omitted["mcpExchange"]["response"]["text"]["observedEncodedBytes"],
            MAX_SAFE_INTEGER
        );
        assert!(encoded_omitted["mcpExchange"]["response"]["text"]
            .get("observed_encoded_bytes")
            .is_none());
        assert!(result.events[3].mcp_exchange.is_none());

        for (index, invalid) in [
            serde_json::json!({"mcpExchange": null}),
            serde_json::json!({"mcpExchange": {"providerCallId": "call"}}),
            serde_json::json!({
                "mcpExchange": {
                    "providerCallId": "",
                    "response": {
                        "status": "succeeded",
                        "text": {"captureStatus": "absent"},
                        "payload": {"captureStatus": "absent"}
                    }
                }
            }),
            serde_json::json!({
                "mcpExchange": {
                    "providerCallId": "call",
                    "response": {
                        "status": "succeeded",
                        "durationNs": MAX_SAFE_INTEGER + 1,
                        "text": {"captureStatus": "absent"},
                        "payload": {"captureStatus": "absent"}
                    }
                }
            }),
            serde_json::json!({
                "mcpExchange": {
                    "providerCallId": "call",
                    "response": {
                        "status": "succeeded",
                        "text": {
                            "captureStatus": "omitted",
                            "reason": "size_limit",
                            "observedEncodedBytes": MAX_SAFE_INTEGER + 1
                        },
                        "payload": {"captureStatus": "absent"}
                    }
                }
            }),
            serde_json::json!({
                "mcpExchange": {
                    "providerCallId": "call",
                    "invocation": null,
                    "response": {
                        "status": "succeeded",
                        "text": {"captureStatus": "absent"},
                        "payload": {"captureStatus": "absent"}
                    }
                }
            }),
            serde_json::json!({
                "mcpExchange": {
                    "providerCallId": "call",
                    "invocation": {
                        "server": "server",
                        "tool": "tool",
                        "arguments": {"captureStatus": "present", "value": null}
                    }
                }
            }),
            serde_json::json!({
                "mcpExchange": {
                    "providerCallId": "call",
                    "response": {
                        "status": "failed",
                        "text": {"captureStatus": "absent"},
                        "payload": {"captureStatus": "absent"}
                    }
                }
            }),
            serde_json::json!({
                "mcpExchange": {
                    "providerCallId": "call",
                    "response": {
                        "status": "succeeded",
                        "failureKind": "unknown",
                        "text": {"captureStatus": "absent"},
                        "payload": {"captureStatus": "absent"}
                    }
                }
            }),
            serde_json::json!({
                "mcpExchange": {
                    "providerCallId": "call",
                    "response": {
                        "status": "succeeded",
                        "text": {"captureStatus": "absent", "future": true},
                        "payload": {"captureStatus": "absent"}
                    }
                }
            }),
            serde_json::json!({
                "mcpExchange": {
                    "providerCallId": "call",
                    "response": {
                        "status": "succeeded",
                        "text": {"captureStatus": "absent"},
                        "payload": {"captureStatus": "absent"}
                    },
                    "future": true
                }
            }),
            serde_json::json!({
                "mcpExchange": {
                    "providerCallId": "x".repeat(MAX_MCP_EXCHANGE_IDENTITY_BYTES + 1),
                    "response": {
                        "status": "succeeded",
                        "text": {"captureStatus": "absent"},
                        "payload": {"captureStatus": "absent"}
                    }
                }
            }),
            serde_json::json!({
                "mcpExchange": {
                    "providerCallId": "call",
                    "invocation": {
                        "server": "x".repeat(MAX_MCP_EXCHANGE_IDENTITY_BYTES + 1),
                        "tool": "tool",
                        "arguments": {"captureStatus": "absent"}
                    }
                }
            }),
        ]
        .into_iter()
        .enumerate()
        {
            assert!(
                serde_json::from_value::<AgentHistoryEvent>(invalid.clone()).is_err(),
                "invalid MCP exchange fixture {index} decoded: {invalid}"
            );
        }

        let exact: McpExchange = serde_json::from_value(serde_json::json!({
            "providerCallId": "🦀".repeat(MAX_MCP_EXCHANGE_IDENTITY_BYTES / 4),
            "invocation": {
                "server": " ",
                "tool": "tool",
                "arguments": {"captureStatus": "absent"}
            }
        }))
        .unwrap();
        assert_eq!(
            exact.provider_call_id.len(),
            MAX_MCP_EXCHANGE_IDENTITY_BYTES
        );

        let invalid_for_encoding = McpExchange {
            provider_call_id: "call".to_owned(),
            invocation: None,
            response: None,
        };
        assert!(serde_json::to_value(invalid_for_encoding).is_err());
    }

    #[test]
    fn mcp_exchange_direct_decode_rejects_duplicate_captured_json_and_bad_event_text() {
        for name in [
            "duplicate-mcp-exchange-captured-value.json",
            "invalid-mcp-exchange-normalized-body-missing-event-text.json",
            "invalid-mcp-exchange-normalized-body-empty-event-text.json",
            "invalid-mcp-exchange-unsafe-duration-ns.json",
            "invalid-mcp-exchange-unsafe-observed-encoded-bytes.json",
        ] {
            let fixture =
                fs::read_to_string(fixture_root().join("adversarial").join(name)).unwrap();
            assert!(
                serde_json::from_str::<EventResult>(&fixture).is_err(),
                "direct protocol decode accepted {name}"
            );
        }
    }

    #[test]
    fn status_counters_accept_the_exact_cross_sdk_maximum() {
        let status: AgentHistoryStatus = serde_json::from_value(serde_json::json!({
            "initialized": true,
            "localOnly": true,
            "indexedItems": MAX_SAFE_STATUS_COUNTER,
            "indexedSessions": MAX_SAFE_STATUS_COUNTER,
            "indexedEvents": MAX_SAFE_STATUS_COUNTER,
            "indexedSources": MAX_SAFE_STATUS_COUNTER
        }))
        .unwrap();

        assert_eq!(status.indexed_items, Some(MAX_SAFE_STATUS_COUNTER));
        assert_eq!(status.indexed_sessions, Some(MAX_SAFE_STATUS_COUNTER));
        assert_eq!(status.indexed_events, Some(MAX_SAFE_STATUS_COUNTER));
        assert_eq!(status.indexed_sources, Some(MAX_SAFE_STATUS_COUNTER));
        serde_json::to_value(status).unwrap();
    }

    #[test]
    fn status_counters_reject_values_above_the_exact_cross_sdk_maximum() {
        for rejected in [MAX_SAFE_STATUS_COUNTER + 2, u64::MAX] {
            let error = serde_json::from_value::<AgentHistoryStatus>(serde_json::json!({
                "initialized": true,
                "localOnly": true,
                "indexedItems": rejected
            }))
            .unwrap_err();
            assert!(
                error.to_string().contains("status counter exceeds maximum"),
                "{error}"
            );
        }

        let mut status: AgentHistoryStatus = serde_json::from_value(serde_json::json!({
            "initialized": true,
            "localOnly": true
        }))
        .unwrap();
        status.indexed_items = Some(MAX_SAFE_STATUS_COUNTER + 2);
        let error = serde_json::to_value(status).unwrap_err();
        assert!(
            error.to_string().contains("status counter exceeds maximum"),
            "{error}"
        );
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
                "source_format": "codex_session_jsonl",
                "citations": [{
                    "target_type": "event"
                }]
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
        assert_eq!(camel["results"][0]["sourceFormat"], "codex_session_jsonl");
    }
}
