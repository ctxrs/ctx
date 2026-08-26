use std::{borrow::Cow, fmt};

use chrono::{DateTime, Utc};
use serde::{
    de::{IgnoredAny, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use serde_json::Value;

use super::rows::{
    CodexSessionGitMetadata, CodexSessionRow, MAX_CODEX_DURABLE_CWD_BYTES,
    MAX_CODEX_DURABLE_METADATA_BYTES, MAX_CODEX_DURABLE_SESSION_ID_BYTES,
};
use crate::provider::codex::catalog::{codex_session_relationship, codex_source_kind};
use ctx_history_capture_model::time::parse_rfc3339_utc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CodexRetainedKind {
    Message,
    Reasoning,
    Compacted,
    ToolCall,
    /// The paginated thread-history completion envelope. Its nested TurnItem
    /// discriminator is decoded by the semantic projector, not this shallow
    /// envelope classifier.
    ItemCompleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CodexResultKind {
    FunctionCallOutput,
    CustomToolCallOutput,
    ToolSearchOutput,
    OtherResult,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CodexRecordClass {
    SessionMeta,
    TurnContext,
    DescendantActivity,
    DescendantStarted,
    Retained(CodexRetainedKind),
    ExcludedResult(CodexResultKind),
    Ignored,
}

#[derive(Debug)]
struct CodexText<'a> {
    value: Cow<'a, str>,
    escaped: bool,
}

impl CodexText<'_> {
    fn as_str(&self) -> &str {
        self.value.as_ref()
    }
}

impl<'de> Deserialize<'de> for CodexText<'de> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(CodexTextVisitor)
    }
}

struct CodexTextVisitor;

impl<'de> Visitor<'de> for CodexTextVisitor {
    type Value = CodexText<'de>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON string")
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Self::Value, E> {
        Ok(CodexText {
            value: Cow::Borrowed(value),
            escaped: false,
        })
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(CodexText {
            value: Cow::Owned(value.to_owned()),
            escaped: true,
        })
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(CodexText {
            value: Cow::Owned(value),
            escaped: true,
        })
    }
}

#[derive(Debug)]
struct CodexLineageText<'a> {
    value: Option<CodexText<'a>>,
    malformed: bool,
}

impl<'de> Deserialize<'de> for CodexLineageText<'de> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(CodexLineageTextVisitor)
    }
}

struct CodexLineageTextVisitor;

impl<'de> Visitor<'de> for CodexLineageTextVisitor {
    type Value = CodexLineageText<'de>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a lineage string")
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Self::Value, E> {
        Ok(CodexLineageText {
            value: Some(CodexText {
                value: Cow::Borrowed(value),
                escaped: false,
            }),
            malformed: false,
        })
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(CodexLineageText {
            value: Some(CodexText {
                value: Cow::Owned(value.to_owned()),
                escaped: true,
            }),
            malformed: false,
        })
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(CodexLineageText {
            value: Some(CodexText {
                value: Cow::Owned(value),
                escaped: true,
            }),
            malformed: false,
        })
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(malformed_lineage_text())
    }

    fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(malformed_lineage_text())
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(malformed_lineage_text())
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(malformed_lineage_text())
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(malformed_lineage_text())
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(malformed_lineage_text())
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(malformed_lineage_text())
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(malformed_lineage_text())
    }
}

fn malformed_lineage_text<'a>() -> CodexLineageText<'a> {
    CodexLineageText {
        value: None,
        malformed: true,
    }
}

#[derive(Debug)]
struct CodexEnvelopeProbe<'a> {
    record_type: Option<CodexText<'a>>,
    timestamp: Option<Cow<'a, str>>,
    payload: Option<CodexPayloadProbe<'a>>,
    relationship_escaped: bool,
    lineage_malformed: bool,
    item_completed_discriminator: bool,
}

impl<'de> Deserialize<'de> for CodexEnvelopeProbe<'de> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(CodexEnvelopeProbeVisitor)
    }
}

struct CodexEnvelopeProbeVisitor;

