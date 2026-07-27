use std::io::BufRead;

use serde_json::Value;

use crate::{
    common::time::parse_rfc3339_utc, CaptureError, OutputOutcome, OutputOutcomeMetadata, Result,
    MAX_PROVIDER_JSONL_LINE_BYTES,
};

use super::{
    super::{
        checkpoint::{CursorCheckpoint, CursorPrefixBuilder, CursorSessionCheckpoint},
        projection::{project_cursor_record, update_cursor_session_checkpoint},
    },
    classify_cursor_line, decode_sanitized_record,
    stream::{read_bounded_line, strip_line_ending},
    CursorRejectionKind,
};

const CURSOR_OUTPUT_PAGE_MAX_UNITS: usize = 64;
const CURSOR_OUTPUT_PAGE_MAX_OBSERVATIONS: usize = 64;
const CURSOR_OUTPUT_PAGE_MAX_BYTES: usize = 8 * 1024 * 1024;
const CURSOR_OUTPUT_PAGE_ENVELOPE_BYTES: usize = 512 * 1024;
const CURSOR_OUTPUT_OBSERVATION_ENVELOPE_BYTES: usize = 2 * 1024;

#[derive(Debug)]
pub(crate) struct CursorOutputFact {
    pub(crate) semantic_ordinal: u64,
    pub(crate) subrecord_index: u32,
    pub(crate) byte_start: u64,
    pub(crate) byte_end_exclusive: u64,
    pub(crate) occurred_at_unix_ms: Option<i64>,
    pub(crate) call_id: Option<String>,
    pub(crate) tool_name: Option<String>,
    pub(crate) outcome: OutputOutcomeMetadata,
    pub(crate) content: Vec<u8>,
}

