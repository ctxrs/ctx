use std::ops::Range;

use ctx_history_core::MAX_CORE_CONTENT_BYTES;

use crate::{OutputOutcome, OutputOutcomeMetadata};

use super::rows::CLAUDE_MAX_RECORD_ROWS;

mod outcome;

use outcome::{combine_outcome_evidence, scan_direct_output_range, scan_outcome_range};

const MAX_ESCAPED_LABEL_BYTES: usize = 256;
const MAX_SCAN_DEPTH: usize = 128;
const MAX_METADATA_BYTES: usize = 4 * 1024;
const MAX_RESULT_OUTCOME_NODES: usize = 4_096;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct RawResultClassification {
    pub(super) tagged_command_output: bool,
    pub(super) result_block: bool,
    pub(super) result_like_shape: bool,
    pub(super) top_level_result: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RawRecordPreflight {
    pub(super) result: RawResultClassification,
    pub(super) outcome: OutputOutcomeMetadata,
    output_descriptors: Vec<RawOutputDescriptor>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct RawOutputDescriptor {
    call_id: Option<RawStringRange>,
    value: Option<RawValueRange>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct RawStringRange {
    start: usize,
    end: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct RawValueRange {
    start: usize,
    end: usize,
}

impl RawRecordPreflight {
    pub(super) fn output_descriptors(&self) -> &[RawOutputDescriptor] {
        &self.output_descriptors
    }
}

impl RawOutputDescriptor {
    pub(super) fn decode_call_id(self, bytes: &[u8]) -> Option<String> {
        let range = self.call_id?;
        let raw = bytes.get(range.start..range.end)?;
        let inner = raw.strip_prefix(b"\"")?.strip_suffix(b"\"")?;
        let mut decoded = String::with_capacity(inner.len().min(256));
        visit_decoded_chars(inner, |character| {
            decoded.push(character);
            (decoded.len() <= 256).then_some(()).ok_or(())
        })
        .ok()?;
        Some(decoded)
    }

    pub(super) fn decode_outcome(
        self,
        bytes: &[u8],
    ) -> Result<OutputOutcomeMetadata, serde_json::Error> {
        let evidence =
            scan_direct_output_range(bytes, self.value.map(|range| range.start..range.end))?;
        Ok(combine_outcome_evidence(evidence, Default::default()))
    }

    pub(super) fn decode_value(
        self,
        bytes: &[u8],
    ) -> Result<Option<serde_json::Value>, serde_json::Error> {
        self.value
            .map(|range| serde_json::from_slice(&bytes[range.start..range.end]))
            .transpose()
    }
}

pub(super) fn preflight_record(bytes: &[u8]) -> Result<RawRecordPreflight, serde_json::Error> {
    let mut scanner = ResultScanner::new(bytes);
    scanner
        .scan_record()
        .map_err(|()| structural_error("malformed or structurally unbounded Claude JSON record"))?;
    if scanner.duplicate_critical {
        return Err(structural_error("duplicate critical Claude JSON key"));
    }
    if let Some(reason) = scanner.limit_violation {
        return Err(structural_error(reason));
    }
    let content = if scanner.message_present {
        scanner.message_content
    } else {
        scanner.record_content
    };
    let tool_use_result = scanner
        .record_tool_use_result
        .or(scanner.message_tool_use_result);
    let content_evidence = scan_outcome_range(bytes, content, false)?;
    let tool_evidence = scan_outcome_range(bytes, tool_use_result, true)?;
    let outcome = combine_outcome_evidence(content_evidence, tool_evidence);
    Ok(RawRecordPreflight {
        result: scanner.result,
        outcome,
        output_descriptors: scanner.output_descriptors,
    })
}

fn structural_error(reason: &'static str) -> serde_json::Error {
    serde_json::Error::io(std::io::Error::new(std::io::ErrorKind::InvalidData, reason))
}

pub(super) fn is_native_command_output_tag(value: &str) -> bool {
    value.eq_ignore_ascii_case("bash-stdout")
        || value.eq_ignore_ascii_case("bash-stderr")
        || value.eq_ignore_ascii_case("local-command-stdout")
        || value.eq_ignore_ascii_case("local-command-stderr")
        || value.eq_ignore_ascii_case("bash-exit-code")
        || value.eq_ignore_ascii_case("local-command-caveat")
}

pub(super) fn is_result_label(value: &str) -> bool {
    is_result_label_bytes(value.as_bytes())
}

fn is_result_shape_label_bytes(value: &[u8]) -> bool {
    [
        b"tool_use_id".as_slice(),
        b"toolUseId",
        b"is_error",
        b"isError",
    ]
    .into_iter()
    .any(|expected| value.eq_ignore_ascii_case(expected))
        || is_result_label_bytes(value)
}

fn is_result_label_bytes(value: &[u8]) -> bool {
    [
        b"tooluseresult".as_slice(),
        b"tool_use_result",
        b"tool-result",
        b"toolresult",
        b"stdout",
        b"stderr",
        b"exitcode",
        b"exit_code",
    ]
    .into_iter()
    .any(|expected| value.eq_ignore_ascii_case(expected))
        || [b"result".as_slice(), b"results", b"output", b"outputs"]
            .into_iter()
            .any(|suffix| {
                value
                    .get(value.len().saturating_sub(suffix.len())..)
                    .is_some_and(|tail| tail.eq_ignore_ascii_case(suffix))
            })
        || value
            .split(|byte| !byte.is_ascii_alphanumeric())
            .any(|token| {
                [
                    b"result".as_slice(),
                    b"results",
                    b"output",
                    b"outputs",
                    b"stdout",
                    b"stderr",
                ]
                .into_iter()
                .any(|expected| token.eq_ignore_ascii_case(expected))
            })
}

#[derive(Clone, Copy)]
enum ObjectKind {
    Record,
    Message,
    Block,
    Ignored,
}

#[derive(Clone, Copy)]
enum Field {
    Type,
    Message,
    Content,
    BlockText,
    Summary,
    ToolUseResult,
    Metadata,
    ResultLike,
    Other,
}

struct ResultScanner<'a> {
    bytes: &'a [u8],
    index: usize,
    result: RawResultClassification,
    duplicate_critical: bool,
    limit_violation: Option<&'static str>,
    message_present: bool,
    record_content: Option<Range<usize>>,
    message_content: Option<Range<usize>>,
    record_tool_use_result: Option<Range<usize>>,
    message_tool_use_result: Option<Range<usize>>,
    output_descriptors: Vec<RawOutputDescriptor>,
}

impl<'a> ResultScanner<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            index: 0,
            result: RawResultClassification::default(),
            duplicate_critical: false,
            limit_violation: None,
            message_present: false,
            record_content: None,
            message_content: None,
            record_tool_use_result: None,
            message_tool_use_result: None,
            output_descriptors: Vec::new(),
        }
    }

    fn scan_record(&mut self) -> Result<(), ()> {
        self.whitespace();
        self.object(ObjectKind::Record, 0)?;
        self.whitespace();
        (self.index == self.bytes.len()).then_some(()).ok_or(())
    }

    fn object(&mut self, kind: ObjectKind, depth: usize) -> Result<(), ()> {
        self.check_depth(depth)?;
        let object_start = self.index;
        self.expect(b'{')?;
        self.whitespace();
        if self.consume(b'}') {
            return Ok(());
        }
        let mut seen_critical = 0_u16;
        let mut block_primary_output = false;
        let mut block_call_id = None;
        let mut block_result_values = Vec::new();
        loop {
            let key = self.string_range()?;
            self.whitespace();
            self.expect(b':')?;
            self.whitespace();
            let field = classify_field(&self.bytes[key.clone()], kind);
            if let Some(bit) = critical_field_bit(&self.bytes[key.clone()]) {
                if seen_critical & bit != 0 {
                    self.duplicate_critical = true;
                }
                seen_critical |= bit;
            }
            let value_start = self.index;
            match (kind, field) {
                (_, Field::ResultLike) => {
                    self.result.result_like_shape = true;
                    if matches!(kind, ObjectKind::Block) {
                        if raw_label_is_result(&self.bytes[key.clone()]) {
                            // The exact value span is captured after the value
                            // has been structurally skipped.
                        } else {
                            block_primary_output = true;
                        }
                    }
                    self.skip_value(depth + 1)?;
                }
                (_, Field::Type) => {
                    block_primary_output |= self.type_value(kind, depth + 1)?;
                }
                (ObjectKind::Record, Field::Message) => {
                    self.message_present = true;
                    if self.peek() == Some(b'{') {
                        self.object(ObjectKind::Message, depth + 1)?;
                    } else if self.peek() == Some(b'n') {
                        self.skip_value(depth + 1)?;
                    } else {
                        self.limit_violation =
                            Some("Claude message metadata must be an object or null");
                        self.skip_value(depth + 1)?;
                    }
                }
                (ObjectKind::Record, Field::Content)
                | (ObjectKind::Message, Field::Content)
                | (ObjectKind::Block, Field::Content) => self.content(depth + 1)?,
                (ObjectKind::Record, Field::Summary) => self.summary_value(depth + 1)?,
                (ObjectKind::Block, Field::BlockText) => {
                    self.block_text_value(depth + 1)?;
                }
                (ObjectKind::Record, Field::ToolUseResult)
                | (ObjectKind::Message, Field::ToolUseResult) => {
                    self.result.result_like_shape = true;
                    self.skip_value(depth + 1)?;
                }
                (ObjectKind::Record, Field::Metadata)
                | (ObjectKind::Message, Field::Metadata)
                | (ObjectKind::Block, Field::Metadata) => {
                    let is_call_id = raw_label_eq(&self.bytes[key.clone()], b"tool_use_id")
                        || raw_label_eq(&self.bytes[key.clone()], b"toolUseId");
                    self.result.result_like_shape |= is_call_id;
                    self.metadata_value(
                        depth + 1,
                        if is_call_id { 256 } else { MAX_METADATA_BYTES },
                    )?;
                    if matches!(kind, ObjectKind::Block) && is_call_id {
                        block_primary_output = true;
                    }
                }
                _ => self.skip_value(depth + 1)?,
            }
            let value_range = value_start..self.index;
            match (kind, field) {
                (ObjectKind::Record, Field::Content) => {
                    self.record_content = Some(value_range.clone());
                }
                (ObjectKind::Message, Field::Content) => {
                    self.message_content = Some(value_range.clone());
                }
                (ObjectKind::Record, Field::ToolUseResult) => {
                    self.record_tool_use_result = Some(value_range.clone());
                }
                (ObjectKind::Message, Field::ToolUseResult) => {
                    self.message_tool_use_result = Some(value_range.clone());
                }
                (ObjectKind::Block, Field::ResultLike)
                    if raw_label_is_result(&self.bytes[key.clone()]) =>
                {
                    if block_result_values.len() >= CLAUDE_MAX_RECORD_ROWS {
                        self.limit_violation =
                            Some("Claude result exceeds the representable row limit");
                    } else {
                        block_result_values.push(RawValueRange {
                            start: value_range.start,
                            end: value_range.end,
                        });
                    }
                }
                _ => {}
            }
            if matches!(kind, ObjectKind::Block)
                && matches!(field, Field::Metadata)
                && (raw_label_eq(&self.bytes[key.clone()], b"tool_use_id")
                    || raw_label_eq(&self.bytes[key.clone()], b"toolUseId"))
                && block_call_id.is_none()
            {
                block_call_id = Some(RawStringRange {
                    start: value_range.start,
                    end: value_range.end,
                });
            }
            self.whitespace();
            if self.consume(b'}') {
                if matches!(kind, ObjectKind::Block) {
                    if block_primary_output {
                        self.push_output_descriptor(
                            block_call_id,
                            Some(RawValueRange {
                                start: object_start,
                                end: self.index,
                            }),
                        );
                    }
                    for value in block_result_values.iter().copied() {
                        self.push_output_descriptor(None, Some(value));
                    }
                }
                return Ok(());
            }
            self.expect(b',')?;
            self.whitespace();
        }
    }

    fn type_value(&mut self, kind: ObjectKind, depth: usize) -> Result<bool, ()> {
        if self.peek() == Some(b'n') {
            self.skip_value(depth)?;
            return Ok(false);
        }
        if self.peek() != Some(b'"') {
            self.limit_violation = Some("Claude type metadata must be a string or null");
            self.skip_value(depth)?;
            return Ok(false);
        }
        let value = self.string_range()?;
        if value.len() > MAX_METADATA_BYTES {
            self.limit_violation = Some("Claude type metadata exceeds 4 KiB");
        }
        let is_result = raw_label_is_result(&self.bytes[value]);
        if is_result {
            self.result.result_block = true;
            self.result.top_level_result |= matches!(kind, ObjectKind::Record);
        }
        Ok(is_result && matches!(kind, ObjectKind::Block))
    }

    fn push_output_descriptor(
        &mut self,
        call_id: Option<RawStringRange>,
        value: Option<RawValueRange>,
    ) {
        if self.output_descriptors.len() >= CLAUDE_MAX_RECORD_ROWS {
            self.limit_violation = Some("Claude result exceeds the representable row limit");
            return;
        }
        self.output_descriptors
            .push(RawOutputDescriptor { call_id, value });
    }

    fn metadata_value(&mut self, depth: usize, max_bytes: usize) -> Result<(), ()> {
        if self.peek() == Some(b'n') {
            return self.skip_value(depth);
        }
        if self.peek() != Some(b'"') {
            self.limit_violation = Some("Claude metadata must be a string or null");
            return self.skip_value(depth);
        }
        let value = self.string_range()?;
        if value.len() > max_bytes {
            self.limit_violation = Some("Claude metadata exceeds its structural limit");
        }
        Ok(())
    }

    fn summary_value(&mut self, depth: usize) -> Result<(), ()> {
        if self.peek() == Some(b'n') {
            return self.skip_value(depth);
        }
        if self.peek() != Some(b'"') {
            self.limit_violation = Some("Claude summary must be a string or null");
            return self.skip_value(depth);
        }
        let value = self.string_range()?;
        if value.len() > MAX_CORE_CONTENT_BYTES {
            self.limit_violation = Some("Claude summary exceeds the Core content limit");
        }
        scan_tagged_string(&self.bytes[value], &mut self.result);
        Ok(())
    }

    fn block_text_value(&mut self, depth: usize) -> Result<(), ()> {
        if self.peek() == Some(b'n') {
            return self.skip_value(depth);
        }
        if self.peek() != Some(b'"') {
            self.limit_violation = Some("Claude text block body must be a string or null");
            return self.skip_value(depth);
        }
        let value = self.string_range()?;
        if value.len() > MAX_CORE_CONTENT_BYTES {
            self.limit_violation = Some("Claude text block body exceeds the Core content limit");
        }
        scan_tagged_string(&self.bytes[value], &mut self.result);
        Ok(())
    }

    fn content(&mut self, depth: usize) -> Result<(), ()> {
        self.check_depth(depth)?;
        match self.peek().ok_or(())? {
            b'"' => {
                let value = self.string_range()?;
                scan_tagged_string(&self.bytes[value], &mut self.result);
                Ok(())
            }
            b'{' => self.object(ObjectKind::Block, depth),
            b'[' => {
                self.index += 1;
                self.whitespace();
                if self.consume(b']') {
                    return Ok(());
                }
                loop {
                    self.content(depth + 1)?;
                    self.whitespace();
                    if self.consume(b']') {
                        return Ok(());
                    }
                    self.expect(b',')?;
                    self.whitespace();
                }
            }
            _ => self.skip_value(depth),
        }
    }

    fn skip_value(&mut self, depth: usize) -> Result<(), ()> {
        self.check_depth(depth)?;
        match self.peek().ok_or(())? {
            b'"' => {
                self.string_range()?;
                Ok(())
            }
            b'{' => self.object(ObjectKind::Ignored, depth),
            b'[' => {
                self.index += 1;
                self.whitespace();
                if self.consume(b']') {
                    return Ok(());
                }
                loop {
                    self.skip_value(depth + 1)?;
                    self.whitespace();
                    if self.consume(b']') {
                        return Ok(());
                    }
                    self.expect(b',')?;
                    self.whitespace();
                }
            }
            _ => {
                let start = self.index;
                while self
                    .peek()
                    .is_some_and(|byte| !byte.is_ascii_whitespace() && !b",]}".contains(&byte))
                {
                    self.index += 1;
                }
                (self.index > start).then_some(()).ok_or(())
            }
        }
    }

    fn string_range(&mut self) -> Result<std::ops::Range<usize>, ()> {
        self.expect(b'"')?;
        let start = self.index;
        loop {
            match self.peek().ok_or(())? {
                b'"' => {
                    let end = self.index;
                    self.index += 1;
                    return Ok(start..end);
                }
                b'\\' => {
                    self.index += 1;
                    let escape = self.peek().ok_or(())?;
                    self.index += 1;
                    if escape == b'u' {
                        for _ in 0..4 {
                            if !self.peek().is_some_and(|byte| byte.is_ascii_hexdigit()) {
                                return Err(());
                            }
                            self.index += 1;
                        }
                    } else if !b"\"\\/bfnrt".contains(&escape) {
                        return Err(());
                    }
                }
                0x00..=0x1f => return Err(()),
                _ => self.index += 1,
            }
        }
    }

    fn check_depth(&self, depth: usize) -> Result<(), ()> {
        (depth <= MAX_SCAN_DEPTH).then_some(()).ok_or(())
    }

    fn whitespace(&mut self) {
        while self.peek().is_some_and(|byte| byte.is_ascii_whitespace()) {
            self.index += 1;
        }
    }

    fn expect(&mut self, expected: u8) -> Result<(), ()> {
        self.consume(expected).then_some(()).ok_or(())
    }

    fn consume(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.index).copied()
    }
}