impl<'de> Visitor<'de> for CodexEnvelopeProbeVisitor {
    type Value = CodexEnvelopeProbe<'de>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Codex JSON object")
    }

    fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut record_type = None;
        let mut timestamp = None;
        let mut payload = None;
        let mut saw_record_type = false;
        let mut saw_timestamp = false;
        let mut saw_payload = false;
        let mut relationship_escaped = false;
        let mut lineage_malformed = false;
        let mut item_completed_discriminator = false;
        while let Some(key) = map.next_key::<CodexText<'de>>()? {
            let key_escaped = key.escaped;
            match key.as_str() {
                "type" => {
                    let duplicate = saw_record_type;
                    saw_record_type = true;
                    let value = map.next_value::<CodexLineageText<'de>>()?;
                    relationship_escaped |=
                        key_escaped || value.value.as_ref().is_some_and(|value| value.escaped);
                    lineage_malformed |= duplicate || value.malformed;
                    if !duplicate {
                        record_type = value.value;
                    }
                }
                "payload" => {
                    let duplicate = saw_payload;
                    saw_payload = true;
                    relationship_escaped |= key_escaped;
                    let value = map.next_value::<Option<CodexPayloadProbe<'de>>>()?;
                    if let Some(value) = value.as_ref() {
                        relationship_escaped |= value.relationship_escaped;
                        lineage_malformed |= value.lineage_malformed;
                        item_completed_discriminator |= value.item_completed_discriminator;
                    }
                    lineage_malformed |= duplicate;
                    if !duplicate {
                        payload = value;
                    }
                }
                "timestamp" => {
                    if saw_timestamp {
                        return Err(serde::de::Error::duplicate_field("timestamp"));
                    }
                    saw_timestamp = true;
                    timestamp = map.next_value::<Option<Cow<'de, str>>>()?;
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        if !saw_record_type {
            return Err(serde::de::Error::missing_field("type"));
        }
        Ok(CodexEnvelopeProbe {
            record_type,
            timestamp,
            payload,
            relationship_escaped,
            lineage_malformed,
            item_completed_discriminator,
        })
    }
}

#[derive(Debug)]
struct CodexPayloadProbe<'a> {
    item_type: Option<CodexText<'a>>,
    call_id: Option<CodexText<'a>>,
    agent_thread_id: Option<CodexText<'a>>,
    activity_kind: Option<CodexText<'a>>,
    relationship_escaped: bool,
    lineage_malformed: bool,
    activity_relationship_escaped: bool,
    activity_lineage_malformed: bool,
    item_completed_discriminator: bool,
}

fn empty_codex_payload_probe<'a>() -> CodexPayloadProbe<'a> {
    CodexPayloadProbe {
        item_type: None,
        call_id: None,
        agent_thread_id: None,
        activity_kind: None,
        relationship_escaped: false,
        lineage_malformed: false,
        activity_relationship_escaped: false,
        activity_lineage_malformed: false,
        item_completed_discriminator: false,
    }
}

impl<'de> Deserialize<'de> for CodexPayloadProbe<'de> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(CodexPayloadProbeVisitor)
    }
}

struct CodexPayloadProbeVisitor;

