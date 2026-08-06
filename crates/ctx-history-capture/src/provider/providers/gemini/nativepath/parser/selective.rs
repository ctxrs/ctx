use super::*;

mod decoding;
mod output;

use decoding::parse_timestamp;
pub(super) use decoding::{
    decode_result_record, decode_retained_event, nonempty, retained_event_bytes,
    GeminiDecodingError,
};
use output::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GeminiRecordClass {
    Header,
    Message,
    ToolCall,
    Result,
    StateNotice,
    RewindNotice,
    Ignored,
}

#[derive(Debug, Default)]
struct Presence(bool);

impl<'de> Deserialize<'de> for Presence {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        IgnoredAny::deserialize(deserializer)?;
        Ok(Self(true))
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct GeminiRecordProbe {
    pub(super) id: Option<String>,
    session_id: Option<String>,
    #[serde(rename = "type")]
    record_type: Option<String>,
    #[serde(default)]
    tool_calls: Option<GeminiToolCallSummary>,
    #[serde(rename = "$set", default)]
    set: Presence,
    #[serde(rename = "$rewindTo", default)]
    rewind_to: Presence,
    #[serde(default)]
    result: Presence,
}

#[derive(Debug, Default)]
struct GeminiToolCallProbe {
    result: Presence,
}

impl<'de> Deserialize<'de> for GeminiToolCallProbe {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ToolCallProbeVisitor;

        impl<'de> Visitor<'de> for ToolCallProbeVisitor {
            type Value = GeminiToolCallProbe;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("one tolerant Gemini tool call")
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut probe = GeminiToolCallProbe::default();
                while let Some(key) = map.next_key::<String>()? {
                    if key == "result" {
                        probe.result = map.next_value()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(probe)
            }

            fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
                Ok(GeminiToolCallProbe::default())
            }

            fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
                Ok(GeminiToolCallProbe::default())
            }

            fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E> {
                Ok(GeminiToolCallProbe::default())
            }

            fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
                Ok(GeminiToolCallProbe::default())
            }

            fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E> {
                Ok(GeminiToolCallProbe::default())
            }

            fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
                Ok(GeminiToolCallProbe::default())
            }

            fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E> {
                Ok(GeminiToolCallProbe::default())
            }

            fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                while sequence.next_element::<IgnoredAny>()?.is_some() {}
                Ok(GeminiToolCallProbe::default())
            }
        }

        deserializer.deserialize_any(ToolCallProbeVisitor)
    }
}

#[derive(Debug, Default)]
struct GeminiToolCallSummary {
    has_calls: bool,
    has_result: bool,
}

impl<'de> Deserialize<'de> for GeminiToolCallSummary {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct SummaryVisitor;

        impl<'de> Visitor<'de> for SummaryVisitor {
            type Value = GeminiToolCallSummary;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a Gemini toolCalls array")
            }

            fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut summary = GeminiToolCallSummary::default();
                while let Some(call) = sequence.next_element::<GeminiToolCallProbe>()? {
                    summary.has_calls = true;
                    summary.has_result |= call.result.0;
                }
                Ok(summary)
            }
        }

        deserializer.deserialize_seq(SummaryVisitor)
    }
}