fn critical_field_bit(raw: &[u8]) -> Option<u16> {
    [
        b"type".as_slice(),
        b"message",
        b"content",
        b"text",
        b"summary",
        b"sessionId",
        b"uuid",
        b"parentUuid",
        b"toolUseResult",
        b"tool_use_id",
        b"toolUseId",
        b"is_error",
        b"isError",
    ]
    .into_iter()
    .position(|expected| raw_label_eq(raw, expected))
    .map(|index| 1_u16 << index)
}

fn classify_field(raw: &[u8], kind: ObjectKind) -> Field {
    if matches!(kind, ObjectKind::Ignored) {
        Field::Other
    } else if raw_label_eq(raw, b"type") {
        Field::Type
    } else if matches!(kind, ObjectKind::Record | ObjectKind::Message)
        && raw_label_eq(raw, b"toolUseResult")
    {
        Field::ToolUseResult
    } else if is_metadata_field(raw, kind) {
        Field::Metadata
    } else if matches!(kind, ObjectKind::Block) && raw_label_eq(raw, b"text") {
        Field::BlockText
    } else if raw_label_is_result_shape(raw) {
        Field::ResultLike
    } else if matches!(kind, ObjectKind::Record) && raw_label_eq(raw, b"message") {
        Field::Message
    } else if raw_label_eq(raw, b"content") {
        Field::Content
    } else if matches!(kind, ObjectKind::Record) && raw_label_eq(raw, b"summary") {
        Field::Summary
    } else {
        Field::Other
    }
}

