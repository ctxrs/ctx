use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    fmt,
    io::Read,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType};
use serde::{
    de::{DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use serde_json::Value;

use crate::{common::time::parse_rfc3339_utc, MAX_PROVIDER_SQLITE_VALUE_BYTES};

use super::{
    dto::{ZedNativeEncoding, ZedNativeOutputCounters, ZedNativeRejectionKind},
    publication::ZedDecodedCoreEvent,
    ZedNativeResult,
};

const ZED_THREAD_VERSION: &str = "0.3.0";
const ZED_MAX_MESSAGES_PER_THREAD: usize = 65_536;
const ZED_MAX_SAFE_TOUCHES_PER_EVENT: usize = 256;
const ZED_MAX_SAFE_TOUCH_BYTES: usize = 4_096;
const ZED_ENCODING_DIAGNOSTIC_MAX_CHARS: usize = 128;

/// Validated thread metadata plus the bounded source bytes needed for a second,
/// streaming pass. Message wires and event drafts are never retained as a corpus.
pub(super) struct ZedDecodedPayload<'a> {
    pub(super) encoding: ZedNativeEncoding,
    pub(super) title: Option<String>,
    pub(super) updated_at: Option<DateTime<Utc>>,
    pub(super) decoded_bytes: u64,
    json: Cow<'a, [u8]>,
}

pub(super) struct ZedDecodeFailure {
    pub(super) kind: ZedNativeRejectionKind,
    pub(super) reason: String,
}

pub(super) enum ZedDecodeOutcome<'a> {
    Decoded(ZedDecodedPayload<'a>),
    Rejected(ZedDecodeFailure),
}

pub(super) fn decode_zed_native_payload<'a>(
    thread_id: &str,
    data_type: &str,
    data: &'a [u8],
    row_updated_at: DateTime<Utc>,
) -> ZedNativeResult<ZedDecodeOutcome<'a>> {
    let (encoding, json): (ZedNativeEncoding, Cow<'_, [u8]>) = match data_type {
        "json" => (ZedNativeEncoding::Json, Cow::Borrowed(data)),
        "zstd" => match decode_zstd_bounded(thread_id, data) {
            Ok(json) => (ZedNativeEncoding::Zstd, Cow::Owned(json)),
            Err(failure) => return Ok(ZedDecodeOutcome::Rejected(failure)),
        },
        other => {
            return Ok(ZedDecodeOutcome::Rejected(ZedDecodeFailure {
                kind: ZedNativeRejectionKind::UnsupportedEncoding,
                reason: format!(
                    "Zed thread `{thread_id}` uses unsupported data encoding {}",
                    encoding_diagnostic(other)
                ),
            }));
        }
    };
    let wire: ZedThreadWire = match serde_json::from_slice(&json) {
        Ok(wire) => wire,
        Err(error) => {
            return Ok(ZedDecodeOutcome::Rejected(ZedDecodeFailure {
                kind: ZedNativeRejectionKind::MalformedJson,
                reason: format!("Zed thread `{thread_id}` contains malformed JSON: {error}"),
            }));
        }
    };
    if wire
        .version
        .as_deref()
        .is_some_and(|version| version != ZED_THREAD_VERSION)
    {
        return Ok(ZedDecodeOutcome::Rejected(ZedDecodeFailure {
            kind: ZedNativeRejectionKind::UnsupportedThreadVersion,
            reason: format!(
                "Zed thread `{thread_id}` uses unsupported DbThread version {:?}",
                wire.version.as_deref()
            ),
        }));
    }
    let Some(messages) = wire.messages else {
        return Ok(ZedDecodeOutcome::Rejected(ZedDecodeFailure {
            kind: ZedNativeRejectionKind::MalformedThread,
            reason: format!("Zed thread `{thread_id}` is missing DbThread.messages"),
        }));
    };
    if messages.count > ZED_MAX_MESSAGES_PER_THREAD {
        return Ok(ZedDecodeOutcome::Rejected(ZedDecodeFailure {
            kind: ZedNativeRejectionKind::MalformedThread,
            reason: format!(
                "Zed thread `{thread_id}` exceeds {ZED_MAX_MESSAGES_PER_THREAD} messages"
            ),
        }));
    }
    let occurred_at = wire
        .updated_at
        .as_deref()
        .and_then(parse_rfc3339_utc)
        .unwrap_or(row_updated_at);
    let decoded_bytes = u64::try_from(json.len()).unwrap_or(u64::MAX);
    Ok(ZedDecodeOutcome::Decoded(ZedDecodedPayload {
        encoding,
        title: wire.title,
        updated_at: Some(occurred_at),
        decoded_bytes,
        json,
    }))
}

