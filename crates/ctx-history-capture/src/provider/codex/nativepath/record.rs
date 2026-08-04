use std::{borrow::Cow, cmp::Ordering, fmt};

use chrono::{DateTime, Utc};
use serde::{
    de::{IgnoredAny, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use serde_json::Value;

use super::rows::{CodexSessionGitMetadata, CodexSessionRow};
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
    TurnContext,
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
    record_type: CodexText<'a>,
    timestamp: Option<Cow<'a, str>>,
    payload: Option<CodexPayloadProbe<'a>>,
    relationship_escaped: bool,
    lineage_malformed: bool,
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
        while let Some(key) = map.next_key::<CodexText<'de>>()? {
            let key_escaped = key.escaped;
            match key.as_str() {
                "type" => {
                    if saw_record_type {
                        map.next_value::<IgnoredAny>()?;
                        lineage_malformed = true;
                        continue;
                    }
                    saw_record_type = true;
                    let value = map.next_value::<CodexText<'de>>()?;
                    relationship_escaped |= key_escaped || value.escaped;
                    record_type = Some(value);
                }
                "payload" => {
                    if saw_payload {
                        map.next_value::<IgnoredAny>()?;
                        lineage_malformed = true;
                        continue;
                    }
                    saw_payload = true;
                    relationship_escaped |= key_escaped;
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
            relationship_escaped,
            lineage_malformed,
        })
    }
}

#[derive(Debug)]
struct CodexPayloadProbe<'a> {
    item_type: Option<CodexText<'a>>,
    call_id: Option<CodexText<'a>>,
    relationship_escaped: bool,
    lineage_malformed: bool,
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
        let mut relationship_escaped = false;
        let mut lineage_malformed = false;
        while let Some(key) = map.next_key::<CodexText<'de>>()? {
            let key_escaped = key.escaped;
            match key.as_str() {
                "type" => {
                    if saw_item_type {
                        map.next_value::<IgnoredAny>()?;
                        lineage_malformed = true;
                        continue;
                    }
                    saw_item_type = true;
                    let value = map.next_value::<Option<CodexText<'de>>>()?;
                    relationship_escaped |=
                        key_escaped || value.as_ref().is_some_and(|value| value.escaped);
                    item_type = value;
                }
                "call_id" => {
                    if saw_call_id {
                        map.next_value::<IgnoredAny>()?;
                        lineage_malformed = true;
                        continue;
                    }
                    saw_call_id = true;
                    let value = map.next_value::<CodexLineageText<'de>>()?;
                    relationship_escaped |=
                        key_escaped || value.value.as_ref().is_some_and(|value| value.escaped);
                    lineage_malformed |= value.malformed;
                    call_id = value.value;
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(CodexPayloadProbe {
            item_type,
            call_id,
            relationship_escaped,
            lineage_malformed,
        })
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(CodexPayloadProbe {
            item_type: None,
            call_id: None,
            relationship_escaped: false,
            lineage_malformed: false,
        })
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(CodexPayloadProbe {
            item_type: None,
            call_id: None,
            relationship_escaped: false,
            lineage_malformed: false,
        })
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(CodexPayloadProbe {
            item_type: None,
            call_id: None,
            relationship_escaped: false,
            lineage_malformed: false,
        })
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(CodexPayloadProbe {
            item_type: None,
            call_id: None,
            relationship_escaped: false,
            lineage_malformed: false,
        })
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(CodexPayloadProbe {
            item_type: None,
            call_id: None,
            relationship_escaped: false,
            lineage_malformed: false,
        })
    }

    fn visit_borrowed_str<E>(self, _value: &'de str) -> Result<Self::Value, E> {
        Ok(CodexPayloadProbe {
            item_type: None,
            call_id: None,
            relationship_escaped: false,
            lineage_malformed: false,
        })
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E> {
        Ok(CodexPayloadProbe {
            item_type: None,
            call_id: None,
            relationship_escaped: false,
            lineage_malformed: false,
        })
    }

    fn visit_string<E>(self, _value: String) -> Result<Self::Value, E> {
        Ok(CodexPayloadProbe {
            item_type: None,
            call_id: None,
            relationship_escaped: false,
            lineage_malformed: false,
        })
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(CodexPayloadProbe {
            item_type: None,
            call_id: None,
            relationship_escaped: false,
            lineage_malformed: false,
        })
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(CodexPayloadProbe {
            item_type: None,
            call_id: None,
            relationship_escaped: false,
            lineage_malformed: false,
        })
    }
}

#[derive(Debug)]
pub(super) struct CodexRecordProbe<'a> {
    pub(super) class: CodexRecordClass,
    pub(super) timestamp: Option<Cow<'a, str>>,
    pub(super) call_id: Option<Cow<'a, str>>,
    pub(super) output: Option<CodexStructuralOutput>,
    relationship_escaped: bool,
    lineage_malformed: bool,
}

