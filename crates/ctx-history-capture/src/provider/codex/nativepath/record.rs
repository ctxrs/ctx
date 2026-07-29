use std::{borrow::Cow, cmp::Ordering, fmt};

use chrono::{DateTime, Utc};
use serde::{
    de::{IgnoredAny, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use serde_json::Value;

use super::rows::CodexSessionRow;
use crate::common::time::parse_rfc3339_utc;
use crate::provider::codex::catalog::{codex_parent_session_id, codex_source_kind};
use crate::provider::codex::events::{CodexExitCodeParser, CodexWallTimeParser};
use crate::{OutputOutcome, OutputOutcomeMetadata};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CodexRetainedKind {
    Message,
    Reasoning,
    Compacted,
    ToolCall,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CodexResultKind {
    FunctionCallOutput,
    CustomToolCallOutput,
    ToolSearchOutput,
    OtherResult,
}

impl CodexResultKind {
    pub(super) const fn is_eligible_output(self) -> bool {
        !matches!(self, Self::OtherResult)
    }

    pub(super) const fn item_type(self) -> &'static str {
        match self {
            Self::FunctionCallOutput => "function_call_output",
            Self::CustomToolCallOutput => "custom_tool_call_output",
            Self::ToolSearchOutput => "tool_search_output",
            Self::OtherResult => "tool_result",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CodexRecordClass {
    SessionMeta,
    Retained(CodexRetainedKind),
    ExcludedResult(CodexResultKind),
    Ignored,
}

#[derive(Debug)]
struct CodexEnvelopeProbe<'a> {
    record_type: Cow<'a, str>,
    timestamp: Option<Cow<'a, str>>,
    payload: Option<CodexPayloadProbe<'a>>,
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
        while let Some(key) = map.next_key::<Cow<'de, str>>()? {
            match key.as_ref() {
                "type" => {
                    if saw_record_type {
                        return Err(serde::de::Error::duplicate_field("type"));
                    }
                    saw_record_type = true;
                    record_type = Some(map.next_value::<Cow<'de, str>>()?);
                }
                "payload" => {
                    if saw_payload {
                        return Err(serde::de::Error::duplicate_field("payload"));
                    }
                    saw_payload = true;
                    payload = map.next_value::<Option<CodexPayloadProbe<'de>>>()?;
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
        Ok(CodexEnvelopeProbe {
            record_type: record_type.ok_or_else(|| serde::de::Error::missing_field("type"))?,
            timestamp,
            payload,
        })
    }
}

#[derive(Debug)]
struct CodexPayloadProbe<'a> {
    item_type: Option<Cow<'a, str>>,
    call_id: Option<Cow<'a, str>>,
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
        let mut saw_item_type = false;
        let mut saw_call_id = false;
        while let Some(key) = map.next_key::<Cow<'de, str>>()? {
            match key.as_ref() {
                "type" => {
                    if saw_item_type {
                        return Err(serde::de::Error::duplicate_field("type"));
                    }
                    saw_item_type = true;
                    item_type = map.next_value::<Option<Cow<'de, str>>>()?;
                }
                "call_id" => {
                    if saw_call_id {
                        return Err(serde::de::Error::duplicate_field("call_id"));
                    }
                    saw_call_id = true;
                    call_id = map.next_value::<Option<Cow<'de, str>>>()?;
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(CodexPayloadProbe { item_type, call_id })
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(CodexPayloadProbe {
            item_type: None,
            call_id: None,
        })
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(CodexPayloadProbe {
            item_type: None,
            call_id: None,
        })
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(CodexPayloadProbe {
            item_type: None,
            call_id: None,
        })
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(CodexPayloadProbe {
            item_type: None,
            call_id: None,
        })
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(CodexPayloadProbe {
            item_type: None,
            call_id: None,
        })
    }

    fn visit_borrowed_str<E>(self, _value: &'de str) -> Result<Self::Value, E> {
        Ok(CodexPayloadProbe {
            item_type: None,
            call_id: None,
        })
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E> {
        Ok(CodexPayloadProbe {
            item_type: None,
            call_id: None,
        })
    }

    fn visit_string<E>(self, _value: String) -> Result<Self::Value, E> {
        Ok(CodexPayloadProbe {
            item_type: None,
            call_id: None,
        })
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(CodexPayloadProbe {
            item_type: None,
            call_id: None,
        })
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(CodexPayloadProbe {
            item_type: None,
            call_id: None,
        })
    }
}

#[derive(Debug)]
pub(super) struct CodexRecordProbe<'a> {
    pub(super) class: CodexRecordClass,
    pub(super) timestamp: Option<Cow<'a, str>>,
    pub(super) call_id: Option<Cow<'a, str>>,
    pub(super) output: Option<CodexStructuralOutput>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CodexStructuralOutput {
    pub(super) outcome: OutputOutcomeMetadata,
    pub(super) output_bytes: Option<usize>,
    pub(super) has_exact_display_field: bool,
}

pub(super) fn classify_codex_record(line: &[u8]) -> serde_json::Result<CodexRecordProbe<'_>> {
    let envelope = serde_json::from_slice::<CodexEnvelopeProbe<'_>>(line)?;
    let item_type = envelope
        .payload
        .as_ref()
        .and_then(|payload| payload.item_type.as_deref());
    let class = codex_record_class(envelope.record_type.as_ref(), item_type);
    let output = match class {
        CodexRecordClass::ExcludedResult(kind) if kind.is_eligible_output() => {
            Some(probe_structural_output(line)?)
        }
        _ => None,
    };
    Ok(CodexRecordProbe {
        class,
        timestamp: envelope.timestamp,
        call_id: envelope.payload.and_then(|payload| payload.call_id),
        output,
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
        Some(item_type) if result_like_item_type(item_type) => {
            CodexRecordClass::ExcludedResult(CodexResultKind::OtherResult)
        }
        _ => CodexRecordClass::Ignored,
    }
}

fn classify_event_message(item_type: Option<&str>) -> CodexRecordClass {
    match item_type {
        Some(
            "patch_apply_end" | "web_search_end" | "exec_command_end" | "command_complete"
            | "tool_complete",
        ) => CodexRecordClass::ExcludedResult(CodexResultKind::OtherResult),
        Some(
            "task_started" | "task_complete" | "turn_aborted" | "context_compacted" | "token_count",
        ) => CodexRecordClass::Ignored,
        Some(item_type) if result_like_item_type(item_type) => {
            CodexRecordClass::ExcludedResult(CodexResultKind::OtherResult)
        }
        _ => CodexRecordClass::Ignored,
    }
}

fn result_like_item_type(item_type: &str) -> bool {
    item_type.ends_with("_output")
        || item_type.ends_with("_result")
        || item_type.ends_with("_response")
        || item_type.ends_with("_end")
        || matches!(
            item_type,
            "tool_output" | "tool_result" | "command_output" | "command_result"
        )
}

mod prefilter;
mod structural;

#[cfg(test)]
pub(super) use prefilter::codex_skip_projection;
pub(super) use prefilter::{prefilter_codex_record, CodexRecordAdmission, CodexSkipProjection};
use structural::probe_structural_output;

#[derive(Debug, Deserialize)]
struct CodexSessionMetaEnvelope {
    timestamp: Option<String>,
    payload: CodexSessionMetaPayload,
}

#[derive(Debug, Deserialize)]
struct CodexSessionMetaPayload {
    id: String,
    timestamp: Option<String>,
    cwd: Option<String>,
    originator: Option<String>,
    cli_version: Option<String>,
    #[serde(default)]
    source: Value,
    session_id: Option<String>,
    parent_thread_id: Option<String>,
    forked_from_id: Option<String>,
    agent_nickname: Option<String>,
    agent_role: Option<String>,
    model_provider: Option<String>,
}

pub(super) fn parse_session_meta(line: &[u8]) -> Option<CodexSessionRow> {
    let envelope = serde_json::from_slice::<CodexSessionMetaEnvelope>(line).ok()?;
    let payload = envelope.payload;
    let native_session_id = nonempty(payload.id)?;
    let started_at = payload
        .timestamp
        .as_deref()
        .or(envelope.timestamp.as_deref())
        .and_then(parse_rfc3339_utc)?;
    let parent_native_session_id = codex_parent_session_id(&payload.source)
        .or_else(|| payload.parent_thread_id.and_then(nonempty))
        .or_else(|| payload.forked_from_id.and_then(nonempty));
    let root_native_session_id = payload
        .session_id
        .and_then(nonempty)
        .filter(|root| root != &native_session_id)
        .or_else(|| parent_native_session_id.clone());
    Some(CodexSessionRow {
        native_session_id,
        parent_native_session_id,
        root_native_session_id,
        started_at,
        cwd: payload.cwd.and_then(nonempty),
        originator: payload.originator.and_then(nonempty),
        cli_version: payload.cli_version.and_then(nonempty),
        source_kind: codex_source_kind(&payload.source),
        external_agent_id: payload.agent_nickname.and_then(nonempty),
        role_hint: payload.agent_role.and_then(nonempty),
        model_provider: payload.model_provider.and_then(nonempty),
    })
}

fn nonempty(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
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
    let envelope = serde_json::from_slice::<CodexDecodedEnvelope>(line).ok()?;
    let occurred_at = match envelope.timestamp {
        Some(timestamp) => parse_rfc3339_utc(&timestamp)?,
        None => owner.started_at,
    };
    Some(CodexDecodedRecord {
        occurred_at,
        payload: envelope.payload,
    })
}