impl ZedDecodedPayload<'_> {
    /// Decodes and publishes one message at a time after the first pass has proved
    /// that the whole thread is structurally valid.
    pub(super) fn emit_events(
        &self,
        thread_ordinal: u64,
        emit: &mut dyn FnMut(ZedDecodedCoreEvent) -> ZedNativeResult<()>,
    ) -> ZedNativeResult<ZedNativeOutputCounters> {
        let occurred_at = self.updated_at.ok_or_else(|| {
            super::ZedNativePathError::UnsupportedSchema(
                "validated Zed payload is missing its normalized timestamp".to_owned(),
            )
        })?;
        let mut output = ZedNativeOutputCounters::default();
        let mut emit_error = None;
        let parse_result = {
            let mut stream = ZedEventStream {
                thread_ordinal,
                occurred_at,
                output: &mut output,
                emit,
                emit_error: &mut emit_error,
            };
            let mut deserializer = serde_json::Deserializer::from_slice(&self.json);
            ZedThreadEventsSeed {
                stream: &mut stream,
            }
            .deserialize(&mut deserializer)
            .and_then(|()| deserializer.end())
        };
        if let Some(error) = emit_error {
            return Err(error);
        }
        parse_result.map_err(|error| {
            super::ZedNativePathError::UnsupportedSchema(format!(
                "validated Zed payload could not be streamed: {error}"
            ))
        })?;
        Ok(output)
    }
}

fn encoding_diagnostic(encoding: &str) -> String {
    let prefix = encoding
        .chars()
        .flat_map(char::escape_default)
        .take(ZED_ENCODING_DIAGNOSTIC_MAX_CHARS)
        .collect::<String>();
    format!("{prefix:?} ({} bytes)", encoding.len())
}

struct ZedEventStream<'a> {
    thread_ordinal: u64,
    occurred_at: DateTime<Utc>,
    output: &'a mut ZedNativeOutputCounters,
    emit: &'a mut dyn FnMut(ZedDecodedCoreEvent) -> ZedNativeResult<()>,
    emit_error: &'a mut Option<super::ZedNativePathError>,
}

struct ZedThreadEventsSeed<'stream, 'emit> {
    stream: &'stream mut ZedEventStream<'emit>,
}

impl<'de> DeserializeSeed<'de> for ZedThreadEventsSeed<'_, '_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(ZedThreadEventsVisitor {
            stream: self.stream,
        })
    }
}

struct ZedThreadEventsVisitor<'stream, 'emit> {
    stream: &'stream mut ZedEventStream<'emit>,
}

impl<'de> Visitor<'de> for ZedThreadEventsVisitor<'_, '_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a validated Zed thread object")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut saw_messages = false;
        while let Some(key) = map.next_key::<String>()? {
            if key == "messages" {
                if saw_messages {
                    return Err(serde::de::Error::duplicate_field("messages"));
                }
                map.next_value_seed(ZedMessagesSeed {
                    stream: self.stream,
                })?;
                saw_messages = true;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        if !saw_messages {
            return Err(serde::de::Error::missing_field("messages"));
        }
        Ok(())
    }
}

struct ZedMessagesSeed<'stream, 'emit> {
    stream: &'stream mut ZedEventStream<'emit>,
}

impl<'de> DeserializeSeed<'de> for ZedMessagesSeed<'_, '_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(ZedMessagesVisitor {
            stream: self.stream,
        })
    }
}