impl<'de> Visitor<'de> for CodexPayloadProbeVisitor {
    type Value = CodexPayloadProbe<'de>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("any valid Codex payload")
    }

    fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut item_type = None;
        let mut call_id = None;
        let mut agent_thread_id = None;
        let mut activity_kind = None;
        let mut saw_item_type = false;
        let mut saw_call_id = false;
        let mut saw_agent_thread_id = false;
        let mut saw_activity_kind = false;
        let mut relationship_escaped = false;
        let mut lineage_malformed = false;
        let mut activity_relationship_escaped = false;
        let mut activity_lineage_malformed = false;
        let mut item_completed_discriminator = false;
        while let Some(key) = map.next_key::<CodexText<'de>>()? {
            let key_escaped = key.escaped;
            match key.as_str() {
                "type" => {
                    let duplicate = saw_item_type;
                    saw_item_type = true;
                    let value = map.next_value::<CodexLineageText<'de>>()?;
                    relationship_escaped |=
                        key_escaped || value.value.as_ref().is_some_and(|value| value.escaped);
                    lineage_malformed |= duplicate || value.malformed;
                    item_completed_discriminator |= value
                        .value
                        .as_ref()
                        .is_some_and(|value| value.as_str() == "item_completed");
                    if !duplicate {
                        item_type = value.value;
                    }
                }
                "call_id" => {
                    let duplicate = saw_call_id;
                    saw_call_id = true;
                    let value = map.next_value::<CodexLineageText<'de>>()?;
                    relationship_escaped |=
                        key_escaped || value.value.as_ref().is_some_and(|value| value.escaped);
                    lineage_malformed |= duplicate || value.malformed;
                    if let Some(value) = value.value {
                        if !duplicate {
                            call_id = Some(value);
                        }
                    }
                }
                "agent_thread_id" => {
                    let duplicate = saw_agent_thread_id;
                    saw_agent_thread_id = true;
                    let value = map.next_value::<CodexLineageText<'de>>()?;
                    activity_relationship_escaped |=
                        key_escaped || value.value.as_ref().is_some_and(|value| value.escaped);
                    activity_lineage_malformed |= duplicate || value.malformed;
                    if !duplicate {
                        agent_thread_id = value.value;
                    }
                }
                "kind" => {
                    let duplicate = saw_activity_kind;
                    saw_activity_kind = true;
                    let value = map.next_value::<CodexLineageText<'de>>()?;
                    activity_relationship_escaped |=
                        key_escaped || value.value.as_ref().is_some_and(|value| value.escaped);
                    activity_lineage_malformed |= duplicate || value.malformed;
                    if !duplicate {
                        activity_kind = value.value;
                    }
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(CodexPayloadProbe {
            item_type,
            call_id,
            agent_thread_id,
            activity_kind,
            relationship_escaped,
            lineage_malformed,
            activity_relationship_escaped,
            activity_lineage_malformed,
            item_completed_discriminator,
        })
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(empty_codex_payload_probe())
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(empty_codex_payload_probe())
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(empty_codex_payload_probe())
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(empty_codex_payload_probe())
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(empty_codex_payload_probe())
    }

    fn visit_borrowed_str<E>(self, _value: &'de str) -> Result<Self::Value, E> {
        Ok(empty_codex_payload_probe())
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E> {
        Ok(empty_codex_payload_probe())
    }

    fn visit_string<E>(self, _value: String) -> Result<Self::Value, E> {
        Ok(empty_codex_payload_probe())
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(empty_codex_payload_probe())
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(empty_codex_payload_probe())
    }
}

#[derive(Debug)]
pub(super) struct CodexRecordProbe<'a> {
    pub(super) class: CodexRecordClass,
    pub(super) timestamp: Option<Cow<'a, str>>,
    pub(super) call_id: Option<Cow<'a, str>>,
    lineage_malformed: bool,
    item_completed_discriminator: bool,
}

impl CodexRecordProbe<'_> {
    pub(super) const fn lineage_malformed(&self) -> bool {
        self.lineage_malformed
    }

    pub(super) const fn item_completed_discriminator(&self) -> bool {
        self.item_completed_discriminator
    }
}

