//! Pre-parse admission for raw Codex JSONL records.
//!
//! Codex rollout files are dominated by records the reader materializes
//! nothing from: telemetry counters, agent-loop chatter, and result envelopes
//! whose only projection is a counter. Handing those to the structural probe
//! costs a full `serde_json` walk plus borrowed-field extraction for state that
//! is dropped on the next line.
//!
//! This module decides, from the raw record bytes alone, whether the structural
//! probe is needed at all. It allocates nothing, borrows nothing beyond the
//! caller's slice, and reads each record at most once.
//!
//! # Why the walk is still complete
//!
//! A record that fails [`super::classify_codex_record`] contributes to the
//! scanner's malformed and rejected-record counters. A prefilter that skipped
//! a malformed record would silently misclassify it. So the prefilter is not a
//! prefix matcher: it validates the whole record against a deliberately
//! *strict subset* of the structural probe's grammar and reports
//! [`CodexRecordAdmission::Probe`] the moment anything is unusual. Every skip
//! therefore carries a proof that the structural probe would have succeeded,
//! and every non-proof falls back to the unchanged path.
//!
//! The class itself is decided by [`super::codex_record_class`] — the same
//! function the structural probe uses — so the skip set cannot drift away from
//! what the reader materializes.

use super::{codex_record_class, CodexRecordClass};

/// Nesting depth the prefilter is willing to validate. Deeper records fall
/// back to the structural probe rather than growing an unbounded stack.
const MAX_PREFILTER_DEPTH: usize = 24;
/// Longest envelope/payload discriminator the prefilter will read. Codex type
/// names are short; anything longer is not a type this reader knows.
const MAX_PREFILTER_TYPE_BYTES: usize = 64;
/// Longest simple string the prefilter will accept for `timestamp`.
const MAX_PREFILTER_TIMESTAMP_BYTES: usize = 64;

const SWAR_LOW_BITS: u64 = 0x0101_0101_0101_0101;
const SWAR_HIGH_BITS: u64 = 0x8080_8080_8080_8080;
/// Masks off the low five bits, so a lane is zero exactly when its byte is a
/// JSON control byte (`< 0x20`).
const SWAR_CONTROL_MASK: u64 = 0xE0E0_E0E0_E0E0_E0E0;

/// Whether a record needs the structural probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in super::super) enum CodexRecordAdmission {
    /// The raw bytes prove the record parses and that its class projects
    /// nothing the structural probe would have to supply.
    NoProjection(CodexSkipProjection),
    /// Hand the record to the structural probe unchanged.
    Probe,
}

/// The complete projection a skipped record still owes the scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in super::super) enum CodexSkipProjection {
    /// Nothing but the ignored-record counter.
    Ignored,
    /// Nothing but the native-result counters.
    NativeResult,
}

/// Classifies one raw Codex JSONL record before any parse, allocation, or hash.
///
/// The record must already be trimmed of its JSONL terminator.
pub(in super::super) fn prefilter_codex_record(record: &[u8]) -> CodexRecordAdmission {
    match Prefilter::new(record)
        .envelope()
        .and_then(codex_skip_projection)
    {
        Some(projection) => CodexRecordAdmission::NoProjection(projection),
        None => CodexRecordAdmission::Probe,
    }
}

/// The reader's skip set, expressed against the class the reader projects.
///
/// * [`CodexRecordClass::Ignored`] increments one counter and returns an empty
///   projection.
/// * [`CodexRecordClass::ExcludedResult`] with an ineligible kind increments
///   the two native-result counters and returns an empty projection before it
///   reads anything the probe decoded.
///
/// Every other class reaches parsed state, so it stays on the probe path.
pub(in super::super) fn codex_skip_projection(
    class: CodexRecordClass,
) -> Option<CodexSkipProjection> {
    match class {
        CodexRecordClass::Ignored => Some(CodexSkipProjection::Ignored),
        CodexRecordClass::ExcludedResult(kind) if !kind.is_eligible_output() => {
            Some(CodexSkipProjection::NativeResult)
        }
        CodexRecordClass::ExcludedResult(_)
        | CodexRecordClass::SessionMeta
        | CodexRecordClass::TurnContext
        | CodexRecordClass::Retained(_) => None,
    }
}

