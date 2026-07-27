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

mod wire;

use wire::*;

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
