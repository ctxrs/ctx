use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType};

use super::super::wire::{
    warp_message_arm, warp_tool_name, warp_tool_result_name, WarpMessageArm, WarpWireCursor,
    WarpWireValue,
};
use crate::{CaptureError, OutputOutcome, Result};

#[derive(Clone, Debug)]
pub(super) struct WarpDecodedTask {
    pub(super) messages: Vec<WarpDecodedMessage>,
    pub(super) counters: WarpDecodeCounters,
}

#[derive(Clone, Debug)]
pub(super) struct WarpDecodedMessage {
    pub(super) message_ordinal: u32,
    pub(super) message_id: Option<String>,
    pub(super) request_id: Option<String>,
    pub(super) occurred_at: Option<DateTime<Utc>>,
    pub(super) legacy_indexed: bool,
    pub(super) payload: WarpDecodedMessagePayload,
}

#[derive(Clone, Debug)]
pub(super) enum WarpDecodedMessagePayload {
    Retained(WarpRetainedMessage),
    Output(WarpDecodedOutput),
    Excluded,
}

#[derive(Clone, Debug)]
pub(super) struct WarpRetainedMessage {
    pub(super) event_type: EventType,
    pub(super) role: Option<EventRole>,
    pub(super) kind: &'static str,
    pub(super) body: String,
    pub(super) tool_call: bool,
}

#[derive(Clone, Debug)]
pub(super) struct WarpDecodedOutput {
    pub(super) call_id: Option<String>,
    pub(super) tool_name: &'static str,
    pub(super) outcome: OutputOutcome,
    pub(super) body: String,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct WarpDecodeCounters {
    pub(super) unknown_fields: u64,
    pub(super) unknown_oneofs: u64,
    pub(super) native_result_records: u64,
    pub(super) native_result_envelope_bytes: u64,
    pub(super) native_result_body_bytes_observed: u64,
    pub(super) native_results_success: u64,
    pub(super) native_results_failure: u64,
    pub(super) native_results_timeout: u64,
    pub(super) native_results_unknown: u64,
    pub(super) malformed_output_records: u64,
}

#[derive(Clone, Debug)]
enum WarpClassifiedBody<'a> {
    Bytes(Option<&'a [u8]>),
    Malformed,
}

#[derive(Clone, Debug)]
struct WarpToolResultClassification<'a> {
    call_id: Option<&'a [u8]>,
    tool_name: &'static str,
    outcome: OutputOutcome,
    body: WarpClassifiedBody<'a>,
}

pub(super) fn decode_warp_native_task(data: &[u8]) -> Result<WarpDecodedTask> {
    let mut cursor = WarpWireCursor::new(data);
    let mut task_id = None;
    let mut message_payloads = Vec::new();
    let mut counters = WarpDecodeCounters::default();
    while let Some(field) = cursor.next()? {
        match (field.number, field.value) {
            (1, WarpWireValue::LengthDelimited(value)) => task_id = Some(value),
            (2 | 3 | 6, WarpWireValue::LengthDelimited(_)) => {}
            (5, WarpWireValue::LengthDelimited(value)) => message_payloads.push(value),
            _ => counters.unknown_fields = counters.unknown_fields.saturating_add(1),
        }
    }

    if let Some(task_id) = task_id {
        let _ = super::super::wire::warp_wire_text(task_id)?;
    }
    let mut messages = Vec::new();
    for (ordinal, message) in message_payloads.into_iter().enumerate() {
        let message_ordinal = u32::try_from(ordinal).map_err(|_| {
            CaptureError::InvalidPayload("Warp task has too many protobuf messages".to_owned())
        })?;
        let payload = decode_warp_native_message(message, message_ordinal, &mut counters)?;
        messages.push(payload);
    }
    Ok(WarpDecodedTask { messages, counters })
}