struct Prefilter<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Prefilter<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    /// Validates the whole record and returns the class its discriminators
    /// select, or `None` when anything at all is unusual.
    fn envelope(mut self) -> Option<CodexRecordClass> {
        self.whitespace();
        self.take(b'{')?;
        self.whitespace();
        let mut record_type = None;
        let mut item_type = None;
        let mut saw_record_type = false;
        let mut saw_timestamp = false;
        let mut saw_payload = false;
        // An empty envelope has no `type`, which the structural probe rejects
        // as a missing field.
        if self.peek()? == b'}' {
            return None;
        }
        loop {
            let key = self.simple_string(MAX_PREFILTER_TYPE_BYTES)?;
            self.whitespace();
            self.take(b':')?;
            self.whitespace();
            match key {
                "type" => {
                    if saw_record_type {
                        return None;
                    }
                    saw_record_type = true;
                    let value = self.simple_string(MAX_PREFILTER_TYPE_BYTES)?;
                    // Stop as soon as the discriminators rule a skip out. A
                    // record that has to be probed anyway must not pay for a
                    // second full walk of its body.
                    decided_skip(value, item_type)?;
                    record_type = Some(value);
                }
                "timestamp" => {
                    if saw_timestamp {
                        return None;
                    }
                    saw_timestamp = true;
                    if self.peek()? == b'n' {
                        self.literal(b"null")?;
                    } else {
                        self.simple_string(MAX_PREFILTER_TIMESTAMP_BYTES)?;
                    }
                }
                "payload" => {
                    if saw_payload {
                        return None;
                    }
                    saw_payload = true;
                    item_type = self.payload(1, record_type)?;
                }
                _ => self.value(1)?,
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
        Some(codex_record_class(record_type?, item_type))
    }

    /// Validates a `payload` member and reports its discriminator.
    ///
    /// The structural probe accepts any JSON here: a non-object payload simply
    /// has no item type.
    fn payload(&mut self, depth: usize, record_type: Option<&str>) -> Option<Option<&'a str>> {
        if self.peek()? != b'{' {
            decided_skip_with(record_type, None)?;
            self.value(depth)?;
            return Some(None);
        }
        self.offset += 1;
        self.whitespace();
        if self.peek()? == b'}' {
            decided_skip_with(record_type, None)?;
            self.offset += 1;
            return Some(None);
        }
        let mut item_type = None;
        let mut saw_item_type = false;
        let mut saw_call_id = false;
        loop {
            let key = self.simple_string(MAX_PREFILTER_TYPE_BYTES)?;
            self.whitespace();
            self.take(b':')?;
            self.whitespace();
            match key {
                "type" => {
                    if saw_item_type {
                        return None;
                    }
                    saw_item_type = true;
                    if self.peek()? == b'n' {
                        self.literal(b"null")?;
                        decided_skip_with(record_type, None)?;
                    } else {
                        let value = self.simple_string(MAX_PREFILTER_TYPE_BYTES)?;
                        decided_skip_with(record_type, Some(value))?;
                        item_type = Some(value);
                    }
                }
                "call_id" => {
                    if saw_call_id {
                        return None;
                    }
                    saw_call_id = true;
                    if self.peek()? == b'n' {
                        self.literal(b"null")?;
                    } else {
                        self.simple_string(MAX_PREFILTER_TYPE_BYTES)?;
                    }
                }
                _ => self.value(depth + 1)?,
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
        Some(item_type)
    }

    fn value(&mut self, depth: usize) -> Option<()> {
        if depth > MAX_PREFILTER_DEPTH {
            return None;
        }
        match self.peek()? {
            b'"' => self.skip_string(),
            b'{' => self.skip_object(depth),
            b'[' => self.skip_array(depth),
            b't' => self.literal(b"true"),
            b'f' => self.literal(b"false"),
            b'n' => self.literal(b"null"),
            b'-' | b'0'..=b'9' => self.number(),
            _ => None,
        }
    }

    fn skip_object(&mut self, depth: usize) -> Option<()> {
        self.offset += 1;
        self.whitespace();
        if self.peek()? == b'}' {
            self.offset += 1;
            return Some(());
        }
        loop {
            self.skip_string()?;
            self.whitespace();
            self.take(b':')?;
            self.whitespace();
            self.value(depth + 1)?;
            self.whitespace();
            match self.peek()? {
                b',' => {
                    self.offset += 1;
                    self.whitespace();
                }
                b'}' => {
                    self.offset += 1;
                    return Some(());
                }
                _ => return None,
            }
        }
    }

    fn skip_array(&mut self, depth: usize) -> Option<()> {
        self.offset += 1;
        self.whitespace();
        if self.peek()? == b']' {
            self.offset += 1;
            return Some(());
        }
        loop {
            self.value(depth + 1)?;
            self.whitespace();
            match self.peek()? {
                b',' => {
                    self.offset += 1;
                    self.whitespace();
                }
                b']' => {
                    self.offset += 1;
                    return Some(());
                }
                _ => return None,
            }
        }
    }

    /// Accepts an escape-free ASCII string and returns it undecoded.
    ///
    /// Keys and probed values are the only strings the structural probe turns
    /// into `Cow<str>`, so they are the only ones whose UTF-8 validity matters.
    /// Requiring plain ASCII is stricter than the probe, which is safe: a
    /// rejected string only costs a fallback parse. Because escapes are
    /// refused, the raw bytes are already the decoded value the probe would
    /// have compared against.
    fn simple_string(&mut self, max_bytes: usize) -> Option<&'a str> {
        self.take(b'"')?;
        let start = self.offset;
        let rest = self.bytes.get(start..)?;
        let plain = plain_json_string_bytes(rest);
        let end = start.checked_add(plain)?;
        (self.bytes.get(end).copied()? == b'"').then_some(())?;
        let text = self.bytes.get(start..end)?;
        (text.len() <= max_bytes).then_some(())?;
        text.is_ascii().then_some(())?;
        self.offset = end.checked_add(1)?;
        std::str::from_utf8(text).ok()
    }

    /// Validates a JSON string the structural probe would only walk past.
    ///
    /// Non-ASCII bytes are accepted here because the probe hands these strings
    /// to `IgnoredAny`, which does not validate UTF-8 either.
    fn skip_string(&mut self) -> Option<()> {
        self.take(b'"')?;
        loop {
            let rest = self.bytes.get(self.offset..)?;
            let plain = plain_json_string_bytes(rest);
            self.offset = self.offset.checked_add(plain)?;
            match self.peek()? {
                b'"' => {
                    self.offset += 1;
                    return Some(());
                }
                b'\\' => {
                    self.offset += 1;
                    let escaped = self.peek()?;
                    self.offset += 1;
                    match escaped {
                        b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {}
                        b'u' => self.unicode_escape()?,
                        _ => return None,
                    }
                }
                // `plain_json_string_bytes` only stops on a quote, a backslash,
                // or a control byte, and control bytes are not legal here.
                _ => return None,
            }
        }
    }

    /// Consumes `\uXXXX`, refusing surrogates so the prefilter never has to
    /// reason about pairing rules the probe applies elsewhere.
    fn unicode_escape(&mut self) -> Option<()> {
        let end = self.offset.checked_add(4)?;
        let digits = self.bytes.get(self.offset..end)?;
        let mut value = 0_u32;
        for digit in digits {
            let nibble = match digit {
                b'0'..=b'9' => u32::from(digit - b'0'),
                b'a'..=b'f' => u32::from(digit - b'a' + 10),
                b'A'..=b'F' => u32::from(digit - b'A' + 10),
                _ => return None,
            };
            value = value * 16 + nibble;
        }
        (!(0xD800..=0xDFFF).contains(&value)).then_some(())?;
        self.offset = end;
        Some(())
    }

    /// Accepts only the canonical JSON number grammar. Anything looser is left
    /// to the structural probe.
    fn number(&mut self) -> Option<()> {
        if self.peek()? == b'-' {
            self.offset += 1;
        }
        match self.peek()? {
            b'0' => self.offset += 1,
            b'1'..=b'9' => self.digits()?,
            _ => return None,
        }
        if self.peek() == Some(b'.') {
            self.offset += 1;
            self.digits()?;
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.offset += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.offset += 1;
            }
            self.digits()?;
        }
        Some(())
    }

    fn digits(&mut self) -> Option<()> {
        let start = self.offset;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.offset += 1;
        }
        (self.offset > start).then_some(())
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

    /// JSON insignificant whitespace. Deliberately excludes form feed, which
    /// Rust's `is_ascii_whitespace` accepts but JSON does not.
    fn whitespace(&mut self) {
        while self
            .peek()
            .is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\n' | b'\r'))
        {
            self.offset += 1;
        }
    }
}

