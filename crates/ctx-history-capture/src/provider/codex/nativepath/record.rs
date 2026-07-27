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
}

pub(super) fn classify_codex_record(line: &[u8]) -> serde_json::Result<CodexRecordProbe<'_>> {
    let envelope = serde_json::from_slice::<CodexEnvelopeProbe<'_>>(line)?;
    let item_type = envelope
        .payload
        .as_ref()
        .and_then(|payload| payload.item_type.as_deref());
    let class = match envelope.record_type.as_ref() {
        "session_meta" => CodexRecordClass::SessionMeta,
        "compacted" => CodexRecordClass::Retained(CodexRetainedKind::Compacted),
        "response_item" => classify_response_item(item_type),
        "event_msg" => classify_event_message(item_type),
        _ => CodexRecordClass::Ignored,
    };
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

const MAX_JSON_VISITOR_DEPTH: usize = 128;
const MAX_JSON_VISITOR_TOKENS: usize = 256 * 1024;
const MAX_STRUCTURAL_KEY_BYTES: usize = 256;
const MAX_STRUCTURAL_RECURSIVE_KEYS: usize = 64;
const MAX_STRUCTURAL_TEXT_PREFIX: usize = 64;
const MAX_STRUCTURAL_NUMBER_BYTES: usize = 128;
const STRUCTURAL_ROLLING_BYTES: usize = 32;

#[derive(Debug, Clone, Copy, Default)]
struct StructuralOutputSignals {
    timed_out: bool,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
    explicit_failure: bool,
    explicit_success: bool,
}

impl StructuralOutputSignals {
    fn contributes(self) -> bool {
        self.timed_out
            || self.exit_code.is_some()
            || self.duration_ms.is_some()
            || self.explicit_failure
            || self.explicit_success
    }

    fn merge_recursive(&mut self, other: Self) {
        self.timed_out |= other.timed_out;
        self.exit_code = self.exit_code.or(other.exit_code);
        self.duration_ms = self.duration_ms.or(other.duration_ms);
        self.explicit_failure |= other.explicit_failure;
        self.explicit_success |= other.explicit_success;
    }
}

#[derive(Debug, Clone, Copy)]
struct StructuralRecursiveKey<'a> {
    raw_key: &'a [u8],
    signals: StructuralOutputSignals,
}

#[derive(Debug)]
struct StructuralRecursiveKeys<'a> {
    slots: [Option<StructuralRecursiveKey<'a>>; MAX_STRUCTURAL_RECURSIVE_KEYS],
    len: usize,
}

impl Default for StructuralRecursiveKeys<'_> {
    fn default() -> Self {
        Self {
            slots: [None; MAX_STRUCTURAL_RECURSIVE_KEYS],
            len: 0,
        }
    }
}

impl<'a> StructuralRecursiveKeys<'a> {
    fn observe(&mut self, raw_key: &'a [u8], signals: StructuralOutputSignals) -> Option<()> {
        let existing = self.slots[..self.len].iter().position(|slot| {
            slot.is_some_and(|slot| decoded_json_key_cmp(slot.raw_key, raw_key).is_eq())
        });
        if let Some(index) = existing {
            if signals.contributes() {
                self.slots[index] = Some(StructuralRecursiveKey { raw_key, signals });
            } else {
                self.len -= 1;
                self.slots[index] = self.slots[self.len].take();
            }
            return Some(());
        }
        if !signals.contributes() {
            return Some(());
        }
        let slot = self.slots.get_mut(self.len)?;
        *slot = Some(StructuralRecursiveKey { raw_key, signals });
        self.len += 1;
        Some(())
    }

    fn merge_into(self, recursive: &mut StructuralObjectSignals<'a>) {
        for slot in &self.slots[..self.len] {
            if let Some(slot) = *slot {
                recursive.observe(slot.raw_key, slot.signals);
            }
        }
    }
}

