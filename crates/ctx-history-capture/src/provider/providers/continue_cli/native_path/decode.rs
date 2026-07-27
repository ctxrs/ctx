use serde::{de::IgnoredAny, Deserialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum JsonKind {
    Null,
    Bool,
    Number,
    String,
    Array,
    Object,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct JsonSpan<'a> {
    raw: &'a [u8],
    kind: JsonKind,
}

impl<'a> JsonSpan<'a> {
    pub(super) fn raw(self) -> &'a [u8] {
        self.raw
    }

    pub(super) fn kind(self) -> JsonKind {
        self.kind
    }

    pub(super) fn encoded_len(self) -> usize {
        self.raw.len()
    }

    pub(super) fn range_within(self, bytes: &[u8]) -> Option<std::ops::Range<usize>> {
        let bytes_start = bytes.as_ptr() as usize;
        let span_start = self.raw.as_ptr() as usize;
        let start = span_start.checked_sub(bytes_start)?;
        let end = start.checked_add(self.raw.len())?;
        (end <= bytes.len()).then_some(start..end)
    }

    pub(super) fn string_normalized_is(self, expected: &str) -> bool {
        self.kind == JsonKind::String
            && json_string_normalized_matches_ascii(self.raw, expected.as_bytes())
    }

    pub(super) fn as_object(self) -> Result<ObjectIter<'a>, JsonScanError> {
        if self.kind != JsonKind::Object {
            return Err(JsonScanError::new("expected JSON object"));
        }
        ObjectIter::new(self.raw)
    }

    pub(super) fn as_array(self) -> Result<ArrayIter<'a>, JsonScanError> {
        if self.kind != JsonKind::Array {
            return Err(JsonScanError::new("expected JSON array"));
        }
        ArrayIter::new(self.raw)
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct JsonKey<'a> {
    raw: &'a [u8],
}