fn decode_warp_native_message(
    data: &[u8],
    message_ordinal: u32,
    counters: &mut WarpDecodeCounters,
) -> Result<WarpDecodedMessage> {
    let mut cursor = WarpWireCursor::new(data);
    let mut message_id = None;
    let mut request_id = None;
    let mut timestamp = None;
    let mut selected_arm = None;
    while let Some(field) = cursor.next()? {
        match (field.number, field.value) {
            (1, WarpWireValue::LengthDelimited(value)) => message_id = Some(value),
            (11, WarpWireValue::LengthDelimited(_)) => {}
            (13, WarpWireValue::LengthDelimited(value)) => request_id = Some(value),
            (14, WarpWireValue::LengthDelimited(value)) => timestamp = Some(value),
            (number, WarpWireValue::LengthDelimited(value))
                if warp_message_arm(number).is_some() =>
            {
                let arm = warp_message_arm(number).ok_or(CaptureError::SystemInvariant(
                    "Warp message arm classification changed during decode",
                ))?;
                if matches!(arm, WarpMessageArm::Unknown(_)) {
                    counters.unknown_fields = counters.unknown_fields.saturating_add(1);
                    counters.unknown_oneofs = counters.unknown_oneofs.saturating_add(1);
                }
                selected_arm = Some((arm, value));
            }
            _ => counters.unknown_fields = counters.unknown_fields.saturating_add(1),
        }
    }

    let message_id = message_id
        .map(warp_text_owned)
        .transpose()?
        .and_then(bounded_linkage_owned);
    let Some((arm, payload)) = selected_arm else {
        return Ok(WarpDecodedMessage {
            message_ordinal,
            message_id,
            request_id: None,
            occurred_at: None,
            legacy_indexed: false,
            payload: WarpDecodedMessagePayload::Excluded,
        });
    };
    if matches!(arm, WarpMessageArm::Unknown(_)) {
        return Ok(WarpDecodedMessage {
            message_ordinal,
            message_id,
            request_id: None,
            occurred_at: None,
            legacy_indexed: false,
            payload: WarpDecodedMessagePayload::Excluded,
        });
    }
    if matches!(
        arm,
        WarpMessageArm::ToolResult | WarpMessageArm::DebugOutput
    ) {
        let decoded_payload = decode_output(arm, payload, counters)?;
        let needs_metadata = matches!(&decoded_payload, WarpDecodedMessagePayload::Output(_));
        let (request_id, occurred_at) = if needs_metadata {
            decode_output_metadata(request_id, timestamp, counters)
        } else {
            (None, None)
        };
        return Ok(WarpDecodedMessage {
            message_ordinal,
            message_id,
            request_id,
            occurred_at,
            legacy_indexed: true,
            payload: decoded_payload,
        });
    }

    let request_id = request_id
        .map(warp_text_owned)
        .transpose()?
        .and_then(bounded_linkage_owned);
    let occurred_at = timestamp.map(decode_timestamp).transpose()?.flatten();
    let (event_type, role, kind, body, tool_call) = match arm {
        WarpMessageArm::UserQuery => (
            EventType::Message,
            Some(EventRole::User),
            "user_query",
            decode_last_nested_text(payload, 1)?.unwrap_or_default(),
            false,
        ),
        WarpMessageArm::AgentOutput => (
            EventType::Message,
            Some(EventRole::Assistant),
            "agent_output",
            decode_last_nested_text(payload, 1)?.unwrap_or_default(),
            false,
        ),
        WarpMessageArm::ToolCall => {
            let field = last_length_delimited_field(payload)?.map_or(0, |(field, _)| field);
            (
                EventType::ToolCall,
                Some(EventRole::Assistant),
                "tool_call",
                format!("tool call: {}", warp_tool_name(field)),
                true,
            )
        }
        WarpMessageArm::SystemQuery => (
            EventType::Message,
            Some(EventRole::System),
            "system_query",
            decode_system_query(payload)?,
            false,
        ),
        WarpMessageArm::AgentReasoning => (
            EventType::Message,
            Some(EventRole::Assistant),
            "agent_reasoning",
            decode_last_nested_text(payload, 1)?.unwrap_or_default(),
            false,
        ),
        WarpMessageArm::Summarization => (
            EventType::Message,
            Some(EventRole::Assistant),
            "summarization",
            decode_summarization(payload)?,
            false,
        ),
        WarpMessageArm::ReceivedMessages => (
            EventType::Message,
            Some(EventRole::Assistant),
            "messages_received_from_agents",
            decode_received_messages(payload)?,
            false,
        ),
        WarpMessageArm::ToolResult | WarpMessageArm::DebugOutput | WarpMessageArm::Unknown(_) => {
            return Err(CaptureError::SystemInvariant(
                "Warp excluded message arm reached retained-body construction",
            ))
        }
    };
    Ok(WarpDecodedMessage {
        message_id,
        request_id,
        occurred_at,
        message_ordinal,
        legacy_indexed: tool_call || !body.is_empty(),
        payload: WarpDecodedMessagePayload::Retained(WarpRetainedMessage {
            event_type,
            role,
            kind,
            body,
            tool_call,
        }),
    })
}