struct ZedMessagesVisitor<'stream, 'emit> {
    stream: &'stream mut ZedEventStream<'emit>,
}

impl<'de> Visitor<'de> for ZedMessagesVisitor<'_, '_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a validated Zed message sequence")
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut message_ordinal = 0_u64;
        while let Some(message) = sequence.next_element::<ZedMessageWire>()? {
            if let Some(event) = decode_message(
                self.stream.thread_ordinal,
                message_ordinal,
                message,
                self.stream.occurred_at,
                self.stream.output,
            ) {
                if let Err(error) = (self.stream.emit)(event) {
                    *self.stream.emit_error = Some(error);
                    return Err(serde::de::Error::custom(
                        "Zed page sink rejected a streamed event",
                    ));
                }
            }
            message_ordinal = message_ordinal.saturating_add(1);
        }
        Ok(())
    }
}

fn decode_zstd_bounded(
    thread_id: &str,
    data: &[u8],
) -> std::result::Result<Vec<u8>, ZedDecodeFailure> {
    let mut decoder = zstd::stream::read::Decoder::new(data).map_err(|error| ZedDecodeFailure {
        kind: ZedNativeRejectionKind::InvalidCompression,
        reason: format!("Zed thread `{thread_id}` has invalid zstd data: {error}"),
    })?;
    let mut limited = decoder
        .by_ref()
        .take(MAX_PROVIDER_SQLITE_VALUE_BYTES as u64 + 1);
    let mut out = Vec::new();
    limited
        .read_to_end(&mut out)
        .map_err(|error| ZedDecodeFailure {
            kind: ZedNativeRejectionKind::InvalidCompression,
            reason: format!("Zed thread `{thread_id}` has invalid zstd data: {error}"),
        })?;
    if out.len() > MAX_PROVIDER_SQLITE_VALUE_BYTES {
        return Err(ZedDecodeFailure {
            kind: ZedNativeRejectionKind::OversizedDecompression,
            reason: format!(
                "Zed thread `{thread_id}` exceeds {MAX_PROVIDER_SQLITE_VALUE_BYTES} decompressed bytes"
            ),
        });
    }
    Ok(out)
}