impl JsonKey<'_> {
    pub(super) fn is(self, expected: &str) -> bool {
        json_string_matches_ascii(self.raw, expected.as_bytes())
    }

    pub(super) fn is_result_like(self) -> bool {
        [
            "output",
            "outputs",
            "future",
            "futureoutput",
            "result",
            "results",
            "response",
            "stdout",
            "stderr",
            "returnvalue",
            "commandoutput",
            "commandresult",
            "tooloutput",
            "toolresult",
            "terminaloutput",
            "executionresult",
        ]
        .iter()
        .any(|candidate| json_string_normalized_matches_ascii(self.raw, candidate.as_bytes()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct JsonScanError {
    message: &'static str,
}

impl JsonScanError {
    fn new(message: &'static str) -> Self {
        Self { message }
    }
}

impl std::fmt::Display for JsonScanError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for JsonScanError {}

pub(super) fn validate_and_root(bytes: &[u8]) -> Result<JsonSpan<'_>, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    IgnoredAny::deserialize(&mut deserializer)?;
    deserializer.end()?;

    // serde_json has already established complete syntax and bounded nesting.
    // The provider scanner below only creates borrowed spans into these bytes.
    root_span(bytes).map_err(|error| {
        <serde_json::Error as serde::de::Error>::custom(format!(
            "validated JSON could not be scanned: {error}"
        ))
    })
}

pub(super) fn decode_string(
    span: JsonSpan<'_>,
    maximum_bytes: usize,
) -> Result<Option<String>, serde_json::Error> {
    if span.kind != JsonKind::String {
        return Ok(None);
    }
    // A decoded UTF-8 scalar can consume at most six JSON source bytes when it
    // is represented as a \uXXXX escape. Refuse clearly over-bound identifiers
    // before asking serde_json to allocate their String.
    let maximum_encoded = maximum_bytes.saturating_mul(6).saturating_add(2);
    if span.raw.len() > maximum_encoded {
        return Ok(None);
    }
    let value = serde_json::from_slice::<String>(span.raw)?;
    Ok((value.len() <= maximum_bytes).then_some(value))
}

pub(super) fn decode_unbounded_string(
    span: JsonSpan<'_>,
) -> Result<Option<String>, serde_json::Error> {
    if span.kind != JsonKind::String {
        return Ok(None);
    }
    serde_json::from_slice::<String>(span.raw).map(Some)
}

pub(super) fn decode_f64(span: JsonSpan<'_>) -> Option<f64> {
    match span.kind {
        JsonKind::Number => std::str::from_utf8(span.raw).ok()?.parse().ok(),
        JsonKind::String => {
            let raw = decode_string(span, 128).ok().flatten()?;
            raw.parse().ok()
        }
        JsonKind::Null | JsonKind::Bool | JsonKind::Array | JsonKind::Object => None,
    }
}

pub(super) fn decode_i64(span: JsonSpan<'_>) -> Option<i64> {
    (span.kind == JsonKind::Number)
        .then(|| std::str::from_utf8(span.raw).ok()?.parse().ok())
        .flatten()
}

pub(super) fn decode_u64(span: JsonSpan<'_>) -> Option<u64> {
    (span.kind == JsonKind::Number)
        .then(|| std::str::from_utf8(span.raw).ok()?.parse().ok())
        .flatten()
}

pub(super) fn decode_bool(span: JsonSpan<'_>) -> Option<bool> {
    match span.raw {
        b"true" if span.kind == JsonKind::Bool => Some(true),
        b"false" if span.kind == JsonKind::Bool => Some(false),
        _ => None,
    }
}

pub(super) struct ObjectIter<'a> {
    cursor: Cursor<'a>,
    first: bool,
    finished: bool,
}

impl<'a> ObjectIter<'a> {
    fn new(raw: &'a [u8]) -> Result<Self, JsonScanError> {
        let mut cursor = Cursor::new(raw);
        cursor.expect(b'{')?;
        Ok(Self {
            cursor,
            first: true,
            finished: false,
        })
    }
}

impl<'a> Iterator for ObjectIter<'a> {
    type Item = Result<(JsonKey<'a>, JsonSpan<'a>), JsonScanError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        self.cursor.skip_whitespace();
        if self.first {
            self.first = false;
            if self.cursor.peek() == Some(b'}') {
                self.finished = true;
                self.cursor.position = self.cursor.position.saturating_add(1);
                return None;
            }
        } else {
            if self.cursor.peek() == Some(b'}') {
                self.finished = true;
                self.cursor.position = self.cursor.position.saturating_add(1);
                return None;
            }
            if let Err(error) = self.cursor.expect(b',') {
                self.finished = true;
                return Some(Err(error));
            }
        }
        self.cursor.skip_whitespace();
        let key = match self.cursor.take_string() {
            Ok(raw) => JsonKey { raw },
            Err(error) => {
                self.finished = true;
                return Some(Err(error));
            }
        };
        self.cursor.skip_whitespace();
        if let Err(error) = self.cursor.expect(b':') {
            self.finished = true;
            return Some(Err(error));
        }
        match self.cursor.take_value() {
            Ok(value) => Some(Ok((key, value))),
            Err(error) => {
                self.finished = true;
                Some(Err(error))
            }
        }
    }
}

pub(super) struct ArrayIter<'a> {
    raw: &'a [u8],
    cursor: JsonArrayCursor,
}

impl<'a> ArrayIter<'a> {
    fn new(raw: &'a [u8]) -> Result<Self, JsonScanError> {
        Ok(Self {
            raw,
            cursor: JsonArrayCursor::new(raw)?,
        })
    }
}

impl<'a> Iterator for ArrayIter<'a> {
    type Item = Result<JsonSpan<'a>, JsonScanError>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.cursor.next(self.raw) {
            Ok(Some(value)) => Some(Ok(value)),
            Ok(None) => None,
            Err(error) => Some(Err(error)),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct JsonArrayCursor {
    position: usize,
    first: bool,
    finished: bool,
}

impl JsonArrayCursor {
    pub(super) fn new(raw: &[u8]) -> Result<Self, JsonScanError> {
        let mut cursor = Cursor::new(raw);
        cursor.expect(b'[')?;
        Ok(Self {
            position: cursor.position,
            first: true,
            finished: false,
        })
    }