#[derive(Debug)]
pub(crate) struct CursorOutputPage {
    pub(crate) expected_checkpoint: CursorCheckpoint,
    pub(crate) next_checkpoint: CursorCheckpoint,
    pub(crate) terminal: bool,
    pub(crate) logical_units: usize,
    pub(crate) conservative_serialized_bytes: usize,
    pub(crate) outputs: Vec<CursorOutputFact>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CursorOutputScanOutcome {
    Complete,
    PrefixMismatch,
    Stopped,
}

pub(crate) fn scan_cursor_output_pages(
    reader: &mut impl BufRead,
    resume: Option<&CursorCheckpoint>,
    core_checkpoint: &CursorCheckpoint,
    emit: &mut dyn FnMut(CursorOutputPage) -> Result<bool>,
) -> Result<CursorOutputScanOutcome> {
    if !core_checkpoint.is_supported() {
        return Err(CaptureError::InvalidPayload(
            "Cursor output replay requires a supported committed Core checkpoint".to_owned(),
        ));
    }
    let resume = resume.filter(|checkpoint| checkpoint.is_supported());
    let resume_offset = resume.map_or(0, |checkpoint| checkpoint.next_byte_offset);
    if resume_offset > core_checkpoint.next_byte_offset {
        return Ok(CursorOutputScanOutcome::PrefixMismatch);
    }

    let mut prefix = CursorPrefixBuilder::new();
    let mut session = CursorSessionCheckpoint::default();
    let initial_checkpoint = CursorCheckpoint::new(prefix.proof(), session.clone(), false, false);
    let mut pages =
        CursorOutputPageBuffer::new(emit, resume.cloned().unwrap_or(initial_checkpoint));
    let mut rejection_count = 0_u64;
    let mut verified_resume = resume.is_none() || resume_offset == 0;

    loop {
        if prefix.complete_bytes() == core_checkpoint.next_byte_offset {
            break;
        }
        let byte_start = prefix.complete_bytes();
        let line = read_bounded_line(reader, MAX_PROVIDER_JSONL_LINE_BYTES)?;
        if line.consumed_bytes == 0 || !line.terminated {
            return Err(CaptureError::InvalidPayload(
                "Cursor output replay ended before committed Core frontier".to_owned(),
            ));
        }
        let byte_end_exclusive = byte_start.saturating_add(line.consumed_bytes);
        if byte_end_exclusive > core_checkpoint.next_byte_offset {
            return Err(CaptureError::InvalidPayload(
                "Cursor committed Core frontier splits an output replay record".to_owned(),
            ));
        }
        if !verified_resume && byte_end_exclusive > resume_offset {
            return Ok(CursorOutputScanOutcome::PrefixMismatch);
        }
        let verifying = !verified_resume && byte_end_exclusive <= resume_offset;
        let payload = strip_line_ending(&line.bytes);

        if line.oversized {
            rejection_count = rejection_count.saturating_add(1);
            prefix.record_rejection(
                CursorRejectionKind::Oversized,
                line.consumed_bytes,
                line.content_sha256,
            );
            pages.advance(checkpoint(&prefix, &session, rejection_count, false));
        } else if payload.iter().all(u8::is_ascii_whitespace) {
            prefix.record_blank(line.consumed_bytes, line.content_sha256);
            pages.advance(checkpoint(&prefix, &session, rejection_count, false));
        } else {
            let classification = match classify_cursor_line(payload) {
                Ok(classification) => classification,
                Err(kind) => {
                    rejection_count = rejection_count.saturating_add(1);
                    prefix.record_rejection(kind, line.consumed_bytes, line.content_sha256);
                    pages.advance(checkpoint(&prefix, &session, rejection_count, false));
                    if !verified_resume && byte_end_exclusive == resume_offset {
                        if !resume_matches(
                            resume.expect("unverified resume exists"),
                            &prefix,
                            &session,
                        ) {
                            return Ok(CursorOutputScanOutcome::PrefixMismatch);
                        }
                        verified_resume = true;
                    }
                    continue;
                }
            };
            let semantic_ordinal = prefix.semantic_records();
            let physical_ordinal = prefix.physical_lines();
            let sanitized = match decode_sanitized_record(
                payload,
                semantic_ordinal,
                physical_ordinal,
                byte_start,
                byte_end_exclusive,
                &classification,
            ) {
                Ok(sanitized) => sanitized,
                Err(_) => {
                    rejection_count = rejection_count.saturating_add(1);
                    prefix.record_rejection(
                        CursorRejectionKind::UnsupportedShape,
                        line.consumed_bytes,
                        line.content_sha256,
                    );
                    pages.advance(checkpoint(&prefix, &session, rejection_count, false));
                    if !verified_resume && byte_end_exclusive == resume_offset {
                        if !resume_matches(
                            resume.expect("unverified resume exists"),
                            &prefix,
                            &session,
                        ) {
                            return Ok(CursorOutputScanOutcome::PrefixMismatch);
                        }
                        verified_resume = true;
                    }
                    continue;
                }
            };
            prefix.record_semantic(line.consumed_bytes, line.content_sha256, &sanitized)?;
            let projected = project_cursor_record(&sanitized)?;
            update_cursor_session_checkpoint(&mut session, &projected);
            let next_checkpoint = checkpoint(&prefix, &session, rejection_count, false);
            if verifying {
                pages.advance(next_checkpoint);
            } else {
                let outputs = classify_cursor_outputs(
                    payload,
                    semantic_ordinal,
                    byte_start,
                    byte_end_exclusive,
                    classification.timestamp.as_deref(),
                )?;
                if !pages.push_semantic(next_checkpoint, outputs)? {
                    return Ok(CursorOutputScanOutcome::Stopped);
                }
            }
        }

        if !verified_resume && byte_end_exclusive == resume_offset {
            if !resume_matches(resume.expect("unverified resume exists"), &prefix, &session) {
                return Ok(CursorOutputScanOutcome::PrefixMismatch);
            }
            verified_resume = true;
        }
    }

    if !verified_resume
        || !checkpoint_matches_core(core_checkpoint, &prefix, &session, rejection_count)
    {
        return Ok(CursorOutputScanOutcome::PrefixMismatch);
    }
    let final_checkpoint = checkpoint(&prefix, &session, rejection_count, core_checkpoint.terminal);
    pages.finish(final_checkpoint)
}

fn checkpoint(
    prefix: &CursorPrefixBuilder,
    session: &CursorSessionCheckpoint,
    rejection_count: u64,
    terminal: bool,
) -> CursorCheckpoint {
    CursorCheckpoint::new(
        prefix.proof(),
        session.clone(),
        terminal,
        rejection_count > 0,
    )
}

fn resume_matches(
    resume: &CursorCheckpoint,
    prefix: &CursorPrefixBuilder,
    session: &CursorSessionCheckpoint,
) -> bool {
    resume.next_byte_offset == prefix.complete_bytes()
        && resume.next_physical_line == prefix.physical_lines()
        && resume.next_semantic_ordinal == prefix.semantic_records()
        && resume.prefix == prefix.proof()
        && &resume.session == session
}

fn checkpoint_matches_core(
    core: &CursorCheckpoint,
    prefix: &CursorPrefixBuilder,
    session: &CursorSessionCheckpoint,
    rejection_count: u64,
) -> bool {
    core.next_byte_offset == prefix.complete_bytes()
        && core.next_physical_line == prefix.physical_lines()
        && core.next_semantic_ordinal == prefix.semantic_records()
        && core.prefix == prefix.proof()
        && &core.session == session
        && matches!(
            core.disposition,
            super::super::checkpoint::CursorCheckpointDisposition::WithholdForRejections
        ) == (rejection_count > 0)
}

struct CursorOutputPageBuffer<'a> {
    emit: &'a mut dyn FnMut(CursorOutputPage) -> Result<bool>,
    expected_checkpoint: CursorCheckpoint,
    next_checkpoint: CursorCheckpoint,
    logical_units: usize,
    conservative_serialized_bytes: usize,
    outputs: Vec<CursorOutputFact>,
}

