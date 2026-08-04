mod mcp_response;
mod protobuf;

use mcp_response::decode_mcp_tool_result_response;
#[cfg(test)]
use protobuf::decode_protobuf_struct;
use protobuf::{
    bounded_exact_linkage_owned, bounded_linkage_owned, decode_last_nested_text_occurrences,
    decode_protobuf_struct_map, decode_received_messages_occurrences,
    decode_summarization_occurrences, decode_system_query_occurrences,
    decode_timestamp_occurrences, last_length_delimited_field_occurrences,
    last_length_delimited_value, last_length_delimited_value_occurrences, warp_text_owned,
};

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use ctx_history_core::{
    EventRole, EventType, McpJsonCapture, McpTerminalResponseContent, McpTerminalStatus,
    McpTextCapture, MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES,
};
use serde_json::{Map, Value};
use uuid::Uuid;

use super::super::wire::{
    is_warp_tool_arm, is_warp_tool_result_arm, warp_message_arm, warp_tool_name,
    warp_tool_result_name, WarpMessageArm, WarpWireCursor, WarpWireValue,
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
    result_call_id: Option<String>,
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
    pub(super) call_id: Option<String>,
    pub(super) mcp_invocation: Option<WarpMcpToolInvocation>,
}

#[derive(Clone, Debug)]
pub(super) struct WarpDecodedOutput {
    pub(super) call_id: Option<String>,
    pub(super) tool_name: &'static str,
    pub(super) outcome: OutputOutcome,
    pub(super) body: String,
    pub(super) mcp_invocation: Option<WarpMcpToolInvocation>,
    pub(super) mcp_response: Option<McpTerminalResponseContent>,
    result_kind: WarpToolResultKind,
}