impl GeminiRecordProbe {
    pub(super) fn classify(&self) -> GeminiRecordClass {
        if self
            .session_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        {
            return GeminiRecordClass::Header;
        }
        let has_calls = self
            .tool_calls
            .as_ref()
            .is_some_and(|calls| calls.has_calls);
        let has_result = self.result.0
            || self
                .tool_calls
                .as_ref()
                .is_some_and(|calls| calls.has_result);
        if has_result {
            GeminiRecordClass::Result
        } else if has_calls {
            GeminiRecordClass::ToolCall
        } else if self.set.0 {
            GeminiRecordClass::StateNotice
        } else if self.rewind_to.0 {
            GeminiRecordClass::RewindNotice
        } else if matches!(self.record_type.as_deref(), Some("user" | "gemini")) {
            GeminiRecordClass::Message
        } else {
            GeminiRecordClass::Ignored
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiHeaderDto {
    session_id: String,
    start_time: Option<String>,
    kind: Option<String>,
    #[serde(default)]
    directories: Vec<String>,
}

pub(super) fn decode_header(
    payload: &[u8],
    layout: &GeminiTranscriptLayout,
) -> std::result::Result<GeminiSession, String> {
    let header: GeminiHeaderDto = serde_json::from_slice(payload)
        .map_err(|error| format!("invalid Gemini header: {error}"))?;
    let native_session_id = header.session_id.trim();
    if native_session_id.is_empty() {
        return Err("Gemini header has an empty sessionId".to_owned());
    }
    let (parent_native_session_id, path_agent_type) = match layout {
        GeminiTranscriptLayout::Primary => (None, AgentType::Primary),
        GeminiTranscriptLayout::Subagent {
            parent_native_session_id_hint,
        } => (
            Some(parent_native_session_id_hint.clone()),
            AgentType::Subagent,
        ),
    };
    let agent_type =
        if parent_native_session_id.is_some() || header.kind.as_deref() == Some("subagent") {
            AgentType::Subagent
        } else {
            path_agent_type
        };
    let mut directories = header
        .directories
        .into_iter()
        .filter(|directory| !directory.trim().is_empty())
        .collect::<Vec<_>>();
    directories.dedup();
    let (cwd, cwd_ambiguous) = match directories.as_slice() {
        [] => (None, false),
        [directory] => (Some(directory.clone()), false),
        _ => (None, true),
    };
    Ok(GeminiSession {
        native_session_id: native_session_id.to_owned(),
        parent_native_session_id,
        agent_type,
        started_at: header.start_time.as_deref().and_then(parse_timestamp),
        cwd,
        cwd_ambiguous,
        native_kind: header.kind,
    })
}

#[derive(Debug, Deserialize)]
struct GeminiMessageDto {
    id: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "type")]
    record_type: Option<String>,
    content: Option<String>,
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiToolCallRecordDto {
    id: Option<String>,
    timestamp: Option<String>,
    content: Option<Value>,
    #[serde(default)]
    tool_calls: Vec<GeminiToolCallDto>,
}

#[derive(Debug, Deserialize)]
struct GeminiToolCallDto {
    id: Option<String>,
    name: Option<String>,
    args: Option<Value>,
    #[serde(default)]
    result: Presence,
}

#[derive(Debug, Default, Clone)]
struct GeminiOutputOutcomeDto {
    error: FailureMarker,
    success: BoolMarker,
    ok: BoolMarker,
    status: StatusMarker,
    state: StatusMarker,
    outcome: StatusMarker,
    is_error: BoolMarker,
    timed_out: BoolMarker,
    timeout: BoolMarker,
    exit_code: I64Marker,
    status_code: I64Marker,
    duration_ms: U64Marker,
    redacted: RedactionMarker,
    is_redacted: RedactionMarker,
    framing_unknown: bool,
    diagnostic_member: bool,
}

impl GeminiOutputOutcomeDto {
    fn merge_nested(&mut self, other: Self) {
        self.error.0 |= other.error.0;
        self.success.0 = self.success.0.or(other.success.0);
        self.ok.0 = self.ok.0.or(other.ok.0);
        self.status.merge_nested(other.status);
        self.state.merge_nested(other.state);
        self.outcome.merge_nested(other.outcome);
        self.is_error.0 = self.is_error.0.or(other.is_error.0);
        self.timed_out.0 = self.timed_out.0.or(other.timed_out.0);
        self.timeout.0 = self.timeout.0.or(other.timeout.0);
        self.exit_code.0 = self.exit_code.0.or(other.exit_code.0);
        self.status_code.0 = self.status_code.0.or(other.status_code.0);
        self.duration_ms.0 = self.duration_ms.0.or(other.duration_ms.0);
        self.framing_unknown |= other.framing_unknown;
        self.diagnostic_member |= other.diagnostic_member;
    }

    fn combined_metadata(&self, inner: &Self) -> OutputOutcomeMetadata {
        let timeout = self.timed_out.0 == Some(true)
            || self.timeout.0 == Some(true)
            || inner.timed_out.0 == Some(true)
            || inner.timeout.0 == Some(true);
        let failure = self.error.0
            || self.success.0 == Some(false)
            || self.is_error.0 == Some(true)
            || self.exit_code.0.is_some_and(|code| code != 0)
            || self.status_code.0.is_some_and(|code| code >= 400)
            || self.status.failure
            || self.state.failure
            || self.outcome.failure
            || inner.error.0
            || inner.success.0 == Some(false)
            || inner.is_error.0 == Some(true)
            || inner.exit_code.0.is_some_and(|code| code != 0)
            || inner.status_code.0.is_some_and(|code| code >= 400)
            || inner.status.failure
            || inner.state.failure
            || inner.outcome.failure;
        let success = self.success.0 == Some(true)
            || self.ok.0 == Some(true)
            || self.is_error.0 == Some(false)
            || self.timed_out.0 == Some(false)
            || self.timeout.0 == Some(false)
            || self.exit_code.0 == Some(0)
            || self
                .status_code
                .0
                .is_some_and(|code| (200..400).contains(&code))
            || self.status.success
            || self.state.success
            || self.outcome.success
            || inner.success.0 == Some(true)
            || inner.ok.0 == Some(true)
            || inner.is_error.0 == Some(false)
            || inner.timed_out.0 == Some(false)
            || inner.timeout.0 == Some(false)
            || inner.exit_code.0 == Some(0)
            || inner
                .status_code
                .0
                .is_some_and(|code| (200..400).contains(&code))
            || inner.status.success
            || inner.state.success
            || inner.outcome.success;
        OutputOutcomeMetadata {
            outcome: if timeout {
                OutputOutcome::Timeout
            } else if failure {
                OutputOutcome::Failure
            } else if success {
                OutputOutcome::Success
            } else {
                OutputOutcome::Unknown
            },
            exit_code: inner
                .exit_code
                .0
                .or(self.exit_code.0)
                .and_then(|code| i32::try_from(code).ok()),
            duration_ms: inner.duration_ms.0.or(self.duration_ms.0),
        }
    }

    fn redacted_with(&self, inner: &Self) -> bool {
        self.is_redacted() || inner.is_redacted()
    }

    fn is_redacted(&self) -> bool {
        self.redacted.0 || self.is_redacted.0 || self.status.redacted || self.state.redacted
    }

    fn terminal_status_with(&self, inner: &Self) -> ResultTerminalStatus {
        if self.framing_unknown || inner.framing_unknown {
            return ResultTerminalStatus::Unknown;
        }
        let failure = self.error.0
            || self.success.0 == Some(false)
            || self.ok.0 == Some(false)
            || self.is_error.0 == Some(true)
            || self.timed_out.0 == Some(true)
            || self.timeout.0 == Some(true)
            || self.exit_code.0.is_some_and(|code| code != 0)
            || self.status_code.0.is_some_and(|code| code >= 400)
            || self.status.failure
            || self.state.failure
            || self.outcome.failure
            || inner.error.0
            || inner.success.0 == Some(false)
            || inner.ok.0 == Some(false)
            || inner.is_error.0 == Some(true)
            || inner.timed_out.0 == Some(true)
            || inner.timeout.0 == Some(true)
            || inner.exit_code.0.is_some_and(|code| code != 0)
            || inner.status_code.0.is_some_and(|code| code >= 400)
            || inner.status.failure
            || inner.state.failure
            || inner.outcome.failure;
        if failure {
            return ResultTerminalStatus::Failed;
        }
        let nonterminal_or_unknown_status = [
            self.status,
            self.state,
            self.outcome,
            inner.status,
            inner.state,
            inner.outcome,
        ]
        .into_iter()
        .any(|status| status.present && !status.success && !status.failure);
        if nonterminal_or_unknown_status {
            return ResultTerminalStatus::Unknown;
        }
        let success = self.success.0 == Some(true)
            || self.ok.0 == Some(true)
            || self.exit_code.0 == Some(0)
            || self
                .status_code
                .0
                .is_some_and(|code| (200..400).contains(&code))
            || self.status.success
            || self.state.success
            || self.outcome.success
            || inner.success.0 == Some(true)
            || inner.ok.0 == Some(true)
            || inner.exit_code.0 == Some(0)
            || inner
                .status_code
                .0
                .is_some_and(|code| (200..400).contains(&code))
            || inner.status.success
            || inner.state.success
            || inner.outcome.success;
        if success {
            ResultTerminalStatus::Succeeded
        } else {
            ResultTerminalStatus::Unknown
        }
    }
}

#[derive(Debug, Default, Clone)]
struct FailureMarker(bool);

#[derive(Debug, Default, Clone, Copy)]
struct BoolMarker(Option<bool>);

#[derive(Debug, Default, Clone, Copy)]
struct RedactionMarker(bool);

#[derive(Debug, Default, Clone, Copy)]
struct I64Marker(Option<i64>);

#[derive(Debug, Default, Clone, Copy)]
struct U64Marker(Option<u64>);

#[derive(Debug, Default, Clone, Copy)]
struct StatusMarker {
    success: bool,
    failure: bool,
    redacted: bool,
    present: bool,
}

impl StatusMarker {
    fn merge_nested(&mut self, other: Self) {
        self.success |= other.success;
        self.failure |= other.failure;
        self.present |= other.present;
    }
}

#[derive(Default)]
enum GeminiSelectedContent {
    #[default]
    Absent,
    String {
        value: String,
        sha256: [u8; 32],
    },
    Null,
    Structured {
        value: Value,
        sha256: [u8; 32],
    },
}

#[derive(Debug, Default)]
pub(super) struct GeminiRepositoryArgs {
    command: Option<String>,
    command_too_large: bool,
    declared_workdir: Option<String>,
    file_paths: Vec<String>,
    ambiguous_native_fields: bool,
}

struct ProbedGeminiOutput {
    result: Option<Value>,
    call_id: Option<String>,
    tool_name: Option<String>,
    command: Option<String>,
    command_too_large: bool,
    declared_workdir: Option<String>,
    file_paths: Vec<String>,
    ambiguous_native_fields: bool,
    outcome: OutputOutcomeMetadata,
    terminal_status: ResultTerminalStatus,
    atoms: Vec<ResultAtom>,
    redacted: bool,
    fallback_identity_sha256: [u8; 32],
}

struct ProbedGeminiResult {
    native_record_id: Option<String>,
    occurred_at_unix_ms: Option<i64>,
    outputs: Vec<ProbedGeminiOutput>,
    aggregate_unknown: bool,
}

pub(super) struct DecodedGeminiResult {
    pub(super) events: Vec<(GeminiRetainedEvent, usize)>,
}

const MAX_GEMINI_STRUCTURAL_DEPTH: usize = 128;
const MAX_GEMINI_STRUCTURAL_KEY_CHARS: usize = 64;
const MAX_GEMINI_REPOSITORY_STRING_CHARS: usize = 64 * 1024;
const MAX_GEMINI_REPOSITORY_PATHS: usize = 256;

struct GeminiRawJson<'a> {
    bytes: &'a [u8],
    offset: usize,
}

struct GeminiRawString {
    retained: String,
    truncated: bool,
    non_whitespace: bool,
    sha256: [u8; 32],
}

impl GeminiRawString {
    fn exact(self) -> Option<String> {
        (!self.truncated).then_some(self.retained)
    }