fn is_metadata_field(raw: &[u8], kind: ObjectKind) -> bool {
    match kind {
        ObjectKind::Record => [
            b"sessionId".as_slice(),
            b"uuid",
            b"parentUuid",
            b"timestamp",
            b"cwd",
            b"version",
            b"gitBranch",
            b"role",
        ]
        .into_iter()
        .any(|expected| raw_label_eq(raw, expected)),
        ObjectKind::Message => [b"id".as_slice(), b"role"]
            .into_iter()
            .any(|expected| raw_label_eq(raw, expected)),
        ObjectKind::Block => [b"id".as_slice(), b"name", b"tool_use_id", b"toolUseId"]
            .into_iter()
            .any(|expected| raw_label_eq(raw, expected)),
        ObjectKind::Ignored => false,
    }
}

fn raw_label_is_result_shape(raw: &[u8]) -> bool {
    let mut decoded = [0_u8; MAX_ESCAPED_LABEL_BYTES];
    match decode_label(raw, &mut decoded) {
        Ok(value) => is_result_shape_label_bytes(value),
        Err(()) => true,
    }
}

fn raw_label_is_result(raw: &[u8]) -> bool {
    let mut decoded = [0_u8; MAX_ESCAPED_LABEL_BYTES];
    match decode_label(raw, &mut decoded) {
        Ok(value) => is_result_label_bytes(value),
        Err(()) => true,
    }
}