fn decode_output_metadata(
    request_id: Option<&[u8]>,
    timestamp: Option<&[u8]>,
    counters: &mut WarpDecodeCounters,
) -> (Option<String>, Option<DateTime<Utc>>) {
    let request_id = request_id
        .map(warp_text_owned)
        .transpose()
        .map(|value| value.and_then(bounded_linkage_owned));
    let occurred_at = timestamp
        .map(decode_timestamp)
        .transpose()
        .map(Option::flatten);
    let metadata_error = request_id
        .as_ref()
        .err()
        .map(ToString::to_string)
        .or_else(|| occurred_at.as_ref().err().map(ToString::to_string));
    if let Some(error) = metadata_error {
        let _ = error;
        counters.malformed_output_records = counters.malformed_output_records.saturating_add(1);
    }
    (request_id.unwrap_or(None), occurred_at.unwrap_or(None))
}

fn decode_output(
    arm: WarpMessageArm,
    payload: &[u8],
    counters: &mut WarpDecodeCounters,
) -> Result<WarpDecodedMessagePayload> {
    counters.native_result_records = counters.native_result_records.saturating_add(1);
    counters.native_result_envelope_bytes = counters
        .native_result_envelope_bytes
        .saturating_add(u64::try_from(payload.len()).unwrap_or(u64::MAX));
    let classification = match arm {
        WarpMessageArm::ToolResult => match classify_tool_result(payload) {
            Ok(classification) => classification,
            Err(error) => {
                counters.malformed_output_records =
                    counters.malformed_output_records.saturating_add(1);
                let _ = error;
                return Ok(WarpDecodedMessagePayload::Excluded);
            }
        },
        WarpMessageArm::DebugOutput => return Ok(WarpDecodedMessagePayload::Excluded),
        _ => {
            return Err(CaptureError::SystemInvariant(
                "Warp retained message was classified as an output",
            ))
        }
    };
    let body_bytes = match &classification.body {
        WarpClassifiedBody::Bytes(body) => {
            u64::try_from(body.map_or(0, <[u8]>::len)).unwrap_or(u64::MAX)
        }
        WarpClassifiedBody::Malformed => 0,
    };
    counters.native_result_body_bytes_observed = counters
        .native_result_body_bytes_observed
        .saturating_add(body_bytes);
    match classification.outcome {
        OutputOutcome::Success => {
            counters.native_results_success = counters.native_results_success.saturating_add(1);
        }
        OutputOutcome::Failure => {
            counters.native_results_failure = counters.native_results_failure.saturating_add(1);
        }
        OutputOutcome::Timeout => {
            counters.native_results_timeout = counters.native_results_timeout.saturating_add(1);
        }
        OutputOutcome::Unknown => {
            counters.native_results_unknown = counters.native_results_unknown.saturating_add(1);
        }
    }
    let body = match classification.body {
        WarpClassifiedBody::Bytes(Some(body)) => match warp_text_owned(body) {
            Ok(body) if !body.trim().is_empty() => body,
            Ok(_) | Err(_) => {
                counters.malformed_output_records =
                    counters.malformed_output_records.saturating_add(1);
                return Ok(WarpDecodedMessagePayload::Excluded);
            }
        },
        WarpClassifiedBody::Bytes(None) | WarpClassifiedBody::Malformed => {
            counters.malformed_output_records = counters.malformed_output_records.saturating_add(1);
            return Ok(WarpDecodedMessagePayload::Excluded);
        }
    };
    let call_id = classification
        .call_id
        .map(warp_text_owned)
        .transpose()
        .map(|value| value.and_then(bounded_linkage_owned));
    if call_id.is_err() {
        counters.malformed_output_records = counters.malformed_output_records.saturating_add(1);
    }
    Ok(WarpDecodedMessagePayload::Output(WarpDecodedOutput {
        call_id: call_id.unwrap_or(None),
        tool_name: classification.tool_name,
        outcome: classification.outcome,
        body,
    }))
}