#[derive(Clone, Debug, PartialEq)]
pub(in super::super) struct WarpMcpToolInvocation {
    pub(in super::super) server_id: String,
    pub(in super::super) tool_name: String,
    pub(in super::super) args: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WarpToolResultKind {
    Other,
    Mcp,
    Cancellation,
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
enum WarpClassifiedBody {
    Bytes(Option<Vec<u8>>),
    Owned(Option<String>),
    Malformed,
}

#[derive(Clone, Debug)]
struct WarpToolResultClassification {
    call_id: Option<String>,
    tool_name: &'static str,
    outcome: OutputOutcome,
    body: WarpClassifiedBody,
    result_kind: WarpToolResultKind,
    mcp_response: Option<McpTerminalResponseContent>,
}

#[derive(Debug)]
struct WarpSelectedMessage {
    field: u32,
    payloads: Vec<Vec<u8>>,
}

#[derive(Debug)]
struct WarpValidatedString {
    value: Option<String>,
    valid: bool,
}

impl Default for WarpValidatedString {
    fn default() -> Self {
        Self {
            value: None,
            valid: true,
        }
    }
}

impl WarpValidatedString {
    fn observe(&mut self, value: &[u8]) {
        match warp_text_owned(value) {
            Ok(value) if self.valid => self.value = Some(value),
            Ok(_) => {}
            Err(_) => {
                self.valid = false;
                self.value = None;
            }
        }
    }

    fn into_optional(self, label: &str) -> Result<Option<String>> {
        if self.valid {
            Ok(self.value)
        } else {
            Err(CaptureError::InvalidPayload(format!(
                "invalid UTF-8 in Warp protobuf {label}"
            )))
        }
    }
}

pub(super) fn decode_warp_native_task(data: &[u8]) -> Result<WarpDecodedTask> {
    let mut cursor = WarpWireCursor::new(data);
    let mut task_id = None::<String>;
    let mut message_payloads = Vec::new();
    let mut counters = WarpDecodeCounters::default();
    while let Some(field) = cursor.next()? {
        match (field.number, field.value) {
            (1, WarpWireValue::LengthDelimited(value)) => {
                task_id = Some(warp_text_owned(value)?);
            }
            (2 | 3 | 6, WarpWireValue::LengthDelimited(_)) => {}
            (5, WarpWireValue::LengthDelimited(value)) => message_payloads.push(value),
            _ => counters.unknown_fields = counters.unknown_fields.saturating_add(1),
        }
    }

    let _ = task_id;
    let mut messages = Vec::new();
    for (ordinal, message) in message_payloads.into_iter().enumerate() {
        let message_ordinal = u32::try_from(ordinal).map_err(|_| {
            CaptureError::InvalidPayload("Warp task has too many protobuf messages".to_owned())
        })?;
        let payload = decode_warp_native_message(message, message_ordinal, &mut counters)?;
        messages.push(payload);
    }
    link_mcp_tool_results(&mut messages);
    Ok(WarpDecodedTask { messages, counters })
}

fn decode_warp_native_message(
    data: &[u8],
    message_ordinal: u32,
    counters: &mut WarpDecodeCounters,
) -> Result<WarpDecodedMessage> {
    let mut cursor = WarpWireCursor::new(data);
    let mut message_id = None::<String>;
    let mut request_id = None::<String>;
    let mut timestamps = Vec::new();
    let mut selected_arm = None;
    while let Some(field) = cursor.next()? {
        match (field.number, field.value) {
            (1, WarpWireValue::LengthDelimited(value)) => {
                message_id = Some(warp_text_owned(value)?);
            }
            (11, WarpWireValue::LengthDelimited(_)) => {}
            (13, WarpWireValue::LengthDelimited(value)) => {
                request_id = Some(warp_text_owned(value)?);
            }
            (14, WarpWireValue::LengthDelimited(value)) => timestamps.push(value),
            (number, WarpWireValue::LengthDelimited(value))
                if warp_message_arm(number).is_some() =>
            {
                let arm = warp_message_arm(number).ok_or(CaptureError::SystemInvariant(
                    "Warp message arm classification changed during decode",
                ))?;
                if matches!(arm, WarpMessageArm::Unknown(_)) {
                    counters.unknown_fields = counters.unknown_fields.saturating_add(1);
                    counters.unknown_oneofs = counters.unknown_oneofs.saturating_add(1);
                } else {
                    validate_message_payload(value)?;
                }
                select_message_oneof(&mut selected_arm, number, value);
            }
            _ => counters.unknown_fields = counters.unknown_fields.saturating_add(1),
        }
    }

    let message_id = message_id.and_then(bounded_linkage_owned);
    let Some(selected_arm) = selected_arm else {
        return Ok(WarpDecodedMessage {
            message_ordinal,
            message_id,
            request_id: None,
            occurred_at: None,
            legacy_indexed: false,
            result_call_id: None,
            payload: WarpDecodedMessagePayload::Excluded,
        });
    };
    let arm = warp_message_arm(selected_arm.field).ok_or(CaptureError::SystemInvariant(
        "Warp selected message arm classification changed during decode",
    ))?;
    if matches!(arm, WarpMessageArm::Unknown(_)) {
        return Ok(WarpDecodedMessage {
            message_ordinal,
            message_id,
            request_id: None,
            occurred_at: None,
            legacy_indexed: false,
            result_call_id: None,
            payload: WarpDecodedMessagePayload::Excluded,
        });
    }
    if matches!(
        arm,
        WarpMessageArm::ToolResult | WarpMessageArm::DebugOutput
    ) {
        let result_call_id = matches!(arm, WarpMessageArm::ToolResult)
            .then(|| decode_tool_result_call_id(&selected_arm.payloads))
            .flatten();
        let decoded_payload = decode_output(arm, &selected_arm.payloads, counters)?;
        let needs_metadata = matches!(&decoded_payload, WarpDecodedMessagePayload::Output(_));
        let (request_id, occurred_at) = if needs_metadata {
            decode_output_metadata(request_id, &timestamps, counters)
        } else {
            (None, None)
        };
        return Ok(WarpDecodedMessage {
            message_ordinal,
            message_id,
            request_id,
            occurred_at,
            legacy_indexed: true,
            result_call_id,
            payload: decoded_payload,
        });
    }

    let request_id = request_id.and_then(bounded_linkage_owned);
    let occurred_at = decode_timestamp_occurrences(&timestamps)?;
    let payloads = &selected_arm.payloads;
    let (event_type, role, kind, body, tool_call, call_id, mcp_invocation) = match arm {
        WarpMessageArm::UserQuery => (
            EventType::Message,
            Some(EventRole::User),
            "user_query",
            decode_last_nested_text_occurrences(payloads, 1)?.unwrap_or_default(),
            false,
            None,
            None,
        ),
        WarpMessageArm::AgentOutput => (
            EventType::Message,
            Some(EventRole::Assistant),
            "agent_output",
            decode_last_nested_text_occurrences(payloads, 1)?.unwrap_or_default(),
            false,
            None,
            None,
        ),
        WarpMessageArm::ToolCall => {
            let decoded = decode_tool_call(payloads)?;
            (
                EventType::ToolCall,
                Some(EventRole::Assistant),
                "tool_call",
                format!("tool call: {}", decoded.tool_name),
                true,
                decoded.call_id,
                decoded.mcp_invocation,
            )
        }
        WarpMessageArm::SystemQuery => (
            EventType::Message,
            Some(EventRole::System),
            "system_query",
            decode_system_query_occurrences(payloads)?,
            false,
            None,
            None,
        ),
        WarpMessageArm::AgentReasoning => (
            EventType::Message,
            Some(EventRole::Assistant),
            "agent_reasoning",
            decode_last_nested_text_occurrences(payloads, 1)?.unwrap_or_default(),
            false,
            None,
            None,
        ),
        WarpMessageArm::Summarization => (
            EventType::Message,
            Some(EventRole::Assistant),
            "summarization",
            decode_summarization_occurrences(payloads)?,
            false,
            None,
            None,
        ),
        WarpMessageArm::ReceivedMessages => (
            EventType::Message,
            Some(EventRole::Assistant),
            "messages_received_from_agents",
            decode_received_messages_occurrences(payloads)?,
            false,
            None,
            None,
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
        result_call_id: None,
        payload: WarpDecodedMessagePayload::Retained(WarpRetainedMessage {
            event_type,
            role,
            kind,
            body,
            tool_call,
            call_id,
            mcp_invocation,
        }),
    })
}

fn decode_output_metadata(
    request_id: Option<String>,
    timestamps: &[&[u8]],
    counters: &mut WarpDecodeCounters,
) -> (Option<String>, Option<DateTime<Utc>>) {
    let request_id = request_id.and_then(bounded_linkage_owned);
    let occurred_at = decode_timestamp_occurrences(timestamps);
    if let Err(error) = &occurred_at {
        let _ = error;
        counters.malformed_output_records = counters.malformed_output_records.saturating_add(1);
    }
    (request_id, occurred_at.unwrap_or(None))
}

#[derive(Debug)]
struct WarpDecodedToolCall {
    call_id: Option<String>,
    tool_name: &'static str,
    mcp_invocation: Option<WarpMcpToolInvocation>,
}

fn decode_tool_call(payloads: &[Vec<u8>]) -> Result<WarpDecodedToolCall> {
    let mut call_id = WarpValidatedString::default();
    let mut selected_tool = None;
    for payload in payloads {
        let mut cursor = WarpWireCursor::new(payload);
        while let Some(field) = cursor.next()? {
            match (field.number, field.value) {
                (1, WarpWireValue::LengthDelimited(value)) => call_id.observe(value),
                (number, WarpWireValue::LengthDelimited(value)) if is_warp_tool_arm(number) => {
                    validate_message_payload(value)?;
                    select_message_oneof(&mut selected_tool, number, value);
                }
                _ => {}
            }
        }
    }
    let call_id = call_id
        .into_optional("ToolCall.tool_call_id")
        .ok()
        .flatten()
        .and_then(bounded_exact_linkage_owned);
    let tool_field = selected_tool.as_ref().map_or(0, |selected| selected.field);
    let mcp_invocation = (tool_field == 12)
        .then(|| {
            decode_mcp_tool_call(
                &selected_tool
                    .as_ref()
                    .ok_or(CaptureError::SystemInvariant(
                        "Warp MCP tool arm disappeared during decode",
                    ))?
                    .payloads,
            )
        })
        .transpose()
        .ok()
        .flatten();
    Ok(WarpDecodedToolCall {
        call_id,
        tool_name: warp_tool_name(tool_field),
        mcp_invocation,
    })
}

fn decode_mcp_tool_call(payloads: &[Vec<u8>]) -> Result<WarpMcpToolInvocation> {
    let mut name = WarpValidatedString::default();
    let mut args = None::<Map<String, Value>>;
    let mut server_id = WarpValidatedString::default();
    for payload in payloads {
        let mut cursor = WarpWireCursor::new(payload);
        while let Some(field) = cursor.next()? {
            match (field.number, field.value) {
                (1, WarpWireValue::LengthDelimited(value)) => name.observe(value),
                (2, WarpWireValue::LengthDelimited(value)) => {
                    let occurrence = decode_protobuf_struct_map(value, 0)?;
                    args.get_or_insert_with(Map::new).extend(occurrence);
                }
                (3, WarpWireValue::LengthDelimited(value)) => server_id.observe(value),
                _ => {}
            }
        }
    }
    Ok(WarpMcpToolInvocation {
        server_id: server_id
            .into_optional("CallMCPTool.server_id")?
            .unwrap_or_default(),
        tool_name: name.into_optional("CallMCPTool.name")?.unwrap_or_default(),
        args: Value::Object(args.ok_or_else(|| {
            CaptureError::InvalidPayload("Warp CallMCPTool.args is missing".to_owned())
        })?),
    })
}

fn link_mcp_tool_results(messages: &mut [WarpDecodedMessage]) {
    #[derive(Default)]
    struct CallLink {
        count: usize,
        invocation: Option<WarpMcpToolInvocation>,
    }

    #[derive(Default)]
    struct ResultLink {
        count: usize,
        exact_terminal: bool,
    }

    let mut calls = HashMap::<String, CallLink>::new();
    let mut results = HashMap::<String, ResultLink>::new();
    for message in messages.iter() {
        match &message.payload {
            WarpDecodedMessagePayload::Retained(retained) if retained.tool_call => {
                let Some(call_id) = retained.call_id.as_ref() else {
                    continue;
                };
                let entry = calls.entry(call_id.clone()).or_default();
                entry.count = entry.count.saturating_add(1);
                if entry.count == 1 {
                    entry.invocation = retained.mcp_invocation.clone();
                } else {
                    entry.invocation = None;
                }
            }
            _ => {}
        }
        if let Some(call_id) = message.result_call_id.as_ref() {
            let exact_terminal = matches!(
                &message.payload,
                WarpDecodedMessagePayload::Output(output)
                    if matches!(
                        output.result_kind,
                        WarpToolResultKind::Mcp | WarpToolResultKind::Cancellation
                    ) && output.mcp_response.is_some()
            );
            let link = results.entry(call_id.clone()).or_default();
            link.count = link.count.saturating_add(1);
            if link.count == 1 {
                link.exact_terminal = exact_terminal;
            } else {
                link.exact_terminal = false;
            }
        }
    }

    for message in messages {
        match &mut message.payload {
            WarpDecodedMessagePayload::Retained(retained) if retained.tool_call => {
                let qualified = retained.call_id.as_ref().and_then(|call_id| {
                    let call = calls.get(call_id)?;
                    let result = results.get(call_id)?;
                    let invocation = call.invocation.as_ref()?;
                    (call.count == 1
                        && result.count == 1
                        && result.exact_terminal
                        && qualifies_mcp_invocation(invocation))
                    .then(|| invocation.clone())
                });
                retained.mcp_invocation = qualified;
            }
            WarpDecodedMessagePayload::Output(output)
                if matches!(
                    output.result_kind,
                    WarpToolResultKind::Mcp | WarpToolResultKind::Cancellation
                ) =>
            {
                let qualified = output.call_id.as_ref().and_then(|call_id| {
                    let call = calls.get(call_id)?;
                    let result = results.get(call_id)?;
                    let invocation = call.invocation.as_ref()?;
                    (call.count == 1
                        && result.count == 1
                        && result.exact_terminal
                        && qualifies_mcp_invocation(invocation))
                    .then(|| invocation.clone())
                });
                output.mcp_invocation = qualified;
                if output.mcp_invocation.is_none() {
                    output.mcp_response = None;
                }
            }
            _ => {}
        }
    }
}

fn qualifies_mcp_invocation(invocation: &WarpMcpToolInvocation) -> bool {
    Uuid::parse_str(&invocation.server_id).is_ok()
        && !invocation.tool_name.is_empty()
        && invocation.tool_name.len() <= MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES
}

fn decode_tool_result_call_id(payloads: &[Vec<u8>]) -> Option<String> {
    let mut call_id = WarpValidatedString::default();
    for payload in payloads {
        let mut cursor = WarpWireCursor::new(payload);
        while let Ok(Some(field)) = cursor.next() {
            if let (1, WarpWireValue::LengthDelimited(value)) = (field.number, field.value) {
                call_id.observe(value);
            }
        }
    }
    call_id
        .into_optional("ToolCallResult.tool_call_id")
        .ok()
        .flatten()
        .and_then(bounded_exact_linkage_owned)
}

fn select_message_oneof(selected: &mut Option<WarpSelectedMessage>, field: u32, payload: &[u8]) {
    if let Some(selected) = selected {
        if selected.field == field {
            selected.payloads.push(payload.to_vec());
            return;
        }
    }
    *selected = Some(WarpSelectedMessage {
        field,
        payloads: vec![payload.to_vec()],
    });
}

fn validate_message_payload(payload: &[u8]) -> Result<()> {
    let mut cursor = WarpWireCursor::new(payload);
    while cursor.next()?.is_some() {}
    Ok(())
}

fn decode_output(
    arm: WarpMessageArm,
    payloads: &[Vec<u8>],
    counters: &mut WarpDecodeCounters,
) -> Result<WarpDecodedMessagePayload> {
    counters.native_result_records = counters.native_result_records.saturating_add(1);
    counters.native_result_envelope_bytes = counters.native_result_envelope_bytes.saturating_add(
        payloads.iter().fold(0_u64, |total, payload| {
            total.saturating_add(u64::try_from(payload.len()).unwrap_or(u64::MAX))
        }),
    );
    let classification = match arm {
        WarpMessageArm::ToolResult => match classify_tool_result(payloads) {
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
            u64::try_from(body.as_ref().map_or(0, Vec::len)).unwrap_or(u64::MAX)
        }
        WarpClassifiedBody::Owned(body) => {
            u64::try_from(body.as_ref().map_or(0, String::len)).unwrap_or(u64::MAX)
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
    let terminal_without_text = matches!(
        classification.result_kind,
        WarpToolResultKind::Mcp | WarpToolResultKind::Cancellation
    );
    let body = match classification.body {
        WarpClassifiedBody::Bytes(Some(body)) => match warp_text_owned(&body) {
            Ok(body) if !body.trim().is_empty() => body,
            Ok(_) if terminal_without_text => String::new(),
            Ok(_) | Err(_) => {
                counters.malformed_output_records =
                    counters.malformed_output_records.saturating_add(1);
                return Ok(WarpDecodedMessagePayload::Excluded);
            }
        },
        WarpClassifiedBody::Owned(Some(body)) if !body.trim().is_empty() => body,
        WarpClassifiedBody::Owned(_) | WarpClassifiedBody::Bytes(None) if terminal_without_text => {
            String::new()
        }
        WarpClassifiedBody::Bytes(None)
        | WarpClassifiedBody::Owned(_)
        | WarpClassifiedBody::Malformed => {
            counters.malformed_output_records = counters.malformed_output_records.saturating_add(1);
            return Ok(WarpDecodedMessagePayload::Excluded);
        }
    };
    Ok(WarpDecodedMessagePayload::Output(WarpDecodedOutput {
        call_id: classification.call_id,
        tool_name: classification.tool_name,
        outcome: classification.outcome,
        body,
        mcp_invocation: None,
        mcp_response: classification.mcp_response,
        result_kind: classification.result_kind,
    }))
}

fn classify_tool_result(payloads: &[Vec<u8>]) -> Result<WarpToolResultClassification> {
    let mut call_id = WarpValidatedString::default();
    let mut variant = None;
    for payload in payloads {
        let mut cursor = WarpWireCursor::new(payload);
        while let Some(field) = cursor.next()? {
            match (field.number, field.value) {
                (1, WarpWireValue::LengthDelimited(value)) => call_id.observe(value),
                (1 | 11, _) => {}
                (number, WarpWireValue::LengthDelimited(value))
                    if is_warp_tool_result_arm(number) =>
                {
                    validate_message_payload(value)?;
                    select_message_oneof(&mut variant, number, value);
                }
                _ => {}
            }
        }
    }
    let call_id = call_id
        .into_optional("ToolCallResult.tool_call_id")
        .ok()
        .flatten()
        .and_then(bounded_exact_linkage_owned);
    let Some(selected) = variant else {
        return Ok(WarpToolResultClassification {
            call_id,
            tool_name: warp_tool_result_name(0),
            outcome: OutputOutcome::Unknown,
            body: WarpClassifiedBody::Bytes(None),
            result_kind: WarpToolResultKind::Other,
            mcp_response: None,
        });
    };
    let variant = selected.field;
    if variant == 14 {
        return Ok(WarpToolResultClassification {
            call_id,
            tool_name: warp_tool_result_name(variant),
            outcome: OutputOutcome::Unknown,
            body: WarpClassifiedBody::Bytes(None),
            result_kind: WarpToolResultKind::Cancellation,
            mcp_response: Some(McpTerminalResponseContent {
                status: McpTerminalStatus::Cancelled,
                failure_kind: None,
                duration_ns: None,
                text: McpTextCapture::Absent,
                payload: McpJsonCapture::Absent,
            }),
        });
    }
    if variant == 16 {
        let Some(decoded) = decode_mcp_tool_result_response(&selected.payloads)? else {
            return Ok(WarpToolResultClassification {
                call_id,
                tool_name: warp_tool_result_name(variant),
                outcome: OutputOutcome::Unknown,
                body: WarpClassifiedBody::Bytes(None),
                result_kind: WarpToolResultKind::Other,
                mcp_response: None,
            });
        };
        return Ok(WarpToolResultClassification {
            call_id,
            tool_name: warp_tool_result_name(variant),
            outcome: decoded.outcome,
            body: WarpClassifiedBody::Owned(decoded.body),
            result_kind: WarpToolResultKind::Mcp,
            mcp_response: Some(decoded.response),
        });
    }
    if variant == 2 {
        let (outcome, body) = classify_run_shell_result_occurrences(&selected.payloads)?;
        return Ok(WarpToolResultClassification {
            call_id,
            tool_name: warp_tool_result_name(variant),
            outcome,
            body,
            result_kind: WarpToolResultKind::Other,
            mcp_response: None,
        });
    }
    let selected_field = last_length_delimited_field_occurrences(&selected.payloads)?;
    let selected_field_number = selected_field.map_or(0, |(field, _)| field);
    let outcome = match (variant, selected_field_number) {
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
    let body = classified_result_body(variant, selected_field);
    Ok(WarpToolResultClassification {
        call_id,
        tool_name: warp_tool_result_name(variant),
        outcome,
        body,
        result_kind: WarpToolResultKind::Other,
        mcp_response: None,
    })
}

fn classify_run_shell_result_occurrences(
    payloads: &[Vec<u8>],
) -> Result<(OutputOutcome, WarpClassifiedBody)> {
    let mut deprecated_output = None;
    let mut terminal = None;
    for payload in payloads {
        let mut cursor = WarpWireCursor::new(payload);
        while let Some(field) = cursor.next()? {
            match (field.number, field.value) {
                (1, WarpWireValue::LengthDelimited(value)) => {
                    deprecated_output = Some(value.to_vec());
                }
                (4..=6, WarpWireValue::LengthDelimited(value)) => {
                    validate_message_payload(value)?;
                    select_message_oneof(&mut terminal, field.number, value);
                }
                _ => {}
            }
        }
    }
    let Some(terminal) = terminal else {
        return Ok((
            OutputOutcome::Unknown,
            WarpClassifiedBody::Bytes(deprecated_output),
        ));
    };
    let body = match terminal.field {
        4 | 5 => classified_nested_text_occurrences(&terminal.payloads, 1),
        6 => match classified_nested_text_occurrences(&terminal.payloads, 1) {
            WarpClassifiedBody::Bytes(None) => WarpClassifiedBody::Bytes(deprecated_output),
            body => body,
        },
        _ => WarpClassifiedBody::Bytes(None),
    };
    let outcome = match terminal.field {
        5 => OutputOutcome::Success,
        6 => OutputOutcome::Failure,
        _ => OutputOutcome::Unknown,
    };
    Ok((outcome, body))
}

fn classified_result_body(variant: u32, selected: Option<(u32, &[u8])>) -> WarpClassifiedBody {
    let Some((arm, arm_payload)) = selected else {
        return WarpClassifiedBody::Bytes(None);
    };
    match (variant, arm) {
        (4 | 23, 1) => WarpClassifiedBody::Bytes(Some(arm_payload.to_vec())),
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

fn classified_nested_text(data: &[u8], field: u32) -> WarpClassifiedBody {
    match last_length_delimited_value(data, field) {
        Ok(value) => WarpClassifiedBody::Bytes(value.map(<[u8]>::to_vec)),
        Err(_) => WarpClassifiedBody::Malformed,
    }
}

fn classified_nested_text_occurrences(payloads: &[Vec<u8>], field: u32) -> WarpClassifiedBody {
    match last_length_delimited_value_occurrences(payloads, field) {
        Ok(value) => WarpClassifiedBody::Bytes(value.map(<[u8]>::to_vec)),
        Err(_) => WarpClassifiedBody::Malformed,
    }
}

#[cfg(test)]
mod tests;