impl<'a> CursorOutputPageBuffer<'a> {
    fn new(
        emit: &'a mut dyn FnMut(CursorOutputPage) -> Result<bool>,
        expected_checkpoint: CursorCheckpoint,
    ) -> Self {
        Self {
            emit,
            next_checkpoint: expected_checkpoint.clone(),
            expected_checkpoint,
            logical_units: 0,
            conservative_serialized_bytes: CURSOR_OUTPUT_PAGE_ENVELOPE_BYTES,
            outputs: Vec::new(),
        }
    }

    fn advance(&mut self, next_checkpoint: CursorCheckpoint) {
        self.next_checkpoint = next_checkpoint;
    }

    fn push_semantic(
        &mut self,
        next_checkpoint: CursorCheckpoint,
        outputs: Vec<CursorOutputFact>,
    ) -> Result<bool> {
        if outputs.len() > CURSOR_OUTPUT_PAGE_MAX_OBSERVATIONS {
            return Err(CaptureError::InvalidPayload(
                "one Cursor record exceeds the bounded output observation count".to_owned(),
            ));
        }
        let output_bytes = outputs.iter().try_fold(0_usize, |total, output| {
            total
                .checked_add(output_wire_bytes(output))
                .ok_or(CaptureError::SystemInvariant(
                    "Cursor output page byte accounting overflowed",
                ))
        })?;
        let requires_flush = self.logical_units > 0
            && (self.logical_units >= CURSOR_OUTPUT_PAGE_MAX_UNITS
                || self.outputs.len().saturating_add(outputs.len())
                    > CURSOR_OUTPUT_PAGE_MAX_OBSERVATIONS
                || self
                    .conservative_serialized_bytes
                    .saturating_add(output_bytes)
                    > CURSOR_OUTPUT_PAGE_MAX_BYTES);
        if requires_flush && !self.flush(false)? {
            return Ok(false);
        }
        if self
            .conservative_serialized_bytes
            .saturating_add(output_bytes)
            > CURSOR_OUTPUT_PAGE_MAX_BYTES
        {
            return Err(CaptureError::InvalidPayload(
                "one Cursor record exceeds the bounded output page byte limit".to_owned(),
            ));
        }
        self.logical_units = self.logical_units.saturating_add(1);
        self.conservative_serialized_bytes = self
            .conservative_serialized_bytes
            .saturating_add(output_bytes);
        self.outputs.extend(outputs);
        self.next_checkpoint = next_checkpoint;
        Ok(true)
    }

    fn finish(mut self, final_checkpoint: CursorCheckpoint) -> Result<CursorOutputScanOutcome> {
        self.next_checkpoint = final_checkpoint;
        if (self.expected_checkpoint != self.next_checkpoint || !self.outputs.is_empty())
            && !self.flush(true)?
        {
            return Ok(CursorOutputScanOutcome::Stopped);
        }
        Ok(CursorOutputScanOutcome::Complete)
    }