fn decode_message(
    thread_ordinal: u64,
    message_ordinal: u64,
    message: ZedMessageWire,
    occurred_at: DateTime<Utc>,
    output: &mut ZedNativeOutputCounters,
) -> Option<ZedDecodedCoreEvent> {
    match message {
        ZedMessageWire::User(user) => {
            let body = retained_content_text(&user.content, output);
            body.map(|body| ZedDecodedCoreEvent {
                provider_message_id: nonempty_owned(user.id),
                thread_ordinal,
                message_ordinal,
                event_type: EventType::Message,
                role: EventRole::User,
                occurred_at,
                kind: "user",
                call_ids: Vec::new(),
                body,
                safe_file_touches: Vec::new(),
            })
        }
        ZedMessageWire::Agent(agent) => {
            classify_result_map(&agent.tool_results, output);
            let mut parts = Vec::new();
            let mut call_ids = Vec::new();
            let mut touches = BTreeSet::new();
            for content in agent.content {
                match content {
                    ZedContentWire::Text(text) => push_nonempty(&mut parts, text),
                    ZedContentWire::Thinking(text) => {
                        push_nonempty(&mut parts, format!("<think>{text}</think>"));
                    }
                    ZedContentWire::RedactedThinking => {
                        parts.push("<redacted_thinking />".to_owned());
                    }
                    ZedContentWire::ToolUse(tool) => {
                        let name = tool
                            .name
                            .as_deref()
                            .filter(|name| !name.trim().is_empty())
                            .unwrap_or("tool");
                        parts.push(format!("tool call: {name}"));
                        if tool.input.is_some()
                            || tool
                                .raw_input
                                .as_deref()
                                .is_some_and(|raw| !raw.trim().is_empty())
                        {
                            parts.push("tool input: present".to_owned());
                        }
                        if let Some(id) = nonempty_owned(tool.id) {
                            call_ids.push(id);
                        }
                        if let Some(input) = tool.input.as_ref() {
                            collect_safe_touches(input, &mut touches);
                        }
                    }
                    ZedContentWire::ToolResult(result) => classify_result(&result, output),
                    ZedContentWire::Mention(content) => {
                        if let Some(content) = content {
                            push_nonempty(&mut parts, content);
                        }
                    }
                    ZedContentWire::Image => parts.push("<image />".to_owned()),
                    ZedContentWire::Unknown(kind) => {
                        parts.push(format!("[zed content: {kind}]"));
                    }
                }
            }
            if parts.is_empty() {
                return None;
            }
            let event_type = if call_ids.is_empty() {
                EventType::Message
            } else {
                EventType::ToolCall
            };
            Some(ZedDecodedCoreEvent {
                provider_message_id: None,
                thread_ordinal,
                message_ordinal,
                event_type,
                role: EventRole::Assistant,
                occurred_at,
                kind: if event_type == EventType::ToolCall {
                    "agent_tool_call"
                } else {
                    "agent"
                },
                call_ids,
                body: parts.join("\n"),
                safe_file_touches: touches.into_iter().collect(),
            })
        }
        ZedMessageWire::Compaction(summary) => Some(ZedDecodedCoreEvent {
            provider_message_id: None,
            thread_ordinal,
            message_ordinal,
            event_type: EventType::Summary,
            role: EventRole::System,
            occurred_at,
            kind: "compaction",
            call_ids: Vec::new(),
            body: summary
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| "Zed compaction".to_owned()),
            safe_file_touches: Vec::new(),
        }),
        ZedMessageWire::Resume => Some(ZedDecodedCoreEvent {
            provider_message_id: None,
            thread_ordinal,
            message_ordinal,
            event_type: EventType::Message,
            role: EventRole::User,
            occurred_at,
            kind: "resume",
            call_ids: Vec::new(),
            body: "[resume]".to_owned(),
            safe_file_touches: Vec::new(),
        }),
        ZedMessageWire::Unknown(kind) => Some(ZedDecodedCoreEvent {
            provider_message_id: None,
            thread_ordinal,
            message_ordinal,
            event_type: EventType::Notice,
            role: EventRole::Unknown,
            occurred_at,
            kind: "unknown",
            call_ids: Vec::new(),
            body: format!("[zed message: {kind}]"),
            safe_file_touches: Vec::new(),
        }),
    }
}