fn classify_tool_result(data: &[u8]) -> Result<WarpToolResultClassification<'_>> {
    let mut cursor = WarpWireCursor::new(data);
    let mut call_id = None;
    let mut variant = None;
    while let Some(field) = cursor.next()? {
        match (field.number, field.value) {
            (1, WarpWireValue::LengthDelimited(value)) => call_id = Some(value),
            (1 | 11, _) => {}
            (number, WarpWireValue::LengthDelimited(value)) => {
                variant = Some((number, value));
            }
            _ => {}
        }
    }
    let Some((variant, payload)) = variant else {
        return Ok(WarpToolResultClassification {
            call_id,
            tool_name: warp_tool_result_name(0),
            outcome: OutputOutcome::Unknown,
            body: WarpClassifiedBody::Bytes(None),
        });
    };
    if variant == 2 {
        let (outcome, body) = classify_run_shell_result(payload)?;
        return Ok(WarpToolResultClassification {
            call_id,
            tool_name: warp_tool_result_name(variant),
            outcome,
            body,
        });
    }
    let selected = last_length_delimited_field(payload)?;
    let selected_field = selected.map_or(0, |(field, _)| field);
    let outcome = match (variant, selected_field) {
        (4 | 23, 1) => OutputOutcome::Success,
        (
            3 | 5 | 6 | 9 | 10 | 15 | 16 | 19 | 24 | 25 | 26 | 28 | 30 | 32 | 34 | 36 | 38 | 41
            | 42,
            1,
        ) => OutputOutcome::Success,
        (
            3 | 5 | 6 | 9 | 10 | 15 | 16 | 19 | 24 | 25 | 26 | 28 | 30 | 32 | 34 | 36 | 38 | 41
            | 42,
            2,
        ) => OutputOutcome::Failure,
        (29 | 31, 1) => OutputOutcome::Success,
        (29 | 31, 2 | 3) => OutputOutcome::Failure,
        (17 | 35, 2) | (27, 3) => OutputOutcome::Success,
        (39, 1) => OutputOutcome::Success,
        (39, 2 | 3) => OutputOutcome::Failure,
        _ => OutputOutcome::Unknown,
    };
    let body = classified_result_body(variant, selected);
    Ok(WarpToolResultClassification {
        call_id,
        tool_name: warp_tool_result_name(variant),
        outcome,
        body,
    })
}

fn classify_run_shell_result(data: &[u8]) -> Result<(OutputOutcome, WarpClassifiedBody<'_>)> {
    let mut cursor = WarpWireCursor::new(data);
    let mut deprecated_output = None;
    let mut terminal = None;
    while let Some(field) = cursor.next()? {
        match (field.number, field.value) {
            (1, WarpWireValue::LengthDelimited(value)) => {
                deprecated_output = Some(value);
            }
            (4..=6, WarpWireValue::LengthDelimited(value)) => {
                terminal = Some((field.number, value));
            }
            _ => {}
        }
    }
    let Some((field, payload)) = terminal else {
        return Ok((
            OutputOutcome::Unknown,
            WarpClassifiedBody::Bytes(deprecated_output),
        ));
    };
    let body = match field {
        4 | 5 => classified_nested_text(payload, 1),
        6 => match classified_nested_text(payload, 1) {
            WarpClassifiedBody::Bytes(None) => WarpClassifiedBody::Bytes(deprecated_output),
            body => body,
        },
        _ => WarpClassifiedBody::Bytes(None),
    };
    let outcome = match field {
        5 => OutputOutcome::Success,
        6 => OutputOutcome::Failure,
        _ => OutputOutcome::Unknown,
    };
    Ok((outcome, body))
}

fn classified_result_body(variant: u32, selected: Option<(u32, &[u8])>) -> WarpClassifiedBody<'_> {
    let Some((arm, arm_payload)) = selected else {
        return WarpClassifiedBody::Bytes(None);
    };
    match (variant, arm) {
        (4 | 23, 1) => WarpClassifiedBody::Bytes(Some(arm_payload)),
        (10, 1 | 2) => classified_nested_text(arm_payload, 1),
        (
            3 | 5 | 6 | 9 | 15 | 16 | 19 | 24 | 25 | 26 | 28 | 30 | 32 | 34 | 36 | 38 | 41 | 42,
            2,
        ) => classified_nested_text(arm_payload, 1),
        (29 | 31, 3) => classified_nested_text(arm_payload, 1),
        (39, 2 | 3) => classified_nested_text(arm_payload, 1),
        (17 | 35, 1 | 2) | (27, 2 | 3) => classified_nested_text(arm_payload, 1),
        _ => WarpClassifiedBody::Bytes(None),
    }
}