    fn bounded_command(self) -> (Option<String>, bool) {
        let too_large = self.truncated
            || self.retained.len() > crate::repository_attribution::MAX_COMMAND_BYTES;
        if too_large {
            (None, true)
        } else {
            (Some(self.retained), false)
        }
    }
}

impl<'a> GeminiRawJson<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn finish(mut self) -> std::result::Result<(), String> {
        self.whitespace();
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err("Gemini result record has trailing JSON data".to_owned())
        }
    }

    fn whitespace(&mut self) {
        while self
            .bytes
            .get(self.offset)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.offset = self.offset.saturating_add(1);
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.offset).copied()
    }

    fn take(&mut self, expected: u8) -> std::result::Result<(), String> {
        if self.peek() != Some(expected) {
            return Err(format!(
                "invalid Gemini result JSON near byte {}",
                self.offset
            ));
        }
        self.offset = self.offset.saturating_add(1);
        Ok(())
    }

    fn consume_literal(&mut self, literal: &[u8]) -> std::result::Result<(), String> {
        if self
            .bytes
            .get(self.offset..self.offset.saturating_add(literal.len()))
            != Some(literal)
        {
            return Err(format!(
                "invalid Gemini result JSON literal near byte {}",
                self.offset
            ));
        }
        self.offset = self.offset.saturating_add(literal.len());
        Ok(())
    }

    fn string(&mut self, retain_chars: usize) -> std::result::Result<GeminiRawString, String> {
        self.take(b'"')?;
        let mut retained = String::new();
        let mut retained_chars = 0_usize;
        let mut decoded_chars = 0_usize;
        let mut non_whitespace = false;
        let mut hasher = Sha256::new();
        hasher.update(RESULT_STRING_HASH_DOMAIN);
        loop {
            let byte = self
                .peek()
                .ok_or_else(|| "unterminated string in Gemini result JSON".to_owned())?;
            match byte {
                b'"' => {
                    self.offset = self.offset.saturating_add(1);
                    return Ok(GeminiRawString {
                        retained,
                        truncated: retained_chars < decoded_chars,
                        non_whitespace,
                        sha256: hasher.finalize().into(),
                    });
                }
                b'\\' => {
                    self.offset = self.offset.saturating_add(1);
                    let escaped = self
                        .peek()
                        .ok_or_else(|| "unterminated escape in Gemini result JSON".to_owned())?;
                    self.offset = self.offset.saturating_add(1);
                    let character = match escaped {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{0008}',
                        b'f' => '\u{000c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => self.unicode_escape()?,
                        _ => {
                            return Err(format!(
                                "invalid escape in Gemini result JSON near byte {}",
                                self.offset.saturating_sub(1)
                            ));
                        }
                    };
                    let mut encoded = [0_u8; 4];
                    hasher.update(character.encode_utf8(&mut encoded).as_bytes());
                    decoded_chars = decoded_chars.saturating_add(1);
                    non_whitespace |= !character.is_whitespace();
                    if retained_chars < retain_chars {
                        retained.push(character);
                        retained_chars = retained_chars.saturating_add(1);
                    }
                }
                0x00..=0x1f => {
                    return Err(format!(
                        "control byte in Gemini result JSON string near byte {}",
                        self.offset
                    ));
                }
                byte if byte.is_ascii() => {
                    let start = self.offset;
                    while self.peek().is_some_and(|byte| {
                        byte.is_ascii() && !matches!(byte, b'"' | b'\\' | 0x00..=0x1f)
                    }) {
                        self.offset = self.offset.saturating_add(1);
                    }
                    let run = &self.bytes[start..self.offset];
                    hasher.update(run);
                    decoded_chars = decoded_chars.saturating_add(run.len());
                    non_whitespace |= run.iter().any(|byte| !byte.is_ascii_whitespace());
                    let retained_bytes = retain_chars.saturating_sub(retained_chars).min(run.len());
                    if retained_bytes != 0 {
                        retained
                            .push_str(std::str::from_utf8(&run[..retained_bytes]).map_err(
                                |_| "Gemini result JSON string is not UTF-8".to_owned(),
                            )?);
                        retained_chars = retained_chars.saturating_add(retained_bytes);
                    }
                }
                _ => {
                    let width = match byte {
                        0xc2..=0xdf => 2,
                        0xe0..=0xef => 3,
                        0xf0..=0xf4 => 4,
                        _ => {
                            return Err("Gemini result JSON string is not UTF-8".to_owned());
                        }
                    };
                    let end = self
                        .offset
                        .checked_add(width)
                        .ok_or_else(|| "Gemini result string offset overflowed".to_owned())?;
                    let encoded = self
                        .bytes
                        .get(self.offset..end)
                        .ok_or_else(|| "unterminated UTF-8 in Gemini result JSON".to_owned())?;
                    let character = std::str::from_utf8(encoded)
                        .ok()
                        .and_then(|value| value.chars().next())
                        .ok_or_else(|| "Gemini result JSON string is not UTF-8".to_owned())?;
                    self.offset = end;
                    hasher.update(encoded);
                    decoded_chars = decoded_chars.saturating_add(1);
                    non_whitespace |= !character.is_whitespace();
                    if retained_chars < retain_chars {
                        retained.push(character);
                        retained_chars = retained_chars.saturating_add(1);
                    }
                }
            }
        }
    }

    fn unicode_escape(&mut self) -> std::result::Result<char, String> {
        let first = self.hex_quad()?;
        let code = if (0xd800..=0xdbff).contains(&first) {
            self.take(b'\\')?;
            self.take(b'u')?;
            let second = self.hex_quad()?;
            if !(0xdc00..=0xdfff).contains(&second) {
                return Err("invalid Unicode surrogate pair in Gemini result JSON".to_owned());
            }
            0x1_0000 + (u32::from(first - 0xd800) << 10) + u32::from(second - 0xdc00)
        } else if (0xdc00..=0xdfff).contains(&first) {
            return Err("invalid Unicode surrogate pair in Gemini result JSON".to_owned());
        } else {
            u32::from(first)
        };
        char::from_u32(code)
            .ok_or_else(|| "invalid Unicode scalar in Gemini result JSON".to_owned())
    }

    fn hex_quad(&mut self) -> std::result::Result<u16, String> {
        let mut value = 0_u16;
        for _ in 0..4 {
            let digit = self.peek().and_then(|byte| (byte as char).to_digit(16));
            let Some(digit) = digit else {
                return Err(format!(
                    "invalid Unicode escape in Gemini result JSON near byte {}",
                    self.offset
                ));
            };
            self.offset = self.offset.saturating_add(1);
            value = (value << 4) | u16::try_from(digit).unwrap_or_default();
        }
        Ok(value)
    }

    fn key(&mut self) -> std::result::Result<Option<String>, String> {
        let key = self.string(MAX_GEMINI_STRUCTURAL_KEY_CHARS)?;
        Ok(key.exact())
    }

    fn optional_string(&mut self) -> std::result::Result<Option<String>, String> {
        self.whitespace();
        if self.peek() == Some(b'"') {
            return self
                .string(MAX_GEMINI_REPOSITORY_STRING_CHARS)?
                .exact()
                .ok_or_else(|| "Gemini result metadata string exceeded the bound".to_owned())
                .map(Some);
        }
        self.skip_value(0)?;
        Ok(None)
    }

    fn number(&mut self) -> std::result::Result<&'a str, String> {
        let start = self.offset;
        while self.peek().is_some_and(|byte| {
            byte.is_ascii_digit() || matches!(byte, b'-' | b'+' | b'.' | b'e' | b'E')
        }) {
            self.offset = self.offset.saturating_add(1);
        }
        let value = std::str::from_utf8(&self.bytes[start..self.offset])
            .map_err(|_| "Gemini result number is not UTF-8".to_owned())?;
        if value.is_empty() {
            Err(format!(
                "invalid Gemini result number near byte {}",
                self.offset
            ))
        } else {
            Ok(value)
        }
    }

    fn skip_value(&mut self, depth: usize) -> std::result::Result<(), String> {
        if depth > MAX_GEMINI_STRUCTURAL_DEPTH {
            return Err(format!(
                "Gemini result JSON exceeds structural depth {MAX_GEMINI_STRUCTURAL_DEPTH}"
            ));
        }
        self.whitespace();
        match self.peek() {
            Some(b'"') => {
                self.string(0)?;
            }
            Some(b'{') => {
                self.take(b'{')?;
                self.whitespace();
                if self.peek() == Some(b'}') {
                    self.take(b'}')?;
                    return Ok(());
                }
                loop {
                    self.string(0)?;
                    self.whitespace();
                    self.take(b':')?;
                    self.skip_value(depth.saturating_add(1))?;
                    self.whitespace();
                    match self.peek() {
                        Some(b',') => {
                            self.take(b',')?;
                            self.whitespace();
                        }
                        Some(b'}') => {
                            self.take(b'}')?;
                            break;
                        }
                        _ => {
                            return Err(format!(
                                "invalid Gemini result object near byte {}",
                                self.offset
                            ));
                        }
                    }
                }
            }
            Some(b'[') => {
                self.take(b'[')?;
                self.whitespace();
                if self.peek() == Some(b']') {
                    self.take(b']')?;
                    return Ok(());
                }
                loop {
                    self.skip_value(depth.saturating_add(1))?;
                    self.whitespace();
                    match self.peek() {
                        Some(b',') => {
                            self.take(b',')?;
                            self.whitespace();
                        }
                        Some(b']') => {
                            self.take(b']')?;
                            break;
                        }
                        _ => {
                            return Err(format!(
                                "invalid Gemini result array near byte {}",
                                self.offset
                            ));
                        }
                    }
                }
            }
            Some(b't') => self.consume_literal(b"true")?,
            Some(b'f') => self.consume_literal(b"false")?,
            Some(b'n') => self.consume_literal(b"null")?,
            Some(_) => {
                self.number()?;
            }
            None => return Err("missing value in Gemini result JSON".to_owned()),
        }
        Ok(())
    }
}

#[derive(Default)]
struct GeminiRawOutput {
    outcome: GeminiOutputOutcomeDto,
    content: GeminiSelectedContent,
    known_envelope: bool,
    unknown_envelope_member: bool,
}