fn retained_content_text(
    content: &[ZedContentWire],
    output: &mut ZedNativeOutputCounters,
) -> Option<String> {
    let mut parts = Vec::new();
    for item in content {
        match item {
            ZedContentWire::Text(text) => push_nonempty(&mut parts, text.clone()),
            ZedContentWire::Thinking(text) => {
                push_nonempty(&mut parts, format!("<think>{text}</think>"));
            }
            ZedContentWire::RedactedThinking => {
                parts.push("<redacted_thinking />".to_owned());
            }
            ZedContentWire::ToolResult(result) => classify_result(result, output),
            ZedContentWire::Mention(Some(content)) => {
                push_nonempty(&mut parts, content.clone());
            }
            ZedContentWire::Image => parts.push("<image />".to_owned()),
            ZedContentWire::Unknown(kind) => parts.push(format!("[zed content: {kind}]")),
            ZedContentWire::ToolUse(_) | ZedContentWire::Mention(None) => {}
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn classify_result_map(results: &ZedToolResultsWire, output: &mut ZedNativeOutputCounters) {
    for result in results.results.values() {
        classify_result(result, output);
    }
}

fn classify_result(result: &ZedResultWire, output: &mut ZedNativeOutputCounters) {
    output.native_results_observed = output.native_results_observed.saturating_add(1);
    let body_bytes = result.content.string_bytes;
    output.result_body_bytes_observed =
        output.result_body_bytes_observed.saturating_add(body_bytes);
    let classification = result.shape_is_unambiguous.then_some((
        result.is_error,
        result
            .output
            .as_ref()
            .and_then(|metadata| metadata.status.as_deref()),
    ));
    match classification {
        Some((Some(false), Some("ok"))) => {
            output.native_results_success = output.native_results_success.saturating_add(1);
        }
        Some((Some(true), _)) => {
            output.native_results_failure = output.native_results_failure.saturating_add(1);
        }
        _ => {
            output.native_results_unknown = output.native_results_unknown.saturating_add(1);
        }
    }
}

#[derive(Default)]
struct ZedResultWire {
    is_error: Option<bool>,
    content: DiscardedJson,
    output: Option<ZedResultOutputWire>,
    shape_is_unambiguous: bool,
}

impl<'de> Deserialize<'de> for ZedResultWire {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(ZedResultVisitor)
    }
}

struct ZedResultVisitor;

impl<'de> Visitor<'de> for ZedResultVisitor {
    type Value = ZedResultWire;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("any Zed tool-result shape")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut result = ZedResultWire {
            shape_is_unambiguous: true,
            ..ZedResultWire::default()
        };
        let mut saw_is_error = false;
        let mut saw_content = false;
        let mut saw_output = false;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "is_error" => {
                    let value = map.next_value::<TolerantBool>()?.0;
                    if saw_is_error || value.is_none() {
                        result.shape_is_unambiguous = false;
                    } else {
                        result.is_error = value;
                    }
                    saw_is_error = true;
                }
                "content" => {
                    let value = map.next_value::<DiscardedJson>()?;
                    result.content.string_bytes = result
                        .content
                        .string_bytes
                        .saturating_add(value.string_bytes);
                    if saw_content {
                        result.shape_is_unambiguous = false;
                    }
                    saw_content = true;
                }
                "output" => {
                    let parsed = map.next_value::<TolerantResultOutput>()?;
                    if saw_output || !parsed.valid {
                        result.shape_is_unambiguous = false;
                    } else {
                        result.output = parsed.value;
                    }
                    saw_output = true;
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(result)
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(ZedResultWire::default())
    }

    fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E> {
        Ok(ZedResultWire::default())
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
        Ok(ZedResultWire::default())
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E> {
        Ok(ZedResultWire::default())
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
        Ok(ZedResultWire::default())
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(ZedResultWire::default())
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(ZedResultWire::default())
    }

    fn visit_borrowed_str<E>(self, _value: &'de str) -> std::result::Result<Self::Value, E> {
        Ok(ZedResultWire::default())
    }

    fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E> {
        Ok(ZedResultWire::default())
    }

    fn visit_string<E>(self, _value: String) -> std::result::Result<Self::Value, E> {
        Ok(ZedResultWire::default())
    }
}

struct ZedResultOutputWire {
    status: Option<String>,
}

struct TolerantResultOutput {
    value: Option<ZedResultOutputWire>,
    valid: bool,
}

impl<'de> Deserialize<'de> for TolerantResultOutput {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(TolerantResultOutputVisitor)
    }
}

struct TolerantResultOutputVisitor;

impl<'de> Visitor<'de> for TolerantResultOutputVisitor {
    type Value = TolerantResultOutput;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Zed result-output object or an ignored value")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut status = None;
        let mut saw_status = false;
        let mut valid = true;
        while let Some(key) = map.next_key::<String>()? {
            if key == "status" {
                let parsed = map.next_value::<TolerantString>()?.0;
                if saw_status || parsed.is_none() {
                    valid = false;
                } else {
                    status = parsed;
                }
                saw_status = true;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(TolerantResultOutput {
            value: Some(ZedResultOutputWire { status }),
            valid,
        })
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(TolerantResultOutput {
            value: None,
            valid: false,
        })
    }

    fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E> {
        Ok(TolerantResultOutput {
            value: None,
            valid: false,
        })
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
        Ok(TolerantResultOutput {
            value: None,
            valid: false,
        })
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E> {
        Ok(TolerantResultOutput {
            value: None,
            valid: false,
        })
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
        Ok(TolerantResultOutput {
            value: None,
            valid: false,
        })
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(TolerantResultOutput {
            value: None,
            valid: false,
        })
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(TolerantResultOutput {
            value: None,
            valid: false,
        })
    }

    fn visit_borrowed_str<E>(self, _value: &'de str) -> std::result::Result<Self::Value, E> {
        Ok(TolerantResultOutput {
            value: None,
            valid: false,
        })
    }

    fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E> {
        Ok(TolerantResultOutput {
            value: None,
            valid: false,
        })
    }

    fn visit_string<E>(self, _value: String) -> std::result::Result<Self::Value, E> {
        Ok(TolerantResultOutput {
            value: None,
            valid: false,
        })
    }
}

struct TolerantBool(Option<bool>);

impl<'de> Deserialize<'de> for TolerantBool {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(TolerantBoolVisitor)
    }
}

struct TolerantBoolVisitor;

impl<'de> Visitor<'de> for TolerantBoolVisitor {
    type Value = TolerantBool;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a boolean or an ignored value")
    }

    fn visit_bool<E>(self, value: bool) -> std::result::Result<Self::Value, E> {
        Ok(TolerantBool(Some(value)))
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(TolerantBool(None))
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(TolerantBool(None))
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
        Ok(TolerantBool(None))
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E> {
        Ok(TolerantBool(None))
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
        Ok(TolerantBool(None))
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(TolerantBool(None))
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(TolerantBool(None))
    }

    fn visit_borrowed_str<E>(self, _value: &'de str) -> std::result::Result<Self::Value, E> {
        Ok(TolerantBool(None))
    }

    fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E> {
        Ok(TolerantBool(None))
    }

    fn visit_string<E>(self, _value: String) -> std::result::Result<Self::Value, E> {
        Ok(TolerantBool(None))
    }
}

struct TolerantString(Option<String>);

impl<'de> Deserialize<'de> for TolerantString {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(TolerantStringVisitor)
    }
}

struct TolerantStringVisitor;

impl<'de> Visitor<'de> for TolerantStringVisitor {
    type Value = TolerantString;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a string or an ignored value")
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> std::result::Result<Self::Value, E> {
        Ok(TolerantString(Some(value.to_owned())))
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E> {
        Ok(TolerantString(Some(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E> {
        Ok(TolerantString(Some(value)))
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(TolerantString(None))
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(TolerantString(None))
    }

    fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E> {
        Ok(TolerantString(None))
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
        Ok(TolerantString(None))
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E> {
        Ok(TolerantString(None))
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
        Ok(TolerantString(None))
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(TolerantString(None))
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(TolerantString(None))
    }
}

fn collect_safe_touches(value: &Value, touches: &mut BTreeSet<String>) {
    if touches.len() >= ZED_MAX_SAFE_TOUCHES_PER_EVENT {
        return;
    }
    match value {
        Value::Array(values) => {
            for value in values {
                collect_safe_touches(value, touches);
                if touches.len() >= ZED_MAX_SAFE_TOUCHES_PER_EVENT {
                    break;
                }
            }
        }
        Value::Object(object) => {
            for (key, value) in object {
                if matches!(key.as_str(), "path" | "file_path" | "file")
                    && value.as_str().is_some_and(|path| {
                        !path.trim().is_empty() && path.len() <= ZED_MAX_SAFE_TOUCH_BYTES
                    })
                {
                    if let Some(path) = value.as_str() {
                        touches.insert(path.to_owned());
                    }
                } else {
                    collect_safe_touches(value, touches);
                }
                if touches.len() >= ZED_MAX_SAFE_TOUCHES_PER_EVENT {
                    break;
                }
            }
        }
        _ => {}
    }
}

fn push_nonempty(parts: &mut Vec<String>, value: String) {
    if !value.trim().is_empty() {
        parts.push(value);
    }
}

fn nonempty_owned(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

#[derive(Deserialize)]
struct ZedThreadWire {
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    messages: Option<ZedValidatedMessages>,
}

struct ZedValidatedMessages {
    count: usize,
}

impl<'de> Deserialize<'de> for ZedValidatedMessages {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(ZedValidatedMessagesVisitor)
    }
}

struct ZedValidatedMessagesVisitor;

impl<'de> Visitor<'de> for ZedValidatedMessagesVisitor {
    type Value = ZedValidatedMessages;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Zed message sequence")
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut count = 0_usize;
        while sequence.next_element::<ZedMessageWire>()?.is_some() {
            count = count.saturating_add(1);
        }
        Ok(ZedValidatedMessages { count })
    }
}

enum ZedMessageWire {
    User(ZedUserWire),
    Agent(ZedAgentWire),
    Compaction(Option<String>),
    Resume,
    Unknown(String),
}

impl<'de> Deserialize<'de> for ZedMessageWire {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(ZedMessageVisitor)
    }
}

struct ZedMessageVisitor;

impl<'de> Visitor<'de> for ZedMessageVisitor {
    type Value = ZedMessageWire;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Zed externally tagged message")
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> std::result::Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(if value == "Resume" {
            ZedMessageWire::Resume
        } else {
            ZedMessageWire::Unknown(value.to_owned())
        })
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(if value == "Resume" {
            ZedMessageWire::Resume
        } else {
            ZedMessageWire::Unknown(value.to_owned())
        })
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(if value == "Resume" {
            ZedMessageWire::Resume
        } else {
            ZedMessageWire::Unknown(value)
        })
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let kind = map
            .next_key::<String>()?
            .ok_or_else(|| serde::de::Error::custom("Zed message tag is empty"))?;
        let message = match kind.as_str() {
            "User" => ZedMessageWire::User(map.next_value()?),
            "Agent" => ZedMessageWire::Agent(map.next_value()?),
            "Compaction" => {
                let value: ZedCompactionWire = map.next_value()?;
                ZedMessageWire::Compaction(value.summary)
            }
            "Resume" => {
                map.next_value::<IgnoredAny>()?;
                ZedMessageWire::Resume
            }
            _ => {
                map.next_value::<IgnoredAny>()?;
                ZedMessageWire::Unknown(kind)
            }
        };
        if map.next_key::<IgnoredAny>()?.is_some() {
            return Err(serde::de::Error::custom(
                "Zed message must contain exactly one external tag",
            ));
        }
        Ok(message)
    }
}

#[derive(Deserialize)]
struct ZedUserWire {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    content: Vec<ZedContentWire>,
}

#[derive(Deserialize)]
struct ZedAgentWire {
    #[serde(default)]
    content: Vec<ZedContentWire>,
    #[serde(default)]
    tool_results: ZedToolResultsWire,
}

#[derive(Default)]
struct ZedToolResultsWire {
    results: BTreeMap<String, ZedResultWire>,
}

impl<'de> Deserialize<'de> for ZedToolResultsWire {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(ZedToolResultsVisitor)
    }
}

struct ZedToolResultsVisitor;

impl<'de> Visitor<'de> for ZedToolResultsVisitor {
    type Value = ZedToolResultsWire;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Zed tool-results object or discarded output-only evidence")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut results = BTreeMap::new();
        while let Some((key, result)) = map.next_entry::<String, ZedResultWire>()? {
            results.insert(key, result);
        }
        Ok(ZedToolResultsWire { results })
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(ZedToolResultsWire::default())
    }

    fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E> {
        Ok(ZedToolResultsWire::default())
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
        Ok(ZedToolResultsWire::default())
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E> {
        Ok(ZedToolResultsWire::default())
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
        Ok(ZedToolResultsWire::default())
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(ZedToolResultsWire::default())
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(ZedToolResultsWire::default())
    }

    fn visit_borrowed_str<E>(self, _value: &'de str) -> std::result::Result<Self::Value, E> {
        Ok(ZedToolResultsWire::default())
    }

    fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E> {
        Ok(ZedToolResultsWire::default())
    }

    fn visit_string<E>(self, _value: String) -> std::result::Result<Self::Value, E> {
        Ok(ZedToolResultsWire::default())
    }
}

#[derive(Deserialize)]
struct ZedCompactionWire {
    #[serde(default, rename = "Summary")]
    summary: Option<String>,
}

enum ZedContentWire {
    Text(String),
    Thinking(String),
    RedactedThinking,
    ToolUse(ZedToolUseWire),
    ToolResult(ZedResultWire),
    Mention(Option<String>),
    Image,
    Unknown(String),
}

impl<'de> Deserialize<'de> for ZedContentWire {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(ZedContentVisitor)
    }
}

struct ZedContentVisitor;

impl<'de> Visitor<'de> for ZedContentVisitor {
    type Value = ZedContentWire;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Zed externally tagged content value")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let kind = map
            .next_key::<String>()?
            .ok_or_else(|| serde::de::Error::custom("Zed content tag is empty"))?;
        let content = match kind.as_str() {
            "Text" => ZedContentWire::Text(map.next_value()?),
            "Thinking" => {
                let value: ZedThinkingWire = map.next_value()?;
                ZedContentWire::Thinking(value.text.unwrap_or_default())
            }
            "RedactedThinking" => {
                map.next_value::<IgnoredAny>()?;
                ZedContentWire::RedactedThinking
            }
            "ToolUse" => ZedContentWire::ToolUse(map.next_value()?),
            "ToolResult" => ZedContentWire::ToolResult(map.next_value()?),
            "Mention" => {
                let value: ZedMentionWire = map.next_value()?;
                ZedContentWire::Mention(value.content)
            }
            "Image" => {
                map.next_value::<IgnoredAny>()?;
                ZedContentWire::Image
            }
            _ => {
                map.next_value::<IgnoredAny>()?;
                ZedContentWire::Unknown(kind)
            }
        };
        if map.next_key::<IgnoredAny>()?.is_some() {
            return Err(serde::de::Error::custom(
                "Zed content must contain exactly one external tag",
            ));
        }
        Ok(content)
    }
}

#[derive(Deserialize)]
struct ZedThinkingWire {
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize)]
struct ZedMentionWire {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Deserialize)]
struct ZedToolUseWire {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    input: Option<Value>,
    #[serde(default)]
    raw_input: Option<String>,
}

