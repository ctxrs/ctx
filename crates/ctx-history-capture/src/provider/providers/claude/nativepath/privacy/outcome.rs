use super::*;

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct OutcomeEvidence {
    success: bool,
    failure: bool,
    exhausted: bool,
    timed_out: bool,
    exit_code: Option<i64>,
    duration_ms: Option<i64>,
}

impl OutcomeEvidence {
    fn classification(self) -> Option<OutputOutcome> {
        match (self.success, self.failure, self.exhausted) {
            (true, false, false) => Some(OutputOutcome::Success),
            (false, true, false) => Some(OutputOutcome::Failure),
            _ => None,
        }
    }
}

pub(super) fn scan_outcome_range(
    bytes: &[u8],
    range: Option<Range<usize>>,
    direct_tool_result: bool,
) -> Result<OutcomeEvidence, serde_json::Error> {
    let Some(range) = range else {
        return Ok(OutcomeEvidence::default());
    };
    let value = bytes
        .get(range)
        .ok_or_else(|| structural_error("Claude outcome range escaped its record"))?;
    let mut scanner = OutcomeScanner::new(value, direct_tool_result, false);
    scanner
        .scan()
        .map_err(|()| structural_error("malformed Claude structural outcome evidence"))?;
    Ok(scanner.evidence)
}

pub(super) fn scan_direct_output_range(
    bytes: &[u8],
    range: Option<Range<usize>>,
) -> Result<OutcomeEvidence, serde_json::Error> {
    let Some(range) = range else {
        return Ok(OutcomeEvidence::default());
    };
    let value = bytes
        .get(range)
        .ok_or_else(|| structural_error("Claude output range escaped its record"))?;
    let mut scanner = OutcomeScanner::new(value, true, true);
    scanner
        .scan()
        .map_err(|()| structural_error("malformed Claude structural output evidence"))?;
    Ok(scanner.evidence)
}