pub(super) fn classify_codex_record(line: &[u8]) -> serde_json::Result<CodexRecordProbe<'_>> {
    let envelope = serde_json::from_slice::<CodexEnvelopeProbe<'_>>(line)?;
    let item_type = envelope
        .payload
        .as_ref()
        .and_then(|payload| payload.item_type.as_ref().map(CodexText::as_str));
    let base_class = codex_record_class(
        envelope.record_type.as_ref().map_or("", CodexText::as_str),
        item_type,
    );
    let activity_record = base_class == CodexRecordClass::DescendantActivity;
    let lineage_malformed = envelope.lineage_malformed
        || envelope.payload.as_ref().is_some_and(|payload| {
            payload.lineage_malformed || (activity_record && payload.activity_lineage_malformed)
        });
    let relationship_escaped = envelope.relationship_escaped
        || envelope.payload.as_ref().is_some_and(|payload| {
            payload.relationship_escaped
                || (activity_record && payload.activity_relationship_escaped)
        });
    let descendant_started = envelope.payload.as_ref().is_some_and(|payload| {
        base_class == CodexRecordClass::DescendantActivity
            && payload.activity_kind.as_ref().map(CodexText::as_str) == Some("started")
            && !lineage_malformed
            && !relationship_escaped
            && payload
                .agent_thread_id
                .as_ref()
                .is_some_and(|value| uuid::Uuid::parse_str(value.as_str()).is_ok())
    });
    Ok(CodexRecordProbe {
        class: if descendant_started {
            CodexRecordClass::DescendantStarted
        } else {
            base_class
        },
        timestamp: envelope.timestamp,
        call_id: envelope
            .payload
            .and_then(|payload| payload.call_id.map(|call_id| call_id.value)),
        lineage_malformed,
        item_completed_discriminator: envelope.item_completed_discriminator,
    })
}

/// Retains a duplicate-selector record by its parseable provider shape. Raw
/// auditing independently withholds every affected exact channel.
pub(super) fn classify_after_selector_ambiguity(line: &[u8]) -> Option<CodexRecordProbe<'_>> {
    let envelope = serde_json::from_slice::<Value>(line).ok()?;
    let record_type = envelope.get("type").and_then(Value::as_str)?;
    let timestamp = match envelope.get("timestamp") {
        Some(Value::String(timestamp)) => Some(Cow::Owned(timestamp.clone())),
        Some(Value::Null) | None => None,
        Some(_) => return None,
    };
    let payload = envelope.get("payload").and_then(Value::as_object);
    let item_type = payload
        .and_then(|payload| payload.get("type"))
        .and_then(Value::as_str);
    let call_id = match payload.and_then(|payload| payload.get("call_id")) {
        Some(Value::String(call_id)) => Some(Cow::Owned(call_id.clone())),
        Some(Value::Null) | None => None,
        Some(_) => return None,
    };
    Some(CodexRecordProbe {
        class: codex_record_class(record_type, item_type),
        timestamp,
        call_id,
        lineage_malformed: false,
        item_completed_discriminator: item_type == Some("item_completed"),
    })
}

/// The single authority that maps a Codex envelope/payload type pair onto the
/// class the reader projects.
///
/// Both the typed structural probe and the pre-parse byte prefilter decide with
/// this function, so the prefilter's skip set cannot drift away from what the
/// reader materializes.
pub(super) fn codex_record_class(record_type: &str, item_type: Option<&str>) -> CodexRecordClass {
    match record_type {
        "session_meta" => CodexRecordClass::SessionMeta,
        "turn_context" => CodexRecordClass::TurnContext,
        "compacted" => CodexRecordClass::Retained(CodexRetainedKind::Compacted),
        "response_item" => classify_response_item(item_type),
        "event_msg" => classify_event_message(item_type),
        _ => CodexRecordClass::Ignored,
    }
}

fn classify_response_item(item_type: Option<&str>) -> CodexRecordClass {
    match item_type {
        Some("message") => CodexRecordClass::Retained(CodexRetainedKind::Message),
        Some("reasoning") => CodexRecordClass::Retained(CodexRetainedKind::Reasoning),
        Some("function_call" | "custom_tool_call" | "web_search_call" | "tool_search_call") => {
            CodexRecordClass::Retained(CodexRetainedKind::ToolCall)
        }
        Some("function_call_output") => {
            CodexRecordClass::ExcludedResult(CodexResultKind::FunctionCallOutput)
        }
        Some("custom_tool_call_output") => {
            CodexRecordClass::ExcludedResult(CodexResultKind::CustomToolCallOutput)
        }
        Some("tool_search_output") => {
            CodexRecordClass::ExcludedResult(CodexResultKind::ToolSearchOutput)
        }
        Some("tool_output" | "tool_result") => {
            CodexRecordClass::ExcludedResult(CodexResultKind::OtherResult)
        }
        _ => CodexRecordClass::Ignored,
    }
}