impl CodexRecordProbe<'_> {
    pub(super) const fn lineage_malformed(&self) -> bool {
        self.lineage_malformed
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum CodexLineageRecordEvidence<'a> {
    None,
    Call(&'a str),
    Result(&'a str),
    Ambiguous(&'a str),
    UnattributedAmbiguity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CodexStructuralOutput {
    pub(super) outcome: OutputOutcomeMetadata,
    pub(super) output_bytes: Option<usize>,
    pub(super) has_exact_display_field: bool,
}

pub(super) fn classify_codex_record(line: &[u8]) -> serde_json::Result<CodexRecordProbe<'_>> {
    let envelope = serde_json::from_slice::<CodexEnvelopeProbe<'_>>(line)?;
    let lineage_malformed = envelope.lineage_malformed
        || envelope
            .payload
            .as_ref()
            .is_some_and(|payload| payload.lineage_malformed);
    let item_type = envelope
        .payload
        .as_ref()
        .and_then(|payload| payload.item_type.as_ref().map(CodexText::as_str));
    let class = codex_record_class(envelope.record_type.as_str(), item_type);
    let output = match (lineage_malformed, class) {
        (true, _) => None,
        (false, CodexRecordClass::ExcludedResult(_)) => Some(probe_structural_output(line)?),
        (false, _) => None,
    };
    let relationship_escaped = envelope.relationship_escaped
        || envelope
            .payload
            .as_ref()
            .is_some_and(|payload| payload.relationship_escaped);
    Ok(CodexRecordProbe {
        class,
        timestamp: envelope.timestamp,
        call_id: envelope
            .payload
            .and_then(|payload| payload.call_id.map(|call_id| call_id.value)),
        output,
        relationship_escaped,
        lineage_malformed,
    })
}

pub(super) fn codex_lineage_record_evidence<'a>(
    probe: &'a CodexRecordProbe<'_>,
) -> CodexLineageRecordEvidence<'a> {
    if probe.lineage_malformed {
        return CodexLineageRecordEvidence::UnattributedAmbiguity;
    }
    let is_call = matches!(
        probe.class,
        CodexRecordClass::Retained(CodexRetainedKind::ToolCall)
    );
    let is_result = matches!(probe.class, CodexRecordClass::ExcludedResult(_));
    if !is_call && !is_result {
        return CodexLineageRecordEvidence::None;
    }
    let Some(call_id) = probe.call_id.as_deref() else {
        return CodexLineageRecordEvidence::UnattributedAmbiguity;
    };
    if call_id.is_empty() || call_id.len() > super::checkpoint::MAX_CODEX_TOOL_CALL_ID_BYTES {
        return CodexLineageRecordEvidence::UnattributedAmbiguity;
    }
    if probe.relationship_escaped {
        return CodexLineageRecordEvidence::Ambiguous(call_id);
    }
    if is_call {
        CodexLineageRecordEvidence::Call(call_id)
    } else {
        CodexLineageRecordEvidence::Result(call_id)
    }
}

pub(super) fn malformed_record_may_contain_lineage(record: &[u8]) -> bool {
    [
        br#""call_id""#.as_slice(),
        br#""function_call""#.as_slice(),
        br#""custom_tool_call""#.as_slice(),
        br#""function_call_output""#.as_slice(),
        br#""custom_tool_call_output""#.as_slice(),
    ]
    .into_iter()
    .any(|needle| record.windows(needle.len()).any(|window| window == needle))
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
    git: Option<CodexSessionGitMetadata>,
}

#[derive(Debug, Deserialize)]
struct CodexTurnContextEnvelope {
    payload: CodexTurnContextPayload,
}

#[derive(Debug, Deserialize)]
struct CodexTurnContextPayload {
    cwd: String,
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
        git: payload.git.and_then(|git| {
            let git = CodexSessionGitMetadata {
                commit_hash: git.commit_hash.and_then(nonempty),
                branch: git.branch.and_then(nonempty),
                repository_url: git.repository_url.and_then(nonempty),
            };
            (git.commit_hash.is_some() || git.branch.is_some() || git.repository_url.is_some())
                .then_some(git)
        }),
    })
}