    pub(super) fn next<'a>(
        &mut self,
        raw: &'a [u8],
    ) -> Result<Option<JsonSpan<'a>>, JsonScanError> {
        if self.finished {
            return Ok(None);
        }
        let mut cursor = Cursor {
            bytes: raw,
            position: self.position,
        };
        cursor.skip_whitespace();
        if self.first {
            self.first = false;
            if cursor.peek() == Some(b']') {
                self.finished = true;
                cursor.position = cursor.position.saturating_add(1);
                self.position = cursor.position;
                return Ok(None);
            }
        } else {
            if cursor.peek() == Some(b']') {
                self.finished = true;
                cursor.position = cursor.position.saturating_add(1);
                self.position = cursor.position;
                return Ok(None);
            }
            if let Err(error) = cursor.expect(b',') {
                self.finished = true;
                return Err(error);
            }
        }
        match cursor.take_value() {
            Ok(value) => {
                self.position = cursor.position;
                Ok(Some(value))
            }
            Err(error) => {
                self.finished = true;
                Err(error)
            }
        }
    }
}

fn root_span(bytes: &[u8]) -> Result<JsonSpan<'_>, JsonScanError> {
    let mut cursor = Cursor::new(bytes);
    let value = cursor.take_value()?;
    cursor.skip_whitespace();
    if cursor.position != bytes.len() {
        return Err(JsonScanError::new("trailing JSON data"));
    }
    Ok(value)
}

struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.position = self.position.saturating_add(1);
        }
    }

    fn expect(&mut self, expected: u8) -> Result<(), JsonScanError> {
        self.skip_whitespace();
        if self.peek() != Some(expected) {
            return Err(JsonScanError::new("unexpected JSON token"));
        }
        self.position = self.position.saturating_add(1);
        Ok(())
    }

    fn take_string(&mut self) -> Result<&'a [u8], JsonScanError> {
        self.skip_whitespace();
        let start = self.position;
        if self.peek() != Some(b'"') {
            return Err(JsonScanError::new("expected JSON string"));
        }
        self.position = self.position.saturating_add(1);
        let mut escaped = false;
        while let Some(byte) = self.peek() {
            self.position = self.position.saturating_add(1);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                return Ok(&self.bytes[start..self.position]);
            }
        }
        Err(JsonScanError::new("unterminated JSON string"))
    }

    fn take_value(&mut self) -> Result<JsonSpan<'a>, JsonScanError> {
        self.skip_whitespace();
        let start = self.position;
        let kind = match self.peek() {
            Some(b'"') => {
                self.take_string()?;
                JsonKind::String
            }
            Some(b'{') => {
                self.skip_object()?;
                JsonKind::Object
            }
            Some(b'[') => {
                self.skip_array()?;
                JsonKind::Array
            }
            Some(b't' | b'f') => {
                self.skip_scalar();
                JsonKind::Bool
            }
            Some(b'n') => {
                self.skip_scalar();
                JsonKind::Null
            }
            Some(b'-' | b'0'..=b'9') => {
                self.skip_scalar();
                JsonKind::Number
            }
            _ => return Err(JsonScanError::new("expected JSON value")),
        };
        Ok(JsonSpan {
            raw: &self.bytes[start..self.position],
            kind,
        })
    }

    fn skip_object(&mut self) -> Result<(), JsonScanError> {
        self.expect(b'{')?;
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.position = self.position.saturating_add(1);
            return Ok(());
        }
        loop {
            self.take_string()?;
            self.expect(b':')?;
            self.take_value()?;
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.position = self.position.saturating_add(1),
                Some(b'}') => {
                    self.position = self.position.saturating_add(1);
                    return Ok(());
                }
                _ => return Err(JsonScanError::new("unterminated JSON object")),
            }
        }
    }

    fn skip_array(&mut self) -> Result<(), JsonScanError> {
        self.expect(b'[')?;
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.position = self.position.saturating_add(1);
            return Ok(());
        }
        loop {
            self.take_value()?;
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.position = self.position.saturating_add(1),
                Some(b']') => {
                    self.position = self.position.saturating_add(1);
                    return Ok(());
                }
                _ => return Err(JsonScanError::new("unterminated JSON array")),
            }
        }
    }

    fn skip_scalar(&mut self) {
        while !matches!(
            self.peek(),
            None | Some(b' ' | b'\n' | b'\r' | b'\t' | b',' | b']' | b'}')
        ) {
            self.position = self.position.saturating_add(1);
        }
    }
}

