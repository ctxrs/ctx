use super::*;

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
            direct_output_present: false,
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

pub(in super::super) fn probe_structural_output(
    line: &[u8],
) -> serde_json::Result<CodexStructuralOutput> {
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
        has_exact_display_field: payload.direct_output_present,
    })
}

pub(super) fn status_is_failure(value: &[u8]) -> bool {
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

pub(super) fn status_is_success(value: &[u8]) -> bool {
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

pub(super) fn decoded_json_key_is_before_or_same(candidate: &[u8], current: &[u8]) -> bool {
    decoded_json_key_cmp(candidate, current) != Ordering::Greater
}

pub(super) fn decoded_json_key_cmp(candidate: &[u8], current: &[u8]) -> Ordering {
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