    fn flush(&mut self, terminal: bool) -> Result<bool> {
        let next_checkpoint = self.next_checkpoint.clone();
        let page = CursorOutputPage {
            expected_checkpoint: self.expected_checkpoint.clone(),
            next_checkpoint: next_checkpoint.clone(),
            terminal,
            logical_units: self.logical_units.max(1),
            conservative_serialized_bytes: self.conservative_serialized_bytes,
            outputs: std::mem::take(&mut self.outputs),
        };
        let keep_scanning = (self.emit)(page)?;
        if keep_scanning {
            self.expected_checkpoint = next_checkpoint.clone();
            self.next_checkpoint = next_checkpoint;
            self.logical_units = 0;
            self.conservative_serialized_bytes = CURSOR_OUTPUT_PAGE_ENVELOPE_BYTES;
        }
        Ok(keep_scanning)
    }
}

fn output_wire_bytes(output: &CursorOutputFact) -> usize {
    CURSOR_OUTPUT_OBSERVATION_ENVELOPE_BYTES
        .saturating_add(output.content.len())
        .saturating_add(output.call_id.as_deref().map_or(0, str::len))
        .saturating_add(output.tool_name.as_deref().map_or(0, str::len))
}

fn classify_cursor_outputs(
    payload: &[u8],
    semantic_ordinal: u64,
    byte_start: u64,
    byte_end_exclusive: u64,
    timestamp: Option<&str>,
) -> Result<Vec<CursorOutputFact>> {
    let value: Value = serde_json::from_slice(payload)?;
    if cursor_value_is_redacted(&value) {
        return Ok(Vec::new());
    }
    let content = value
        .pointer("/message/content")
        .or_else(|| value.get("content"));
    let Some(content) = content else {
        return Ok(Vec::new());
    };
    let Some(blocks) = content.as_array() else {
        return Ok(Vec::new());
    };
    let occurred_at_unix_ms = timestamp
        .and_then(parse_rfc3339_utc)
        .map(|timestamp| timestamp.timestamp_millis());
    let mut outputs = Vec::new();
    let mut result_index = 0_u32;
    for block in blocks {
        if block.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }
        let subrecord_index = result_index;
        result_index = result_index.checked_add(1).ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Cursor output subrecord index exceeds the supported range".to_owned(),
            )
        })?;
        if cursor_value_is_redacted(block) {
            continue;
        }
        let Some(content) = cursor_output_content(block)? else {
            continue;
        };
        outputs.push(CursorOutputFact {
            semantic_ordinal,
            subrecord_index,
            byte_start,
            byte_end_exclusive,
            occurred_at_unix_ms,
            call_id: cursor_output_atom(
                block,
                &[
                    "call_id",
                    "callId",
                    "tool_call_id",
                    "toolCallId",
                    "tool_use_id",
                    "toolUseId",
                    "id",
                ],
            ),
            tool_name: cursor_output_atom(block, &["tool_name", "toolName", "name", "tool"]),
            outcome: cursor_output_outcome(block, &value),
            content: content.as_bytes().to_vec(),
        });
    }
    Ok(outputs)
}

fn cursor_output_content(value: &Value) -> Result<Option<&str>> {
    for field in ["content", "output", "text"] {
        let Some(selected) = value.get(field) else {
            continue;
        };
        return match selected {
            Value::String(content) => Ok(Some(content)),
            Value::Null => Ok(None),
            _ => Err(CaptureError::InvalidPayload(format!(
                "Cursor tool_result field {field} is not a string"
            ))),
        };
    }
    Ok(None)
}

fn cursor_output_atom(value: &Value, fields: &[&str]) -> Option<String> {
    fields
        .iter()
        .find_map(|field| value.get(*field).and_then(Value::as_str))
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

fn cursor_output_outcome(block: &Value, record: &Value) -> OutputOutcomeMetadata {
    let mut outcome = cursor_value_outcome(block);
    if outcome.outcome == OutputOutcome::Unknown {
        outcome = cursor_value_outcome(record);
    }
    outcome
}

fn cursor_value_outcome(value: &Value) -> OutputOutcomeMetadata {
    let timeout = cursor_value_has_timeout(value);
    let failure = cursor_value_has_failure(value);
    let success = cursor_value_has_success(value);
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
        exit_code: cursor_i64(value, &["exit_code", "exitCode"])
            .and_then(|value| i32::try_from(value).ok()),
        duration_ms: cursor_u64(value, &["duration_ms", "durationMs", "duration"]),
    }
}