#[derive(Clone, Copy, Debug, Default)]
struct DiscardedJson {
    string_bytes: u64,
}

impl<'de> Deserialize<'de> for DiscardedJson {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(DiscardedJsonVisitor)
    }
}

struct DiscardedJsonVisitor;

impl<'de> Visitor<'de> for DiscardedJsonVisitor {
    type Value = DiscardedJson;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("any JSON value to discard")
    }

    fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E> {
        Ok(DiscardedJson::default())
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
        Ok(DiscardedJson::default())
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E> {
        Ok(DiscardedJson::default())
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
        Ok(DiscardedJson::default())
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(DiscardedJson::default())
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(DiscardedJson::default())
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> std::result::Result<Self::Value, E> {
        Ok(DiscardedJson {
            string_bytes: u64::try_from(value.len()).unwrap_or(u64::MAX),
        })
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E> {
        Ok(DiscardedJson {
            string_bytes: u64::try_from(value.len()).unwrap_or(u64::MAX),
        })
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E> {
        Ok(DiscardedJson {
            string_bytes: u64::try_from(value.len()).unwrap_or(u64::MAX),
        })
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut string_bytes = 0_u64;
        while let Some(value) = sequence.next_element::<DiscardedJson>()? {
            string_bytes = string_bytes.saturating_add(value.string_bytes);
        }
        Ok(DiscardedJson { string_bytes })
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut string_bytes = 0_u64;
        while let Some((_key, value)) = map.next_entry::<IgnoredAny, DiscardedJson>()? {
            string_bytes = string_bytes.saturating_add(value.string_bytes);
        }
        Ok(DiscardedJson { string_bytes })
    }
}