fn classify_event_message(item_type: Option<&str>) -> CodexRecordClass {
    match item_type {
        Some("item_completed") => CodexRecordClass::Retained(CodexRetainedKind::ItemCompleted),
        Some("sub_agent_activity") => CodexRecordClass::DescendantActivity,
        Some(
            "patch_apply_end" | "web_search_end" | "exec_command_end" | "command_complete"
            | "tool_complete" | "mcp_tool_call_end",
        ) => CodexRecordClass::ExcludedResult(CodexResultKind::OtherResult),
        Some(
            "task_started" | "task_complete" | "turn_aborted" | "context_compacted" | "token_count",
        ) => CodexRecordClass::Ignored,
        _ => CodexRecordClass::Ignored,
    }
}

mod prefilter;
#[cfg(test)]
pub(super) use prefilter::codex_skip_projection;
pub(super) use prefilter::{prefilter_codex_record, CodexRecordAdmission, CodexSkipProjection};

#[derive(Debug, Deserialize)]
struct CodexSessionMetaEnvelope {
    timestamp: Option<String>,
    payload: CodexSessionMetaPayload,
}

#[derive(Debug, Deserialize)]
struct CodexSessionMetaPayload {
    id: String,
    session_id: Option<String>,
    timestamp: Option<String>,
    cwd: Option<String>,
    originator: Option<String>,
    cli_version: Option<String>,
    #[serde(default)]
    source: Value,
    parent_thread_id: Option<String>,
    forked_from_id: Option<String>,
    history_base: Option<CodexHistoryBase>,
    agent_nickname: Option<String>,
    agent_role: Option<String>,
    model_provider: Option<String>,
    git: Option<CodexSessionGitMetadata>,
}

#[derive(Debug, Deserialize)]
struct CodexHistoryBase {
    thread_id: String,
}

#[derive(Debug, Deserialize)]
struct CodexTurnContextEnvelope {
    payload: CodexTurnContextPayload,
}

#[derive(Debug, Deserialize)]
struct CodexTurnContextPayload {
    cwd: String,
    #[serde(default)]
    turn_id: Option<String>,
}