fn classified_nested_text(data: &[u8], field: u32) -> WarpClassifiedBody<'_> {
    match last_length_delimited_value(data, field) {
        Ok(value) => WarpClassifiedBody::Bytes(value),
        Err(_) => WarpClassifiedBody::Malformed,
    }
}

fn decode_timestamp(data: &[u8]) -> Result<Option<DateTime<Utc>>> {
    let mut cursor = WarpWireCursor::new(data);
    let mut seconds = None;
    let mut nanos = 0_u32;
    while let Some(field) = cursor.next()? {
        match (field.number, field.value) {
            (1, WarpWireValue::Varint(value)) => seconds = Some(value as i64),
            (2, WarpWireValue::Varint(value)) => {
                nanos = u32::try_from(value).map_err(|_| {
                    CaptureError::InvalidPayload(
                        "Warp protobuf timestamp nanos overflowed".to_owned(),
                    )
                })?;
            }
            _ => {}
        }
    }
    if nanos >= 1_000_000_000 {
        return Err(CaptureError::InvalidPayload(
            "Warp protobuf timestamp nanos are out of range".to_owned(),
        ));
    }
    Ok(seconds.and_then(|seconds| DateTime::<Utc>::from_timestamp(seconds, nanos)))
}

fn decode_system_query(data: &[u8]) -> Result<String> {
    let Some((field, payload)) = last_length_delimited_field(data)? else {
        return Ok("system query".to_owned());
    };
    Ok(match field {
        1 => "system query: auto code diff".to_owned(),
        3 => "system query: resume conversation".to_owned(),
        4 => "system query: generate passive suggestions".to_owned(),
        5 => decode_last_nested_text(payload, 1)?
            .map(|query| format!("system query: create new project\n{query}"))
            .unwrap_or_else(|| "system query: create new project".to_owned()),
        6 => "system query: clone repository".to_owned(),
        7 => decode_last_nested_text(payload, 1)?
            .map(|prompt| format!("system query: summarize conversation\n{prompt}"))
            .unwrap_or_else(|| "system query: summarize conversation".to_owned()),
        8 => "system query: fetch review comments".to_owned(),
        9 => "system query: handoff rehydration".to_owned(),
        _ => format!("system query: field {field}"),
    })
}

fn decode_summarization(data: &[u8]) -> Result<String> {
    let Some((field, payload)) = last_length_delimited_field(data)? else {
        return Err(CaptureError::InvalidPayload(
            "Warp summarization has no selected arm".to_owned(),
        ));
    };
    if field == 1 {
        return Ok(decode_last_nested_text(payload, 1)?
            .map(|summary| format!("conversation summary\n{summary}"))
            .unwrap_or_else(|| "conversation summary".to_owned()));
    }
    Ok(format!("summarization: field {field}"))
}

fn decode_received_messages(data: &[u8]) -> Result<String> {
    let mut cursor = WarpWireCursor::new(data);
    let mut parts = Vec::new();
    while let Some(field) = cursor.next()? {
        let (1, WarpWireValue::LengthDelimited(received)) = (field.number, field.value) else {
            continue;
        };
        let subject = decode_last_nested_text(received, 4)?.unwrap_or_default();
        let body = decode_last_nested_text(received, 5)?.unwrap_or_default();
        let text = match (subject.is_empty(), body.is_empty()) {
            (false, false) => format!("{subject}\n{body}"),
            (false, true) => subject,
            (true, false) => body,
            (true, true) => continue,
        };
        parts.push(text);
    }
    Ok(parts.join("\n\n"))
}

fn decode_last_nested_text(data: &[u8], field: u32) -> Result<Option<String>> {
    last_length_delimited_value(data, field)?
        .map(warp_text_owned)
        .transpose()
}

fn last_length_delimited_value(data: &[u8], desired_field: u32) -> Result<Option<&[u8]>> {
    let mut cursor = WarpWireCursor::new(data);
    let mut selected = None;
    while let Some(field) = cursor.next()? {
        if let (number, WarpWireValue::LengthDelimited(value)) = (field.number, field.value) {
            if number == desired_field {
                selected = Some(value);
            }
        }
    }
    Ok(selected)
}

