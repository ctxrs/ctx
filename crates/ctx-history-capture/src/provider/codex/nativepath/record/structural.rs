use super::*;

mod visitor;

pub(super) use visitor::probe_structural_output;
use visitor::{
    decoded_json_key_cmp, decoded_json_key_is_before_or_same, status_is_failure, status_is_success,
};

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