pub(super) fn parse_turn_context_cwd(line: &[u8]) -> Option<String> {
    let envelope = serde_json::from_slice::<CodexTurnContextEnvelope>(line).ok()?;
    nonempty(envelope.payload.cwd)
}

fn nonempty(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

#[cfg(test)]
mod lineage_tests {
    use super::*;

    #[test]
    fn escaped_relationship_fields_are_ambiguous_not_exact() {
        let record = br#"{"type":"response_item","payload":{"type":"function_call","call_\u0069d":"escaped-call"}}"#;
        let probe = classify_codex_record(record).unwrap();
        assert_eq!(
            codex_lineage_record_evidence(&probe),
            CodexLineageRecordEvidence::Ambiguous("escaped-call")
        );
    }

    #[test]
    fn duplicate_relationship_fields_are_malformed_and_unattributed() {
        let record = br#"{"type":"response_item","payload":{"type":"function_call","call_id":"first","call_id":"second"}}"#;
        let probe = classify_codex_record(record).unwrap();
        assert!(probe.lineage_malformed());
        assert_eq!(
            codex_lineage_record_evidence(&probe),
            CodexLineageRecordEvidence::UnattributedAmbiguity
        );
    }

    #[test]
    fn fully_escaped_duplicate_lineage_fields_do_not_evade_ambiguity() {
        let record = br#"{"\u0074\u0079\u0070\u0065":"\u0072\u0065\u0073\u0070\u006f\u006e\u0073\u0065\u005f\u0069\u0074\u0065\u006d","\u0070\u0061\u0079\u006c\u006f\u0061\u0064":{"\u0074\u0079\u0070\u0065":"\u0066\u0075\u006e\u0063\u0074\u0069\u006f\u006e\u005f\u0063\u0061\u006c\u006c","\u0063\u0061\u006c\u006c\u005f\u0069\u0064":"first","\u0063\u0061\u006c\u006c\u005f\u0069\u0064":"second"}}"#;
        assert!(!malformed_record_may_contain_lineage(record));
        let probe = classify_codex_record(record).unwrap();
        assert!(probe.lineage_malformed());
        assert_eq!(
            codex_lineage_record_evidence(&probe),
            CodexLineageRecordEvidence::UnattributedAmbiguity
        );
    }

    #[test]
    fn escaped_non_string_call_ids_are_ambiguous_in_either_duplicate_order() {
        let records = [
            br#"{"\u0074\u0079\u0070\u0065":"\u0072\u0065\u0073\u0070\u006f\u006e\u0073\u0065\u005f\u0069\u0074\u0065\u006d","\u0070\u0061\u0079\u006c\u006f\u0061\u0064":{"\u0074\u0079\u0070\u0065":"\u0066\u0075\u006e\u0063\u0074\u0069\u006f\u006e\u005f\u0063\u0061\u006c\u006c","\u0063\u0061\u006c\u006c\u005f\u0069\u0064":7,"\u0063\u0061\u006c\u006c\u005f\u0069\u0064":"target"}}"#
                .as_slice(),
            br#"{"\u0074\u0079\u0070\u0065":"\u0072\u0065\u0073\u0070\u006f\u006e\u0073\u0065\u005f\u0069\u0074\u0065\u006d","\u0070\u0061\u0079\u006c\u006f\u0061\u0064":{"\u0074\u0079\u0070\u0065":"\u0066\u0075\u006e\u0063\u0074\u0069\u006f\u006e\u005f\u0063\u0061\u006c\u006c","\u0063\u0061\u006c\u006c\u005f\u0069\u0064":"target","\u0063\u0061\u006c\u006c\u005f\u0069\u0064":7}}"#
                .as_slice(),
        ];
        for record in records {
            assert!(!malformed_record_may_contain_lineage(record));
            let probe = classify_codex_record(record).unwrap();
            assert!(probe.lineage_malformed());
            assert_eq!(
                codex_lineage_record_evidence(&probe),
                CodexLineageRecordEvidence::UnattributedAmbiguity
            );
        }

        let unrelated = br#"{"timestamp":"a","timestamp":"b","type":"event_msg","payload":{"type":"token_count"}}"#;
        assert!(classify_codex_record(unrelated).is_err());
        assert!(!malformed_record_may_contain_lineage(unrelated));
    }
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