fn cursor_value_has_timeout(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(cursor_value_has_timeout),
        Value::Object(values) => {
            values.iter().any(|(key, value)| {
                matches!(cursor_normalized_key(key).as_str(), "timeout" | "timedout")
                    && value.as_bool() == Some(true)
            }) || values.values().any(cursor_value_has_timeout)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn cursor_value_has_failure(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(cursor_value_has_failure),
        Value::Object(values) => {
            cursor_object_has_failure(values) || values.values().any(cursor_value_has_failure)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn cursor_object_has_failure(values: &serde_json::Map<String, Value>) -> bool {
    values
        .get("success")
        .and_then(Value::as_bool)
        .is_some_and(|success| !success)
        || values
            .get("isError")
            .or_else(|| values.get("is_error"))
            .and_then(Value::as_bool)
            == Some(true)
        || ["exit_code", "exitCode"].iter().any(|field| {
            values
                .get(*field)
                .and_then(Value::as_i64)
                .is_some_and(|code| code != 0)
        })
        || ["status_code", "statusCode"].iter().any(|field| {
            values
                .get(*field)
                .and_then(Value::as_i64)
                .is_some_and(|code| code >= 400)
        })
        || ["status", "state", "outcome"].iter().any(|field| {
            values
                .get(*field)
                .and_then(Value::as_str)
                .is_some_and(|status| {
                    matches!(
                        status.trim().to_ascii_lowercase().as_str(),
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
                })
        })
        || values.get("error").is_some_and(cursor_error_is_failure)
}

fn cursor_error_is_failure(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::String(value) => !value.trim().is_empty(),
        Value::Number(value) => value.as_i64().is_some_and(|number| number != 0),
        Value::Array(values) => !values.is_empty(),
        Value::Object(values) => !values.is_empty(),
    }
}

fn cursor_value_has_success(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(cursor_value_has_success),
        Value::Object(values) => {
            values.iter().any(|(key, value)| {
                let key = cursor_normalized_key(key);
                (matches!(key.as_str(), "success" | "ok") && value.as_bool() == Some(true))
                    || (key == "exitcode" && value.as_i64() == Some(0))
                    || (key == "statuscode"
                        && value
                            .as_i64()
                            .is_some_and(|code| (200..400).contains(&code)))
                    || (matches!(key.as_str(), "iserror" | "timedout" | "timeout")
                        && value.as_bool() == Some(false))
                    || (matches!(key.as_str(), "status" | "state" | "outcome")
                        && value.as_str().is_some_and(|status| {
                            matches!(
                                status.trim().to_ascii_lowercase().as_str(),
                                "success"
                                    | "succeeded"
                                    | "complete"
                                    | "completed"
                                    | "ok"
                                    | "passed"
                            )
                        }))
            }) || values.values().any(cursor_value_has_success)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn cursor_i64(value: &Value, fields: &[&str]) -> Option<i64> {
    match value {
        Value::Array(values) => values.iter().find_map(|value| cursor_i64(value, fields)),
        Value::Object(values) => values
            .iter()
            .find_map(|(key, value)| {
                fields
                    .iter()
                    .any(|field| key == field)
                    .then(|| value.as_i64())
                    .flatten()
            })
            .or_else(|| values.values().find_map(|value| cursor_i64(value, fields))),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
    }
}

fn cursor_u64(value: &Value, fields: &[&str]) -> Option<u64> {
    match value {
        Value::Array(values) => values.iter().find_map(|value| cursor_u64(value, fields)),
        Value::Object(values) => values
            .iter()
            .find_map(|(key, value)| {
                fields
                    .iter()
                    .any(|field| key == field)
                    .then(|| value.as_u64())
                    .flatten()
            })
            .or_else(|| values.values().find_map(|value| cursor_u64(value, fields))),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
    }
}

fn cursor_normalized_key(key: &str) -> String {
    key.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn cursor_value_is_redacted(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    ["redacted", "is_redacted", "isRedacted"]
        .iter()
        .filter_map(|field| object.get(*field))
        .any(|flag| flag.as_bool() != Some(false))
        || ["status", "state"]
            .iter()
            .filter_map(|field| object.get(*field).and_then(Value::as_str))
            .any(|state| matches!(state, "redacted" | "output-redacted"))
}