pub(super) fn parse_session_meta(line: &[u8]) -> Option<CodexSessionRow> {
    let envelope = serde_json::from_slice::<CodexSessionMetaEnvelope>(line).ok()?;
    let payload = envelope.payload;
    let native_session_id = bounded_nonempty(payload.id, MAX_CODEX_DURABLE_SESSION_ID_BYTES)?;
    let provider_root_native_session_id = match payload.session_id {
        Some(value) => Some(bounded_nonempty(value, MAX_CODEX_DURABLE_SESSION_ID_BYTES)?),
        None => None,
    };
    let started_at = payload
        .timestamp
        .as_deref()
        .or(envelope.timestamp.as_deref())
        .and_then(parse_rfc3339_utc)?;
    let (parent_native_session_id, session_relationship) = codex_session_relationship(
        &payload.source,
        payload.parent_thread_id.as_deref(),
        payload.forked_from_id.as_deref(),
        payload
            .history_base
            .as_ref()
            .map(|history_base| history_base.thread_id.as_str()),
    );
    if parent_native_session_id
        .as_ref()
        .is_some_and(|id| id.len() > MAX_CODEX_DURABLE_SESSION_ID_BYTES)
    {
        return None;
    }
    let root_native_session_id = match session_relationship {
        Some(ctx_history_core::ProviderNativeSessionRelationship::Root) | None => None,
        Some(_) => provider_root_native_session_id
            .filter(|root_native_session_id| root_native_session_id != &native_session_id),
    };
    Some(CodexSessionRow {
        native_session_id,
        parent_native_session_id,
        root_native_session_id,
        session_relationship,
        started_at,
        cwd: payload
            .cwd
            .and_then(|value| bounded_nonempty(value, MAX_CODEX_DURABLE_CWD_BYTES)),
        originator: payload
            .originator
            .and_then(|value| bounded_nonempty(value, MAX_CODEX_DURABLE_METADATA_BYTES)),
        cli_version: payload
            .cli_version
            .and_then(|value| bounded_nonempty(value, MAX_CODEX_DURABLE_METADATA_BYTES)),
        source_kind: codex_source_kind(&payload.source)
            .and_then(|value| bounded_nonempty(value, MAX_CODEX_DURABLE_METADATA_BYTES)),
        external_agent_id: payload
            .agent_nickname
            .and_then(|value| bounded_nonempty(value, MAX_CODEX_DURABLE_METADATA_BYTES)),
        role_hint: payload
            .agent_role
            .and_then(|value| bounded_nonempty(value, MAX_CODEX_DURABLE_METADATA_BYTES)),
        model_provider: payload
            .model_provider
            .and_then(|value| bounded_nonempty(value, MAX_CODEX_DURABLE_METADATA_BYTES)),
        git: payload.git.and_then(|git| {
            let git = CodexSessionGitMetadata {
                commit_hash: git
                    .commit_hash
                    .and_then(|value| bounded_nonempty(value, MAX_CODEX_DURABLE_METADATA_BYTES)),
                branch: git
                    .branch
                    .and_then(|value| bounded_nonempty(value, MAX_CODEX_DURABLE_METADATA_BYTES)),
                repository_url: git
                    .repository_url
                    .and_then(|value| bounded_nonempty(value, MAX_CODEX_DURABLE_METADATA_BYTES)),
            };
            (git.commit_hash.is_some() || git.branch.is_some() || git.repository_url.is_some())
                .then_some(git)
        }),
    })
}

pub(super) fn parse_turn_context(line: &[u8]) -> Option<(String, Option<String>)> {
    let envelope = serde_json::from_slice::<CodexTurnContextEnvelope>(line).ok()?;
    let cwd = bounded_nonempty(envelope.payload.cwd, MAX_CODEX_DURABLE_CWD_BYTES)?;
    let turn_id = match envelope.payload.turn_id {
        Some(turn_id) => Some(bounded_nonempty(
            turn_id,
            MAX_CODEX_DURABLE_SESSION_ID_BYTES,
        )?),
        None => None,
    };
    Some((cwd, turn_id))
}

fn nonempty(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

fn bounded_nonempty(value: String, max_bytes: usize) -> Option<String> {
    (value.len() <= max_bytes)
        .then_some(value)
        .and_then(nonempty)
}

#[derive(Debug, Deserialize)]
struct CodexDecodedEnvelope {
    timestamp: Option<String>,
    payload: Value,
}

#[derive(Debug)]
pub(super) struct CodexDecodedRecord {
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) payload: Value,
}

pub(super) fn parse_decoded_record(
    line: &[u8],
    owner: &CodexSessionRow,
) -> Option<CodexDecodedRecord> {
    let envelope = match serde_json::from_slice::<CodexDecodedEnvelope>(line) {
        Ok(envelope) => envelope,
        Err(_) => parse_after_selector_ambiguity(line)?,
    };
    let occurred_at = match envelope.timestamp {
        Some(timestamp) => parse_rfc3339_utc(&timestamp)?,
        None => owner.started_at,
    };
    Some(CodexDecodedRecord {
        occurred_at,
        payload: envelope.payload,
    })
}

fn parse_after_selector_ambiguity(line: &[u8]) -> Option<CodexDecodedEnvelope> {
    let mut envelope = serde_json::from_slice::<Value>(line).ok()?;
    let object = envelope.as_object_mut()?;
    object.get("type").and_then(Value::as_str)?;
    let timestamp = match object.remove("timestamp") {
        Some(Value::String(timestamp)) => Some(timestamp),
        Some(Value::Null) | None => None,
        Some(_) => return None,
    };
    let payload = object.remove("payload")?;
    Some(CodexDecodedEnvelope { timestamp, payload })
}