/// Abandons the walk as soon as the discriminators read so far rule a skip out.
///
/// A record that has to be probed anyway must not pay for a second full walk of
/// its body, so validation stops at the discriminator rather than at the closing
/// brace. Bailing early is always safe: it only sends a record down the
/// unchanged path.
///
/// Both arguments are the discriminators the reader's class function takes, so
/// this stays in step with the class function by construction. An unknown
/// envelope type is skippable whatever its payload says, which is why the
/// missing-item-type probe is a sound early answer.
fn decided_skip(record_type: &str, item_type: Option<&str>) -> Option<()> {
    codex_skip_projection(codex_record_class(record_type, item_type)).map(|_| ())
}

fn decided_skip_with(record_type: Option<&str>, item_type: Option<&str>) -> Option<()> {
    match record_type {
        Some(record_type) => decided_skip(record_type, item_type),
        // The envelope type has not been read yet, so nothing is decided.
        None => Some(()),
    }
}

/// Counts leading bytes that cannot end a JSON string body.
///
/// Eight bytes are tested per iteration. The word tests never miss a stop byte;
/// they may stop early, which only costs a scalar step.
fn plain_json_string_bytes(bytes: &[u8]) -> usize {
    let mut offset = 0;
    while offset + 8 <= bytes.len() {
        let Some(chunk) = bytes.get(offset..offset + 8) else {
            break;
        };
        let Ok(chunk) = <[u8; 8]>::try_from(chunk) else {
            break;
        };
        let word = u64::from_ne_bytes(chunk);
        if word_contains_byte(word, b'"')
            || word_contains_byte(word, b'\\')
            || word_contains_control_byte(word)
        {
            break;
        }
        offset += 8;
    }
    let tail = &bytes[offset..];
    offset
        + tail
            .iter()
            .position(|byte| *byte < 0x20 || matches!(*byte, b'"' | b'\\'))
            .unwrap_or(tail.len())
}

fn word_contains_byte(word: u64, needle: u8) -> bool {
    word_contains_zero(word ^ u64::from(needle).wrapping_mul(SWAR_LOW_BITS))
}

fn word_contains_control_byte(word: u64) -> bool {
    word_contains_zero(word & SWAR_CONTROL_MASK)
}

fn word_contains_zero(word: u64) -> bool {
    word.wrapping_sub(SWAR_LOW_BITS) & !word & SWAR_HIGH_BITS != 0
}

#[cfg(test)]
#[path = "prefilter_tests.rs"]
mod tests;