fn raw_label_eq(raw: &[u8], expected: &[u8]) -> bool {
    let mut decoded = [0_u8; MAX_ESCAPED_LABEL_BYTES];
    decode_label(raw, &mut decoded).is_ok_and(|value| value.eq_ignore_ascii_case(expected))
}

fn decode_label<'a>(
    raw: &'a [u8],
    decoded: &'a mut [u8; MAX_ESCAPED_LABEL_BYTES],
) -> Result<&'a [u8], ()> {
    if !raw.contains(&b'\\') {
        return Ok(raw);
    }
    let mut length = 0;
    visit_decoded_ascii(raw, |byte| {
        let byte = byte.ok_or(())?;
        let target = decoded.get_mut(length).ok_or(())?;
        *target = byte;
        length += 1;
        Ok(())
    })?;
    Ok(&decoded[..length])
}

fn scan_tagged_string(raw: &[u8], result: &mut RawResultClassification) {
    let mut scanner = TagScanner::default();
    let _ = visit_decoded_ascii(raw, |byte| {
        scanner.feed(byte, result);
        Ok(())
    });
    scanner.finish(result);
}

fn visit_decoded_ascii(
    raw: &[u8],
    mut visit: impl FnMut(Option<u8>) -> Result<(), ()>,
) -> Result<(), ()> {
    visit_decoded_chars(raw, |character| {
        visit(
            character
                .is_ascii()
                .then(|| u8::try_from(u32::from(character)).unwrap_or_default()),
        )
    })
}