pub(super) fn combine_outcome_evidence(
    content: OutcomeEvidence,
    tool: OutcomeEvidence,
) -> OutputOutcomeMetadata {
    let content_class = content.classification();
    let tool_class = tool.classification();
    let outcome = if tool.timed_out {
        OutputOutcome::Timeout
    } else {
        match (content_class, tool_class) {
            (Some(left), Some(right)) if left != right => OutputOutcome::Unknown,
            (Some(outcome), _) | (_, Some(outcome)) => outcome,
            (None, None) => OutputOutcome::Unknown,
        }
    };
    OutputOutcomeMetadata {
        outcome,
        exit_code: tool.exit_code.and_then(|value| i32::try_from(value).ok()),
        duration_ms: tool.duration_ms.and_then(|value| u64::try_from(value).ok()),
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct StringFacts {
    nonempty: bool,
    success_status: bool,
    failure_status: bool,
    timeout_status: bool,
}

#[derive(Debug, Clone, Copy)]
enum ValueFacts {
    Null,
    Bool(bool),
    Integer(Option<i64>),
    String(StringFacts),
    Array(bool),
    Object(bool),
}

impl ValueFacts {
    fn error_indicates_failure(self) -> bool {
        match self {
            Self::Null => false,
            Self::Bool(value) => value,
            Self::Integer(value) => value.is_some_and(|value| value != 0),
            Self::String(value) => value.nonempty,
            Self::Array(nonempty) | Self::Object(nonempty) => nonempty,
        }
    }

    fn bool_value(self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(value),
            _ => None,
        }
    }

    fn integer(self) -> Option<i64> {
        match self {
            Self::Integer(value) => value,
            _ => None,
        }
    }

    fn string(self) -> Option<StringFacts> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutcomeKey {
    ExitCode,
    StatusCode,
    Success,
    IsError,
    TimedOut,
    Status,
    Error,
    Other,
}

struct OutcomeScanner<'a> {
    bytes: &'a [u8],
    index: usize,
    nodes: usize,
    direct_tool_result: bool,
    direct_is_error_success: bool,
    direct_exit_code: Option<i64>,
    direct_exit_code_snake: Option<i64>,
    direct_duration_ms: Option<i64>,
    direct_duration_ms_snake: Option<i64>,
    evidence: OutcomeEvidence,
}

impl<'a> OutcomeScanner<'a> {
    fn new(bytes: &'a [u8], direct_tool_result: bool, direct_is_error_success: bool) -> Self {
        Self {
            bytes,
            index: 0,
            nodes: 0,
            direct_tool_result,
            direct_is_error_success,
            direct_exit_code: None,
            direct_exit_code_snake: None,
            direct_duration_ms: None,
            direct_duration_ms_snake: None,
            evidence: OutcomeEvidence::default(),
        }
    }

    fn scan(&mut self) -> Result<(), ()> {
        self.whitespace();
        self.value(0)?;
        self.whitespace();
        if self.index != self.bytes.len() {
            return Err(());
        }
        self.evidence.exit_code = self.direct_exit_code.or(self.direct_exit_code_snake);
        self.evidence.duration_ms = self.direct_duration_ms.or(self.direct_duration_ms_snake);
        Ok(())
    }

    fn value(&mut self, depth: usize) -> Result<ValueFacts, ()> {
        if depth > MAX_SCAN_DEPTH {
            return Err(());
        }
        self.nodes = self.nodes.saturating_add(1);
        let active = self.nodes <= MAX_RESULT_OUTCOME_NODES;
        if !active {
            self.evidence.exhausted = true;
        }
        match self.peek().ok_or(())? {
            b'{' => self.object(depth, active),
            b'[' => self.array(depth, active),
            b'"' => {
                let range = self.string_range()?;
                let facts = string_facts(&self.bytes[range]);
                if depth <= 8 && facts.timeout_status {
                    self.evidence.timed_out = true;
                }
                Ok(ValueFacts::String(facts))
            }
            b't' => {
                self.literal(b"true")?;
                Ok(ValueFacts::Bool(true))
            }
            b'f' => {
                self.literal(b"false")?;
                Ok(ValueFacts::Bool(false))
            }
            b'n' => {
                self.literal(b"null")?;
                Ok(ValueFacts::Null)
            }
            _ => self.number(),
        }
    }

    fn object(&mut self, depth: usize, active: bool) -> Result<ValueFacts, ()> {
        self.expect(b'{')?;
        self.whitespace();
        if self.consume(b'}') {
            return Ok(ValueFacts::Object(false));
        }
        loop {
            let key = self.string_range()?;
            self.whitespace();
            self.expect(b':')?;
            self.whitespace();
            let facts = self.value(depth + 1)?;
            if active {
                self.apply_outcome_key(&self.bytes[key.clone()], facts, depth == 0);
            }
            if depth <= 8 {
                self.apply_timeout_key(&self.bytes[key.clone()], facts);
            }
            if self.direct_tool_result && depth == 0 {
                if raw_label_exact(&self.bytes[key.clone()], b"exitCode") {
                    self.direct_exit_code = facts.integer();
                } else if raw_label_exact(&self.bytes[key.clone()], b"exit_code") {
                    self.direct_exit_code_snake = facts.integer();
                } else if raw_label_exact(&self.bytes[key.clone()], b"durationMs") {
                    self.direct_duration_ms = facts.integer();
                } else if raw_label_exact(&self.bytes[key], b"duration_ms") {
                    self.direct_duration_ms_snake = facts.integer();
                }
            }
            self.whitespace();
            if self.consume(b'}') {
                return Ok(ValueFacts::Object(true));
            }
            self.expect(b',')?;
            self.whitespace();
        }
    }

    fn array(&mut self, depth: usize, _active: bool) -> Result<ValueFacts, ()> {
        self.expect(b'[')?;
        self.whitespace();
        if self.consume(b']') {
            return Ok(ValueFacts::Array(false));
        }
        loop {
            self.value(depth + 1)?;
            self.whitespace();
            if self.consume(b']') {
                return Ok(ValueFacts::Array(true));
            }
            self.expect(b',')?;
            self.whitespace();
        }
    }

    fn apply_outcome_key(&mut self, raw: &[u8], facts: ValueFacts, direct: bool) {
        match outcome_key(raw) {
            OutcomeKey::ExitCode => {
                if let Some(code) = facts.integer() {
                    self.evidence.success |= code == 0;
                    self.evidence.failure |= code != 0;
                }
            }
            OutcomeKey::StatusCode => {
                if let Some(code) = facts.integer() {
                    self.evidence.success |= (200..400).contains(&code);
                    self.evidence.failure |= code >= 400;
                }
            }
            OutcomeKey::Success => {
                if let Some(success) = facts.bool_value() {
                    self.evidence.success |= success;
                    self.evidence.failure |= !success;
                }
            }
            OutcomeKey::IsError => {
                if let Some(is_error) = facts.bool_value() {
                    self.evidence.failure |= is_error;
                    self.evidence.success |= self.direct_is_error_success && direct && !is_error;
                }
            }
            OutcomeKey::TimedOut => {
                self.evidence.failure |= facts.bool_value().unwrap_or(false);
            }
            OutcomeKey::Status => {
                if let Some(status) = facts.string() {
                    self.evidence.success |= status.success_status;
                    self.evidence.failure |= status.failure_status;
                }
            }
            OutcomeKey::Error => {
                self.evidence.failure |= facts.error_indicates_failure();
            }
            OutcomeKey::Other => {}
        }
    }

    fn apply_timeout_key(&mut self, raw: &[u8], facts: ValueFacts) {
        if outcome_key(raw) == OutcomeKey::TimedOut
            && (facts.bool_value().unwrap_or(false)
                || facts.string().is_some_and(|value| value.timeout_status))
        {
            self.evidence.timed_out = true;
        }
    }

    fn number(&mut self) -> Result<ValueFacts, ()> {
        let start = self.index;
        while self
            .peek()
            .is_some_and(|byte| !byte.is_ascii_whitespace() && !b",]}".contains(&byte))
        {
            self.index += 1;
        }
        if self.index == start {
            return Err(());
        }
        let token = std::str::from_utf8(&self.bytes[start..self.index]).map_err(|_| ())?;
        let integer = (!token.contains(['.', 'e', 'E']))
            .then(|| token.parse::<i64>().ok())
            .flatten();
        Ok(ValueFacts::Integer(integer))
    }

    fn string_range(&mut self) -> Result<Range<usize>, ()> {
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

    fn literal(&mut self, literal: &[u8]) -> Result<(), ()> {
        if self.bytes.get(self.index..self.index + literal.len()) == Some(literal) {
            self.index += literal.len();
            Ok(())
        } else {
            Err(())
        }
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

fn outcome_key(raw: &[u8]) -> OutcomeKey {
    let mut normalized = [0_u8; 32];
    let mut length = 0;
    if visit_decoded_chars(raw, |character| {
        if character.is_ascii_alphanumeric() {
            let target = normalized.get_mut(length).ok_or(())?;
            *target = u8::try_from(u32::from(character))
                .unwrap_or_default()
                .to_ascii_lowercase();
            length += 1;
        }
        Ok(())
    })
    .is_err()
    {
        return OutcomeKey::Other;
    }
    match &normalized[..length] {
        b"exitcode" => OutcomeKey::ExitCode,
        b"statuscode" => OutcomeKey::StatusCode,
        b"success" | b"ok" => OutcomeKey::Success,
        b"iserror" => OutcomeKey::IsError,
        b"timedout" | b"timeout" => OutcomeKey::TimedOut,
        b"status" | b"state" | b"outcome" => OutcomeKey::Status,
        b"error" => OutcomeKey::Error,
        _ => OutcomeKey::Other,
    }
}

fn raw_label_exact(raw: &[u8], expected: &[u8]) -> bool {
    let mut decoded = [0_u8; MAX_ESCAPED_LABEL_BYTES];
    decode_label(raw, &mut decoded).is_ok_and(|value| value == expected)
}

fn string_facts(raw: &[u8]) -> StringFacts {
    let mut decoded = [0_u8; 32];
    let mut length = 0_usize;
    let mut invalid_status = false;
    let mut nonempty = false;
    let mut started = false;
    let mut pending_whitespace = false;
    let decoded_ok = visit_decoded_chars(raw, |character| {
        if character.is_whitespace() {
            if started {
                pending_whitespace = true;
            }
            return Ok(());
        }
        nonempty = true;
        if pending_whitespace {
            invalid_status = true;
        }
        started = true;
        pending_whitespace = false;
        if !character.is_ascii() {
            invalid_status = true;
            return Ok(());
        }
        if let Some(target) = decoded.get_mut(length) {
            *target = u8::try_from(u32::from(character))
                .unwrap_or_default()
                .to_ascii_lowercase();
            length += 1;
        } else {
            invalid_status = true;
        }
        Ok(())
    })
    .is_ok();
    invalid_status |= !decoded_ok;
    let normalized = (!invalid_status)
        .then(|| std::str::from_utf8(&decoded[..length]).ok())
        .flatten();
    let success_status = normalized.is_some_and(|status| {
        matches!(
            status,
            "success" | "succeeded" | "complete" | "completed" | "ok" | "passed"
        )
    });
    let failure_status = normalized.is_some_and(|status| {
        matches!(
            status,
            "failed"
                | "failure"
                | "error"
                | "errored"
                | "timeout"
                | "timed_out"
                | "timedout"
                | "cancelled"
                | "canceled"
        )
    });
    let timeout_status =
        normalized.is_some_and(|status| matches!(status, "timeout" | "timed_out" | "timedout"));
    StringFacts {
        nonempty,
        success_status,
        failure_status,
        timeout_status,
    }
}