fn json_string_matches_ascii(raw: &[u8], expected: &[u8]) -> bool {
    if raw.len() < 2 || raw.first() != Some(&b'"') || raw.last() != Some(&b'"') {
        return false;
    }
    let mut source = 1;
    let mut target = 0;
    while source + 1 < raw.len() {
        let decoded = if raw[source] != b'\\' {
            let value = raw[source];
            source += 1;
            value
        } else {
            source += 1;
            let Some(escape) = raw.get(source).copied() else {
                return false;
            };
            source += 1;
            match escape {
                b'"' | b'\\' | b'/' => escape,
                b'b' => 0x08,
                b'f' => 0x0c,
                b'n' => b'\n',
                b'r' => b'\r',
                b't' => b'\t',
                b'u' => {
                    if source.saturating_add(4) > raw.len().saturating_sub(1) {
                        return false;
                    }
                    let Some(codepoint) = parse_ascii_hex(&raw[source..source + 4]) else {
                        return false;
                    };
                    source += 4;
                    codepoint
                }
                _ => return false,
            }
        };
        if expected.get(target) != Some(&decoded) {
            return false;
        }
        target += 1;
    }
    target == expected.len()
}

fn json_string_normalized_matches_ascii(raw: &[u8], expected: &[u8]) -> bool {
    if raw.len() < 2 || raw.first() != Some(&b'"') || raw.last() != Some(&b'"') {
        return false;
    }
    let mut source = 1;
    let mut target = 0;
    while source + 1 < raw.len() {
        let decoded = if raw[source] != b'\\' {
            let value = raw[source];
            source += 1;
            value
        } else {
            source += 1;
            let Some(escape) = raw.get(source).copied() else {
                return false;
            };
            source += 1;
            match escape {
                b'"' | b'\\' | b'/' => escape,
                b'b' => 0x08,
                b'f' => 0x0c,
                b'n' => b'\n',
                b'r' => b'\r',
                b't' => b'\t',
                b'u' => {
                    if source.saturating_add(4) > raw.len().saturating_sub(1) {
                        return false;
                    }
                    let Some(codepoint) = parse_ascii_hex(&raw[source..source + 4]) else {
                        return false;
                    };
                    source += 4;
                    codepoint
                }
                _ => return false,
            }
        };
        if decoded.is_ascii_alphanumeric() {
            let normalized = decoded.to_ascii_lowercase();
            if expected.get(target) != Some(&normalized) {
                return false;
            }
            target += 1;
        }
    }
    target == expected.len()
}

fn parse_ascii_hex(raw: &[u8]) -> Option<u8> {
    let value = raw.iter().try_fold(0_u16, |value, byte| {
        let digit = match byte {
            b'0'..=b'9' => u16::from(byte - b'0'),
            b'a'..=b'f' => u16::from(byte - b'a' + 10),
            b'A'..=b'F' => u16::from(byte - b'A' + 10),
            _ => return None,
        };
        value.checked_mul(16)?.checked_add(digit)
    })?;
    u8::try_from(value).ok()
}