fn visit_decoded_chars(
    raw: &[u8],
    mut visit: impl FnMut(char) -> Result<(), ()>,
) -> Result<(), ()> {
    let mut index = 0;
    while let Some(&byte) = raw.get(index) {
        if byte != b'\\' {
            let plain_length = raw[index..]
                .iter()
                .position(|byte| *byte == b'\\')
                .unwrap_or(raw.len() - index);
            let plain = std::str::from_utf8(&raw[index..index + plain_length]).map_err(|_| ())?;
            for character in plain.chars() {
                visit(character)?;
            }
            index += plain_length;
            continue;
        }
        let escape = *raw.get(index + 1).ok_or(())?;
        index += 2;
        let decoded = match escape {
            b'"' | b'\\' | b'/' => char::from(escape),
            b'b' => '\u{0008}',
            b'f' => '\u{000c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => {
                let first = decode_hex_quad(raw.get(index..index + 4).ok_or(())?)?;
                index += 4;
                let scalar = if (0xd800..=0xdbff).contains(&first) {
                    if raw.get(index..index + 2) != Some(b"\\u") {
                        return Err(());
                    }
                    index += 2;
                    let second = decode_hex_quad(raw.get(index..index + 4).ok_or(())?)?;
                    index += 4;
                    if !(0xdc00..=0xdfff).contains(&second) {
                        return Err(());
                    }
                    0x1_0000 + ((u32::from(first) - 0xd800) << 10) + (u32::from(second) - 0xdc00)
                } else if (0xdc00..=0xdfff).contains(&first) {
                    return Err(());
                } else {
                    u32::from(first)
                };
                char::from_u32(scalar).ok_or(())?
            }
            _ => return Err(()),
        };
        visit(decoded)?;
    }
    Ok(())
}