#[derive(Debug, Default)]
struct StructuralObjectSignals<'a> {
    timed_out: bool,
    exit_code: Option<(&'a [u8], i32)>,
    duration_ms: Option<(&'a [u8], u64)>,
    explicit_failure: bool,
    explicit_success: bool,
}

impl<'a> StructuralObjectSignals<'a> {
    fn observe(&mut self, key: &'a [u8], signals: StructuralOutputSignals) {
        self.timed_out |= signals.timed_out;
        self.explicit_failure |= signals.explicit_failure;
        self.explicit_success |= signals.explicit_success;
        if let Some(exit_code) = signals.exit_code {
            if self
                .exit_code
                .is_none_or(|(candidate, _)| decoded_json_key_is_before_or_same(key, candidate))
            {
                self.exit_code = Some((key, exit_code));
            }
        }
        if let Some(duration_ms) = signals.duration_ms {
            if self
                .duration_ms
                .is_none_or(|(candidate, _)| decoded_json_key_is_before_or_same(key, candidate))
            {
                self.duration_ms = Some((key, duration_ms));
            }
        }
    }

    fn finish(self) -> StructuralOutputSignals {
        StructuralOutputSignals {
            timed_out: self.timed_out,
            exit_code: self.exit_code.map(|(_, value)| value),
            duration_ms: self.duration_ms.map(|(_, value)| value),
            explicit_failure: self.explicit_failure,
            explicit_success: self.explicit_success,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
enum JsonNodeKind {
    #[default]
    Null,
    Bool,
    Number,
    String,
    Array,
    Object,
}

#[derive(Debug, Clone, Copy, Default)]
struct JsonScalarSummary {
    kind: JsonNodeKind,
    bool_value: Option<bool>,
    integer: Option<i64>,
    unsigned: Option<u64>,
    string_len: Option<usize>,
    string_text: FixedText<MAX_STRUCTURAL_TEXT_PREFIX>,
    string_nonempty: bool,
    status_failure: bool,
    status_success: bool,
    container_nonempty: bool,
}

impl JsonScalarSummary {
    fn error_indicates_failure(self) -> bool {
        match self.kind {
            JsonNodeKind::Null => false,
            JsonNodeKind::Bool => self.bool_value == Some(true),
            JsonNodeKind::String => self.string_nonempty,
            JsonNodeKind::Number => self.integer.is_some_and(|number| number != 0),
            JsonNodeKind::Array | JsonNodeKind::Object => self.container_nonempty,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct JsonNodeSummary {
    signals: StructuralOutputSignals,
    scalar: JsonScalarSummary,
    direct_output_bytes: Option<usize>,
}

#[derive(Debug, Clone, Copy, Default)]
struct ObjectField {
    present: bool,
    summary: JsonNodeSummary,
}

impl ObjectField {
    fn set(&mut self, summary: JsonNodeSummary) {
        self.present = true;
        self.summary = summary;
    }
}

#[derive(Debug, Default)]
struct StructuralObjectFields {
    timed_out: ObjectField,
    timed_out_camel: ObjectField,
    timeout: ObjectField,
    exit_code: ObjectField,
    exit_code_camel: ObjectField,
    duration_ms: ObjectField,
    duration_ms_camel: ObjectField,
    status_code: ObjectField,
    status_code_camel: ObjectField,
    success: ObjectField,
    ok: ObjectField,
    is_error_camel: ObjectField,
    is_error: ObjectField,
    status: ObjectField,
    state: ObjectField,
    outcome: ObjectField,
    error: ObjectField,
    output: ObjectField,
    tools: ObjectField,
    result: ObjectField,
    text: ObjectField,
    input_text: ObjectField,
    output_text: ObjectField,
    summary_text: ObjectField,
    content: ObjectField,
}

impl StructuralObjectFields {
    fn observe(&mut self, key: StructuralKey, summary: JsonNodeSummary) {
        let field = match key {
            StructuralKey::TimedOut => &mut self.timed_out,
            StructuralKey::TimedOutCamel => &mut self.timed_out_camel,
            StructuralKey::Timeout => &mut self.timeout,
            StructuralKey::ExitCode => &mut self.exit_code,
            StructuralKey::ExitCodeCamel => &mut self.exit_code_camel,
            StructuralKey::DurationMs => &mut self.duration_ms,
            StructuralKey::DurationMsCamel => &mut self.duration_ms_camel,
            StructuralKey::StatusCode => &mut self.status_code,
            StructuralKey::StatusCodeCamel => &mut self.status_code_camel,
            StructuralKey::Success => &mut self.success,
            StructuralKey::Ok => &mut self.ok,
            StructuralKey::IsErrorCamel => &mut self.is_error_camel,
            StructuralKey::IsError => &mut self.is_error,
            StructuralKey::Status => &mut self.status,
            StructuralKey::State => &mut self.state,
            StructuralKey::Outcome => &mut self.outcome,
            StructuralKey::Error => &mut self.error,
            StructuralKey::Output => &mut self.output,
            StructuralKey::Tools => &mut self.tools,
            StructuralKey::Result => &mut self.result,
            StructuralKey::Text => &mut self.text,
            StructuralKey::InputText => &mut self.input_text,
            StructuralKey::OutputText => &mut self.output_text,
            StructuralKey::SummaryText => &mut self.summary_text,
            StructuralKey::Content => &mut self.content,
            StructuralKey::Payload | StructuralKey::Other => return,
        };
        field.set(summary);
    }

    fn merge_recursive_signals<'a>(&self, recursive: &mut StructuralObjectSignals<'a>) {
        for (key, field) in [
            (StructuralKey::TimedOut, self.timed_out),
            (StructuralKey::TimedOutCamel, self.timed_out_camel),
            (StructuralKey::Timeout, self.timeout),
            (StructuralKey::ExitCode, self.exit_code),
            (StructuralKey::ExitCodeCamel, self.exit_code_camel),
            (StructuralKey::DurationMs, self.duration_ms),
            (StructuralKey::DurationMsCamel, self.duration_ms_camel),
            (StructuralKey::StatusCode, self.status_code),
            (StructuralKey::StatusCodeCamel, self.status_code_camel),
            (StructuralKey::Success, self.success),
            (StructuralKey::Ok, self.ok),
            (StructuralKey::IsErrorCamel, self.is_error_camel),
            (StructuralKey::IsError, self.is_error),
            (StructuralKey::Status, self.status),
            (StructuralKey::State, self.state),
            (StructuralKey::Outcome, self.outcome),
            (StructuralKey::Error, self.error),
            (StructuralKey::Output, self.output),
            (StructuralKey::Tools, self.tools),
            (StructuralKey::Result, self.result),
            (StructuralKey::Text, self.text),
            (StructuralKey::InputText, self.input_text),
            (StructuralKey::OutputText, self.output_text),
            (StructuralKey::SummaryText, self.summary_text),
            (StructuralKey::Content, self.content),
        ] {
            if field.present {
                recursive.observe(key.decoded(), field.summary.signals);
            }
        }
    }

    fn apply(self, mut recursive: StructuralOutputSignals) -> JsonNodeSummary {
        let direct_timeout =
            first_present_bool(&[self.timed_out, self.timed_out_camel, self.timeout]) == Some(true);
        recursive.timed_out |= direct_timeout;

        let direct_exit = [self.exit_code, self.exit_code_camel]
            .into_iter()
            .filter(|field| field.present)
            .find_map(|field| {
                field
                    .summary
                    .scalar
                    .integer
                    .and_then(|code| i32::try_from(code).ok())
            });
        recursive.exit_code = direct_exit.or(recursive.exit_code);

        let direct_duration = [self.duration_ms, self.duration_ms_camel]
            .into_iter()
            .filter(|field| field.present)
            .find_map(|field| field.summary.scalar.unsigned);
        recursive.duration_ms = direct_duration.or(recursive.duration_ms);

        recursive.explicit_failure |= direct_timeout
            || (self.success.present && self.success.summary.scalar.bool_value == Some(false))
            || first_present_bool(&[self.is_error_camel, self.is_error]) == Some(true)
            || [self.exit_code, self.exit_code_camel]
                .into_iter()
                .filter(|field| field.present)
                .any(|field| field.summary.scalar.integer.is_some_and(|code| code != 0))
            || [self.status_code, self.status_code_camel]
                .into_iter()
                .filter(|field| field.present)
                .any(|field| field.summary.scalar.integer.is_some_and(|code| code >= 400))
            || [self.status, self.state, self.outcome]
                .into_iter()
                .filter(|field| field.present)
                .any(|field| field.summary.scalar.status_failure)
            || (self.error.present && self.error.summary.scalar.error_indicates_failure());

        recursive.explicit_success |= first_present_bool(&[self.success, self.ok]) == Some(true)
            || [self.status, self.state, self.outcome]
                .into_iter()
                .filter(|field| field.present)
                .any(|field| field.summary.scalar.status_success);

        let selected_output = [self.output, self.tools, self.result]
            .into_iter()
            .find(|field| field.present);
        let direct_output_bytes = selected_output.and_then(|field| field.summary.scalar.string_len);
        JsonNodeSummary {
            signals: recursive,
            scalar: JsonScalarSummary {
                kind: JsonNodeKind::Object,
                container_nonempty: true,
                ..JsonScalarSummary::default()
            },
            direct_output_bytes,
        }
    }
}

fn first_present_bool(fields: &[ObjectField]) -> Option<bool> {
    fields
        .iter()
        .find(|field| field.present)
        .and_then(|field| field.summary.scalar.bool_value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StructuralKey {
    Payload,
    TimedOut,
    TimedOutCamel,
    Timeout,
    ExitCode,
    ExitCodeCamel,
    DurationMs,
    DurationMsCamel,
    StatusCode,
    StatusCodeCamel,
    Success,
    Ok,
    IsErrorCamel,
    IsError,
    Status,
    State,
    Outcome,
    Error,
    Output,
    Tools,
    Result,
    Text,
    InputText,
    OutputText,
    SummaryText,
    Content,
    Other,
}

impl StructuralKey {
    fn from_decoded(value: Option<&[u8]>) -> Self {
        match value {
            Some(b"payload") => Self::Payload,
            Some(b"timed_out") => Self::TimedOut,
            Some(b"timedOut") => Self::TimedOutCamel,
            Some(b"timeout") => Self::Timeout,
            Some(b"exit_code") => Self::ExitCode,
            Some(b"exitCode") => Self::ExitCodeCamel,
            Some(b"duration_ms") => Self::DurationMs,
            Some(b"durationMs") => Self::DurationMsCamel,
            Some(b"status_code") => Self::StatusCode,
            Some(b"statusCode") => Self::StatusCodeCamel,
            Some(b"success") => Self::Success,
            Some(b"ok") => Self::Ok,
            Some(b"isError") => Self::IsErrorCamel,
            Some(b"is_error") => Self::IsError,
            Some(b"status") => Self::Status,
            Some(b"state") => Self::State,
            Some(b"outcome") => Self::Outcome,
            Some(b"error") => Self::Error,
            Some(b"output") => Self::Output,
            Some(b"tools") => Self::Tools,
            Some(b"result") => Self::Result,
            Some(b"text") => Self::Text,
            Some(b"input_text") => Self::InputText,
            Some(b"output_text") => Self::OutputText,
            Some(b"summary_text") => Self::SummaryText,
            Some(b"content") => Self::Content,
            _ => Self::Other,
        }
    }

    const fn decoded(self) -> &'static [u8] {
        match self {
            Self::Payload => b"payload",
            Self::TimedOut => b"timed_out",
            Self::TimedOutCamel => b"timedOut",
            Self::Timeout => b"timeout",
            Self::ExitCode => b"exit_code",
            Self::ExitCodeCamel => b"exitCode",
            Self::DurationMs => b"duration_ms",
            Self::DurationMsCamel => b"durationMs",
            Self::StatusCode => b"status_code",
            Self::StatusCodeCamel => b"statusCode",
            Self::Success => b"success",
            Self::Ok => b"ok",
            Self::IsErrorCamel => b"isError",
            Self::IsError => b"is_error",
            Self::Status => b"status",
            Self::State => b"state",
            Self::Outcome => b"outcome",
            Self::Error => b"error",
            Self::Output => b"output",
            Self::Tools => b"tools",
            Self::Result => b"result",
            Self::Text => b"text",
            Self::InputText => b"input_text",
            Self::OutputText => b"output_text",
            Self::SummaryText => b"summary_text",
            Self::Content => b"content",
            Self::Other => b"",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ParsedStructuralKey<'a> {
    kind: StructuralKey,
    raw: &'a [u8],
}

#[derive(Debug, Clone, Copy)]
struct FixedText<const N: usize> {
    bytes: [u8; N],
    len: usize,
    overflowed: bool,
}

impl<const N: usize> Default for FixedText<N> {
    fn default() -> Self {
        Self {
            bytes: [0; N],
            len: 0,
            overflowed: false,
        }
    }
}

impl<const N: usize> FixedText<N> {
    fn push(&mut self, byte: u8) {
        if self.len < N {
            self.bytes[self.len] = byte;
            self.len += 1;
        } else {
            self.overflowed = true;
        }
    }

    fn as_slice(&self) -> Option<&[u8]> {
        (!self.overflowed).then_some(&self.bytes[..self.len])
    }

    fn extend(&mut self, bytes: &[u8]) {
        let copied = bytes.len().min(N.saturating_sub(self.len));
        self.bytes[self.len..self.len + copied].copy_from_slice(&bytes[..copied]);
        self.len += copied;
        self.overflowed |= copied != bytes.len();
    }
}

#[derive(Debug, Default)]
struct StructuralStringVisitor {
    prefix: FixedText<MAX_STRUCTURAL_TEXT_PREFIX>,
    trimmed_text: FixedText<MAX_STRUCTURAL_TEXT_PREFIX>,
    rolling: FixedText<STRUCTURAL_ROLLING_BYTES>,
    exit_code: CodexExitCodeParser,
    wall_time: CodexWallTimeParser,
    timed_out: bool,
    decoded_len: usize,
    nonempty_trimmed: bool,
    saw_trailing_whitespace: bool,
    marker_scan_remaining: usize,
}

impl StructuralStringVisitor {
    fn feed_char(&mut self, character: char) -> Option<()> {
        let mut encoded = [0_u8; 4];
        let encoded = character.encode_utf8(&mut encoded);
        if character.is_whitespace() {
            self.saw_trailing_whitespace |= self.nonempty_trimmed;
        } else {
            if self.saw_trailing_whitespace {
                self.trimmed_text.overflowed = true;
            }
            self.nonempty_trimmed = true;
            for byte in encoded.bytes() {
                self.trimmed_text.push(byte);
            }
        }
        for byte in encoded.bytes() {
            self.feed_byte(byte)?;
        }
        Some(())
    }

    fn can_batch_plain_ascii(&self) -> bool {
        self.marker_scan_remaining == 0
    }

    fn feed_plain_ascii(&mut self, bytes: &[u8]) -> Option<()> {
        self.exit_code.feed_bytes(bytes);
        self.wall_time.feed_bytes(bytes);
        self.prefix.extend(bytes);
        self.decoded_len = self.decoded_len.checked_add(bytes.len())?;
        if !self.trimmed_text.overflowed {
            for byte in bytes {
                if byte.is_ascii_whitespace() {
                    self.saw_trailing_whitespace |= self.nonempty_trimmed;
                } else {
                    if self.saw_trailing_whitespace {
                        self.trimmed_text.overflowed = true;
                        break;
                    }
                    self.nonempty_trimmed = true;
                    self.trimmed_text.push(*byte);
                    if self.trimmed_text.overflowed {
                        break;
                    }
                }
            }
        } else if !self.nonempty_trimmed {
            self.nonempty_trimmed = bytes.iter().any(|byte| !byte.is_ascii_whitespace());
        }
        Some(())
    }

    fn feed_byte(&mut self, byte: u8) -> Option<()> {
        self.exit_code.feed_bytes(std::slice::from_ref(&byte));
        self.wall_time.feed_bytes(std::slice::from_ref(&byte));
        self.prefix.push(byte);
        self.decoded_len = self.decoded_len.checked_add(1)?;

        let marker_start = matches!(byte, b'P' | b't' | b'T' | b'W');
        if marker_start {
            if self.marker_scan_remaining == 0 {
                self.rolling = FixedText::default();
            }
            self.marker_scan_remaining = STRUCTURAL_ROLLING_BYTES;
        } else {
            self.marker_scan_remaining = self.marker_scan_remaining.saturating_sub(1);
        }
        if self.marker_scan_remaining != 0 {
            if self.rolling.len < STRUCTURAL_ROLLING_BYTES {
                self.rolling.push(byte);
            } else {
                self.rolling
                    .bytes
                    .copy_within(1..STRUCTURAL_ROLLING_BYTES, 0);
                self.rolling.bytes[STRUCTURAL_ROLLING_BYTES - 1] = byte;
            }
            let rolling = &self.rolling.bytes[..self.rolling.len];
            self.timed_out |= [
                b"timed out".as_slice(),
                b"Timed out",
                b"TIMED OUT",
                b"timed_out=true",
            ]
            .iter()
            .any(|marker| rolling.ends_with(marker));
        }
        Some(())
    }

    fn finish(self) -> JsonNodeSummary {
        let trimmed_text = self.trimmed_text.as_slice().unwrap_or_default();
        let script_completed = self.exit_code.script_completed();
        let exit_code = self.exit_code.exit_code();
        let duration_ms = self
            .wall_time
            .duration_ms()
            .and_then(|duration| u64::try_from(duration).ok());
        JsonNodeSummary {
            signals: StructuralOutputSignals {
                timed_out: self.timed_out,
                exit_code,
                duration_ms,
                explicit_failure: false,
                explicit_success: script_completed,
            },
            scalar: JsonScalarSummary {
                kind: JsonNodeKind::String,
                string_len: Some(self.decoded_len),
                string_text: self.prefix,
                string_nonempty: self.nonempty_trimmed,
                status_failure: status_is_failure(trimmed_text),
                status_success: status_is_success(trimmed_text),
                ..JsonScalarSummary::default()
            },
            direct_output_bytes: None,
        }
    }
}

struct StructuralJsonVisitor<'a> {
    bytes: &'a [u8],
    offset: usize,
    tokens_remaining: usize,
}

impl<'a> StructuralJsonVisitor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            offset: 0,
            tokens_remaining: MAX_JSON_VISITOR_TOKENS,
        }
    }

    fn payload(mut self) -> Option<JsonNodeSummary> {
        self.whitespace();
        self.take(b'{')?;
        self.whitespace();
        let mut payload = None;
        if self.peek() == Some(b'}') {
            return None;
        }
        loop {
            let key = self.key()?;
            self.whitespace();
            self.take(b':')?;
            self.whitespace();
            if key.kind == StructuralKey::Payload {
                payload = Some(self.value(1)?);
            } else {
                self.skip_value(1)?;
            }
            self.whitespace();
            match self.peek()? {
                b',' => {
                    self.offset += 1;
                    self.whitespace();
                }
                b'}' => {
                    self.offset += 1;
                    break;
                }
                _ => return None,
            }
        }
        self.whitespace();
        (self.offset == self.bytes.len()).then_some(())?;
        payload
    }

    fn value(&mut self, depth: usize) -> Option<JsonNodeSummary> {
        if depth > MAX_JSON_VISITOR_DEPTH {
            return None;
        }
        self.token()?;
        self.whitespace();
        match self.peek()? {
            b'{' => self.object(depth),
            b'[' => self.array(depth),
            b'"' => self.string_summary(),
            b't' => {
                self.literal(b"true")?;
                Some(JsonNodeSummary {
                    scalar: JsonScalarSummary {
                        kind: JsonNodeKind::Bool,
                        bool_value: Some(true),
                        ..JsonScalarSummary::default()
                    },
                    ..JsonNodeSummary::default()
                })
            }
            b'f' => {
                self.literal(b"false")?;
                Some(JsonNodeSummary {
                    scalar: JsonScalarSummary {
                        kind: JsonNodeKind::Bool,
                        bool_value: Some(false),
                        ..JsonScalarSummary::default()
                    },
                    ..JsonNodeSummary::default()
                })
            }
            b'n' => {
                self.literal(b"null")?;
                Some(JsonNodeSummary::default())
            }
            b'-' | b'0'..=b'9' => self.number(),
            _ => None,
        }
    }

    fn object(&mut self, depth: usize) -> Option<JsonNodeSummary> {
        self.take(b'{')?;
        self.whitespace();
        if self.peek() == Some(b'}') {
            self.offset += 1;
            return Some(JsonNodeSummary {
                scalar: JsonScalarSummary {
                    kind: JsonNodeKind::Object,
                    ..JsonScalarSummary::default()
                },
                ..JsonNodeSummary::default()
            });
        }
        let mut fields = StructuralObjectFields::default();
        let mut recursive_keys = StructuralRecursiveKeys::default();
        loop {
            let key = self.key()?;
            self.whitespace();
            self.take(b':')?;
            self.whitespace();
            let child = self.value(depth + 1)?;
            if matches!(key.kind, StructuralKey::Other | StructuralKey::Payload) {
                recursive_keys.observe(key.raw, child.signals)?;
            } else {
                fields.observe(key.kind, child);
            }
            self.whitespace();
            match self.peek()? {
                b',' => {
                    self.offset += 1;
                    self.whitespace();
                }
                b'}' => {
                    self.offset += 1;
                    break;
                }
                _ => return None,
            }
        }
        let mut recursive = StructuralObjectSignals::default();
        fields.merge_recursive_signals(&mut recursive);
        recursive_keys.merge_into(&mut recursive);
        Some(fields.apply(recursive.finish()))
    }

    fn array(&mut self, depth: usize) -> Option<JsonNodeSummary> {
        self.take(b'[')?;
        self.whitespace();
        if self.peek() == Some(b']') {
            self.offset += 1;
            return Some(JsonNodeSummary {
                scalar: JsonScalarSummary {
                    kind: JsonNodeKind::Array,
                    ..JsonScalarSummary::default()
                },
                ..JsonNodeSummary::default()
            });
        }
        let mut signals = StructuralOutputSignals::default();
        loop {
            let child = self.value(depth + 1)?;
            signals.merge_recursive(child.signals);
            self.whitespace();
            match self.peek()? {
                b',' => {
                    self.offset += 1;
                    self.whitespace();
                }
                b']' => {
                    self.offset += 1;
                    break;
                }
                _ => return None,
            }
        }
        Some(JsonNodeSummary {
            signals,
            scalar: JsonScalarSummary {
                kind: JsonNodeKind::Array,
                container_nonempty: true,
                ..JsonScalarSummary::default()
            },
            direct_output_bytes: None,
        })
    }

    fn key(&mut self) -> Option<ParsedStructuralKey<'a>> {
        self.token()?;
        let raw_start = self.offset.checked_add(1)?;
        let summary = self.string_summary()?;
        (summary.scalar.string_len? <= MAX_STRUCTURAL_KEY_BYTES).then_some(())?;
        let raw_end = self.offset.checked_sub(1)?;
        Some(ParsedStructuralKey {
            kind: StructuralKey::from_decoded(summary.scalar.string_text.as_slice()),
            raw: self.bytes.get(raw_start..raw_end)?,
        })
    }

    fn string_summary(&mut self) -> Option<JsonNodeSummary> {
        self.take(b'"')?;
        let mut visitor = StructuralStringVisitor::default();
        loop {
            if visitor.can_batch_plain_ascii() {
                let plain = plain_structural_ascii_bytes(self.bytes.get(self.offset..)?);
                if plain != 0 {
                    let end = self.offset.checked_add(plain)?;
                    visitor.feed_plain_ascii(self.bytes.get(self.offset..end)?)?;
                    self.offset = end;
                    continue;
                }
            }
            match self.peek()? {
                b'"' => {
                    self.offset += 1;
                    break;
                }
                b'\\' => {
                    self.offset += 1;
                    let escaped = self.peek()?;
                    self.offset += 1;
                    match escaped {
                        b'"' | b'\\' | b'/' => visitor.feed_char(char::from(escaped))?,
                        b'b' => visitor.feed_char('\u{0008}')?,
                        b'f' => visitor.feed_char('\u{000c}')?,
                        b'n' => visitor.feed_char('\n')?,
                        b'r' => visitor.feed_char('\r')?,
                        b't' => visitor.feed_char('\t')?,
                        b'u' => {
                            let first = self.unicode_escape()?;
                            let scalar = if (0xD800..=0xDBFF).contains(&first) {
                                self.take(b'\\')?;
                                self.take(b'u')?;
                                let second = self.unicode_escape()?;
                                if !(0xDC00..=0xDFFF).contains(&second) {
                                    return None;
                                }
                                0x1_0000
                                    + ((u32::from(first) - 0xD800) << 10)
                                    + (u32::from(second) - 0xDC00)
                            } else {
                                u32::from(first)
                            };
                            let character = char::from_u32(scalar)?;
                            visitor.feed_char(character)?;
                        }
                        _ => return None,
                    }
                }
                byte if byte < 0x20 => return None,
                byte if byte.is_ascii() => {
                    self.offset += 1;
                    visitor.feed_char(char::from(byte))?;
                }
                _ => {
                    let text = std::str::from_utf8(self.bytes.get(self.offset..)?).ok()?;
                    let character = text.chars().next()?;
                    self.offset = self.offset.checked_add(character.len_utf8())?;
                    visitor.feed_char(character)?;
                }
            }
        }
        Some(visitor.finish())
    }

    fn number(&mut self) -> Option<JsonNodeSummary> {
        let start = self.offset;
        while self.peek().is_some_and(|byte| {
            byte.is_ascii_digit() || matches!(byte, b'-' | b'+' | b'.' | b'e' | b'E')
        }) {
            self.offset += 1;
        }
        (self.offset.checked_sub(start)? <= MAX_STRUCTURAL_NUMBER_BYTES).then_some(())?;
        let text = std::str::from_utf8(self.bytes.get(start..self.offset)?).ok()?;
        let integer = (!text.contains(['.', 'e', 'E']))
            .then(|| text.parse::<i64>().ok())
            .flatten();
        let unsigned = (!text.contains(['.', 'e', 'E']))
            .then(|| text.parse::<u64>().ok())
            .flatten();
        Some(JsonNodeSummary {
            scalar: JsonScalarSummary {
                kind: JsonNodeKind::Number,
                integer,
                unsigned,
                ..JsonScalarSummary::default()
            },
            ..JsonNodeSummary::default()
        })
    }

    fn skip_value(&mut self, depth: usize) -> Option<()> {
        if depth > MAX_JSON_VISITOR_DEPTH {
            return None;
        }
        self.token()?;
        self.whitespace();
        match self.peek()? {
            b'"' => self.skip_string(),
            b'{' => {
                self.offset += 1;
                self.whitespace();
                if self.peek() == Some(b'}') {
                    self.offset += 1;
                    return Some(());
                }
                loop {
                    self.key()?;
                    self.whitespace();
                    self.take(b':')?;
                    self.skip_value(depth + 1)?;
                    self.whitespace();
                    match self.peek()? {
                        b',' => {
                            self.offset += 1;
                            self.whitespace();
                        }
                        b'}' => {
                            self.offset += 1;
                            break;
                        }
                        _ => return None,
                    }
                }
                Some(())
            }
            b'[' => {
                self.offset += 1;
                self.whitespace();
                if self.peek() == Some(b']') {
                    self.offset += 1;
                    return Some(());
                }
                loop {
                    self.skip_value(depth + 1)?;
                    self.whitespace();
                    match self.peek()? {
                        b',' => {
                            self.offset += 1;
                            self.whitespace();
                        }
                        b']' => {
                            self.offset += 1;
                            break;
                        }
                        _ => return None,
                    }
                }
                Some(())
            }
            b't' => self.literal(b"true"),
            b'f' => self.literal(b"false"),
            b'n' => self.literal(b"null"),
            b'-' | b'0'..=b'9' => {
                let start = self.offset;
                while self.peek().is_some_and(|byte| {
                    byte.is_ascii_digit() || matches!(byte, b'-' | b'+' | b'.' | b'e' | b'E')
                }) {
                    self.offset += 1;
                }
                (self.offset.checked_sub(start)? <= MAX_STRUCTURAL_NUMBER_BYTES).then_some(())?;
                Some(())
            }
            _ => None,
        }
    }

    fn skip_string(&mut self) -> Option<()> {
        self.take(b'"')?;
        loop {
            match self.peek()? {
                b'"' => {
                    self.offset += 1;
                    return Some(());
                }
                b'\\' => {
                    self.offset = self.offset.checked_add(2)?;
                    if self.bytes.get(self.offset - 1) == Some(&b'u') {
                        self.offset = self.offset.checked_add(4)?;
                    }
                    (self.offset <= self.bytes.len()).then_some(())?;
                }
                _ => self.offset += 1,
            }
        }
    }

    fn unicode_escape(&mut self) -> Option<u16> {
        let end = self.offset.checked_add(4)?;
        let value = parse_hex_u16(self.bytes.get(self.offset..end)?)?;
        self.offset = end;
        Some(value)
    }

    fn whitespace(&mut self) {
        while self.peek().is_some_and(|byte| byte.is_ascii_whitespace()) {
            self.offset += 1;
        }
    }

    fn literal(&mut self, expected: &[u8]) -> Option<()> {
        let end = self.offset.checked_add(expected.len())?;
        (self.bytes.get(self.offset..end)? == expected).then_some(())?;
        self.offset = end;
        Some(())
    }

    fn take(&mut self, expected: u8) -> Option<()> {
        (self.peek()? == expected).then_some(())?;
        self.offset += 1;
        Some(())
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.offset).copied()
    }

    fn token(&mut self) -> Option<()> {
        self.tokens_remaining = self.tokens_remaining.checked_sub(1)?;
        Some(())
    }
}

fn plain_structural_ascii_bytes(bytes: &[u8]) -> usize {
    const HIGH_BITS: u64 = 0x8080_8080_8080_8080;

    let mut offset = 0;
    while offset + 8 <= bytes.len() {
        let Some(chunk) = bytes.get(offset..offset + 8) else {
            break;
        };
        let Ok(chunk) = <[u8; 8]>::try_from(chunk) else {
            break;
        };
        let word = u64::from_ne_bytes(chunk);
        if word & HIGH_BITS != 0
            || b"\"\\PtTW"
                .iter()
                .copied()
                .any(|needle| word_contains_byte(word, needle))
        {
            break;
        }
        offset += 8;
    }
    offset
        + bytes[offset..]
            .iter()
            .position(|byte| {
                !byte.is_ascii()
                    || *byte < 0x20
                    || matches!(*byte, b'"' | b'\\' | b'P' | b't' | b'T' | b'W')
            })
            .unwrap_or(bytes.len() - offset)
}

fn word_contains_byte(word: u64, needle: u8) -> bool {
    const LOW_BITS: u64 = 0x0101_0101_0101_0101;
    const HIGH_BITS: u64 = 0x8080_8080_8080_8080;
    let compared = word ^ u64::from(needle).wrapping_mul(LOW_BITS);
    compared.wrapping_sub(LOW_BITS) & !compared & HIGH_BITS != 0
}

fn probe_structural_output(line: &[u8]) -> serde_json::Result<CodexStructuralOutput> {
    let payload = StructuralJsonVisitor::new(line).payload().ok_or_else(|| {
        <serde_json::Error as serde::de::Error>::custom(
            "unable to visit the decoded Codex output payload",
        )
    })?;
    let signals = payload.signals;
    let outcome = if signals.timed_out {
        OutputOutcome::Timeout
    } else if signals.exit_code.is_some_and(|code| code != 0) || signals.explicit_failure {
        OutputOutcome::Failure
    } else if signals.exit_code == Some(0) || signals.explicit_success {
        OutputOutcome::Success
    } else {
        OutputOutcome::Unknown
    };
    Ok(CodexStructuralOutput {
        outcome: OutputOutcomeMetadata {
            outcome,
            exit_code: signals.exit_code,
            duration_ms: signals.duration_ms,
        },
        output_bytes: payload.direct_output_bytes,
    })
}

fn status_is_failure(value: &[u8]) -> bool {
    let Some(value) = std::str::from_utf8(value).ok().map(str::trim) else {
        return false;
    };
    [
        "failed",
        "failure",
        "error",
        "errored",
        "timeout",
        "timed_out",
        "timedout",
        "cancelled",
        "canceled",
    ]
    .iter()
    .any(|expected| value.eq_ignore_ascii_case(expected))
}

fn status_is_success(value: &[u8]) -> bool {
    let Some(value) = std::str::from_utf8(value).ok().map(str::trim) else {
        return false;
    };
    [
        "success",
        "succeeded",
        "complete",
        "completed",
        "ok",
        "passed",
    ]
    .iter()
    .any(|expected| value.eq_ignore_ascii_case(expected))
}

fn decoded_json_key_is_before_or_same(candidate: &[u8], current: &[u8]) -> bool {
    decoded_json_key_cmp(candidate, current) != Ordering::Greater
}

fn decoded_json_key_cmp(candidate: &[u8], current: &[u8]) -> Ordering {
    let mut candidate = DecodedJsonBytes::new(candidate);
    let mut current = DecodedJsonBytes::new(current);
    loop {
        match (candidate.next(), current.next()) {
            (Some(left), Some(right)) => match left.cmp(&right) {
                Ordering::Less => return Ordering::Less,
                Ordering::Greater => return Ordering::Greater,
                Ordering::Equal => {}
            },
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (None, None) => return Ordering::Equal,
        }
    }
}

struct DecodedJsonBytes<'a> {
    raw: &'a [u8],
    offset: usize,
    pending: [u8; 4],
    pending_offset: usize,
    pending_len: usize,
}

impl<'a> DecodedJsonBytes<'a> {
    fn new(raw: &'a [u8]) -> Self {
        Self {
            raw,
            offset: 0,
            pending: [0; 4],
            pending_offset: 0,
            pending_len: 0,
        }
    }
}

impl Iterator for DecodedJsonBytes<'_> {
    type Item = u8;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pending_offset < self.pending_len {
            let byte = self.pending[self.pending_offset];
            self.pending_offset += 1;
            return Some(byte);
        }
        let byte = *self.raw.get(self.offset)?;
        self.offset += 1;
        if byte != b'\\' {
            return Some(byte);
        }
        let escaped = *self.raw.get(self.offset)?;
        self.offset += 1;
        let byte = match escaped {
            b'"' | b'\\' | b'/' => return Some(escaped),
            b'b' => return Some(0x08),
            b'f' => return Some(0x0c),
            b'n' => return Some(b'\n'),
            b'r' => return Some(b'\r'),
            b't' => return Some(b'\t'),
            b'u' => {
                let end = self.offset.checked_add(4)?;
                let first = parse_hex_u16(self.raw.get(self.offset..end)?)?;
                self.offset = end;
                if (0xD800..=0xDBFF).contains(&first) {
                    let escape_end = self.offset.checked_add(2)?;
                    if self.raw.get(self.offset..escape_end)? != b"\\u" {
                        return None;
                    }
                    self.offset = escape_end;
                    let end = self.offset.checked_add(4)?;
                    let second = parse_hex_u16(self.raw.get(self.offset..end)?)?;
                    self.offset = end;
                    if !(0xDC00..=0xDFFF).contains(&second) {
                        return None;
                    }
                    0x1_0000 + ((u32::from(first) - 0xD800) << 10) + (u32::from(second) - 0xDC00)
                } else {
                    u32::from(first)
                }
            }
            _ => return None,
        };
        let character = char::from_u32(byte)?;
        self.pending_len = character.encode_utf8(&mut self.pending).len();
        self.pending_offset = 1;
        Some(self.pending[0])
    }
}

fn parse_hex_u16(value: &[u8]) -> Option<u16> {
    if value.len() != 4 {
        return None;
    }
    value.iter().try_fold(0_u16, |number, byte| {
        let digit = match byte {
            b'0'..=b'9' => u16::from(*byte - b'0'),
            b'a'..=b'f' => u16::from(*byte - b'a' + 10),
            b'A'..=b'F' => u16::from(*byte - b'A' + 10),
            _ => return None,
        };
        number.checked_mul(16)?.checked_add(digit)
    })
}

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