fn last_length_delimited_field(data: &[u8]) -> Result<Option<(u32, &[u8])>> {
    let mut cursor = WarpWireCursor::new(data);
    let mut selected = None;
    while let Some(field) = cursor.next()? {
        if let WarpWireValue::LengthDelimited(value) = field.value {
            selected = Some((field.number, value));
        }
    }
    Ok(selected)
}

fn warp_text_owned(data: &[u8]) -> Result<String> {
    super::super::wire::warp_wire_text(data).map(str::to_owned)
}

fn bounded_linkage_owned(value: String) -> Option<String> {
    const MAX_LINKAGE_BYTES: usize = 16 * 1024;
    let value = value.trim();
    (!value.is_empty() && value.len() <= MAX_LINKAGE_BYTES).then(|| value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn textual_success_failure_and_unknown_results_are_complete() {
        for (tool_result, expected_outcome, expected_body) in [
            (
                shell_result(Some((5, nested_text("shell success"))), None),
                OutputOutcome::Success,
                "shell success",
            ),
            (
                shell_result(Some((6, nested_text("shell failure"))), None),
                OutputOutcome::Failure,
                "shell failure",
            ),
            (
                shell_result(None, Some(b"shell unknown")),
                OutputOutcome::Unknown,
                "shell unknown",
            ),
        ] {
            let decoded = decode_warp_native_task(&task_with_tool_result(tool_result)).unwrap();
            let WarpDecodedMessagePayload::Output(output) = &decoded.messages[0].payload else {
                panic!("textual Warp result was not retained");
            };
            assert_eq!(output.outcome, expected_outcome);
            assert_eq!(output.body, expected_body);
            assert_eq!(output.call_id.as_deref(), Some("call-1"));
        }
    }

    #[test]
    fn binary_and_status_only_results_remain_unsupported() {
        let binary = shell_result(None, Some(&[0xff, 0xfe]));
        let status_only = shell_result(Some((6, Vec::new())), None);
        for tool_result in [binary, status_only] {
            let decoded = decode_warp_native_task(&task_with_tool_result(tool_result)).unwrap();
            assert!(matches!(
                decoded.messages[0].payload,
                WarpDecodedMessagePayload::Excluded
            ));
        }
    }

    #[test]
    fn textual_result_larger_than_page_target_is_not_truncated() {
        let body = format!(
            "warp-large-head-{}-warp-large-tail",
            "x".repeat(8 * 1024 * 1024)
        );
        let tool_result = shell_result(Some((5, nested_text(&body))), None);
        let decoded = decode_warp_native_task(&task_with_tool_result(tool_result)).unwrap();
        let WarpDecodedMessagePayload::Output(output) = &decoded.messages[0].payload else {
            panic!("large textual Warp result was not retained");
        };
        assert_eq!(output.body, body);
    }

    fn task_with_tool_result(tool_result: Vec<u8>) -> Vec<u8> {
        let mut message = Vec::new();
        push_length_delimited(&mut message, 5, &tool_result);
        let mut task = Vec::new();
        push_length_delimited(&mut task, 5, &message);
        task
    }

    fn shell_result(terminal: Option<(u32, Vec<u8>)>, deprecated: Option<&[u8]>) -> Vec<u8> {
        let mut shell = Vec::new();
        if let Some(deprecated) = deprecated {
            push_length_delimited(&mut shell, 1, deprecated);
        }
        if let Some((field, payload)) = terminal {
            push_length_delimited(&mut shell, field, &payload);
        }
        let mut result = Vec::new();
        push_length_delimited(&mut result, 1, b"call-1");
        push_length_delimited(&mut result, 2, &shell);
        result
    }

    fn nested_text(text: &str) -> Vec<u8> {
        let mut nested = Vec::new();
        push_length_delimited(&mut nested, 1, text.as_bytes());
        nested
    }

    fn push_length_delimited(target: &mut Vec<u8>, field: u32, payload: &[u8]) {
        push_varint(target, u64::from(field) << 3 | 2);
        push_varint(target, u64::try_from(payload.len()).unwrap());
        target.extend_from_slice(payload);
    }

    fn push_varint(target: &mut Vec<u8>, mut value: u64) {
        while value >= 0x80 {
            target.push((value as u8 & 0x7f) | 0x80);
            value >>= 7;
        }
        target.push(value as u8);
    }
}