fn decode_hex_quad(digits: &[u8]) -> Result<u16, ()> {
    digits.iter().try_fold(0_u16, |value, digit| {
        char::from(*digit)
            .to_digit(16)
            .and_then(|digit| u16::try_from(digit).ok())
            .map(|digit| (value << 4) | digit)
            .ok_or(())
    })
}

struct TagScanner {
    name: [u8; MAX_ESCAPED_LABEL_BYTES],
    length: usize,
    in_tag: bool,
    first: bool,
    overlong: bool,
}

impl Default for TagScanner {
    fn default() -> Self {
        Self {
            name: [0; MAX_ESCAPED_LABEL_BYTES],
            length: 0,
            in_tag: false,
            first: false,
            overlong: false,
        }
    }
}

impl TagScanner {
    fn feed(&mut self, byte: Option<u8>, result: &mut RawResultClassification) {
        let Some(byte) = byte else {
            self.finish(result);
            return;
        };
        if !self.in_tag {
            if byte == b'<' {
                self.in_tag = true;
                self.first = true;
            }
            return;
        }
        if self.first && byte == b'/' {
            self.first = false;
            return;
        }
        self.first = false;
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':' | b'.') {
            if let Some(target) = self.name.get_mut(self.length) {
                *target = byte;
                self.length += 1;
            } else {
                self.overlong = true;
            }
            return;
        }
        self.finish(result);
        if byte == b'<' {
            self.in_tag = true;
            self.first = true;
        }
    }

    fn finish(&mut self, result: &mut RawResultClassification) {
        if self.overlong {
            result.result_like_shape = true;
        } else if self.length > 0 {
            let name = std::str::from_utf8(&self.name[..self.length]).unwrap_or_default();
            if is_native_command_output_tag(name) {
                result.tagged_command_output = true;
            } else if is_result_label(name) {
                result.result_like_shape = true;
            }
        }
        self.length = 0;
        self.in_tag = false;
        self.first = false;
        self.overlong = false;
    }
}
