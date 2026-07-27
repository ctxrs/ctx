use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType};

use crate::{CaptureError, Result};

#[derive(Debug, Clone, Default)]
pub(super) struct WarpTaskProto {
    pub(super) id: String,
    pub(super) description: String,
    pub(super) parent_task_id: Option<String>,
    pub(super) summary: String,
    pub(super) messages: Vec<WarpMessageProto>,
}

#[derive(Debug, Clone)]
pub(super) struct WarpMessageProto {
    pub(super) id: String,
    pub(super) task_id: String,
    pub(super) request_id: String,
    pub(super) timestamp: Option<DateTime<Utc>>,
    pub(super) kind: &'static str,
    pub(super) role: Option<EventRole>,
    pub(super) event_type: EventType,
    pub(super) text: String,
    /// Exact provider-backed text for source reopening. Synthetic labels are
    /// deliberately excluded even when they remain useful as indexed event
    /// descriptions.
    pub(super) complete_text: Option<String>,
}

impl Default for WarpMessageProto {
    fn default() -> Self {
        Self {
            id: String::new(),
            task_id: String::new(),
            request_id: String::new(),
            timestamp: None,
            kind: "unknown",
            role: None,
            event_type: EventType::Notice,
            text: String::new(),
            complete_text: None,
        }
    }
}

pub(super) fn warp_decode_task(data: &[u8]) -> Result<WarpTaskProto> {
    let mut task = WarpTaskProto::default();
    let mut pos = 0;
    while pos < data.len() {
        let (field, wire) = proto_key(data, &mut pos)?;
        match (field, wire) {
            (1, 2) => task.id = proto_string(data, &mut pos)?,
            (2, 2) => task.description = proto_string(data, &mut pos)?,
            (3, 2) => task.parent_task_id = warp_decode_dependencies(proto_len(data, &mut pos)?)?,
            (5, 2) => task
                .messages
                .push(warp_decode_message(proto_len(data, &mut pos)?)?),
            (6, 2) => task.summary = proto_string(data, &mut pos)?,
            _ => proto_skip(data, &mut pos, wire)?,
        }
    }
    Ok(task)
}

pub(super) fn warp_decode_dependencies(data: &[u8]) -> Result<Option<String>> {
    let mut pos = 0;
    let mut parent = None;
    while pos < data.len() {
        let (field, wire) = proto_key(data, &mut pos)?;
        match (field, wire) {
            (1, 2) => {
                let value = proto_string(data, &mut pos)?;
                if !value.is_empty() {
                    parent = Some(value);
                }
            }
            _ => proto_skip(data, &mut pos, wire)?,
        }
    }
    Ok(parent)
}

pub(super) fn warp_decode_message(data: &[u8]) -> Result<WarpMessageProto> {
    let mut message = WarpMessageProto::default();
    let mut pos = 0;
    while pos < data.len() {
        let (field, wire) = proto_key(data, &mut pos)?;
        match (field, wire) {
            (1, 2) => message.id = proto_string(data, &mut pos)?,
            (11, 2) => message.task_id = proto_string(data, &mut pos)?,
            (13, 2) => message.request_id = proto_string(data, &mut pos)?,
            (14, 2) => message.timestamp = warp_decode_timestamp(proto_len(data, &mut pos)?)?,
            (2, 2) => {
                message.kind = "user_query";
                message.role = Some(EventRole::User);
                message.event_type = EventType::Message;
                message.text =
                    proto_nested_string_field(proto_len(data, &mut pos)?, 1)?.unwrap_or_default();
                message.complete_text = Some(message.text.clone());
            }
            (3, 2) => {
                message.kind = "agent_output";
                message.role = Some(EventRole::Assistant);
                message.event_type = EventType::Message;
                message.text =
                    proto_nested_string_field(proto_len(data, &mut pos)?, 1)?.unwrap_or_default();
                message.complete_text = Some(message.text.clone());
            }
            (4, 2) => {
                let tool_name =
                    warp_tool_name(proto_first_len_field(proto_len(data, &mut pos)?)?.unwrap_or(0));
                message.kind = "tool_call";
                message.role = Some(EventRole::Assistant);
                message.event_type = EventType::ToolCall;
                message.text = format!("tool call: {tool_name}");
            }
            (5, 2) => {
                let result = proto_len(data, &mut pos)?;
                let tool_name = warp_tool_result_name(warp_tool_result_field(result)?.unwrap_or(0));
                message.kind = "tool_call_result";
                message.role = Some(EventRole::Tool);
                message.event_type = EventType::ToolOutput;
                message.complete_text = warp_decode_tool_result_text(result)?;
                message.text = message
                    .complete_text
                    .clone()
                    .unwrap_or_else(|| format!("tool result: {tool_name}"));
            }
            (9, 2) => {
                message.kind = "system_query";
                message.role = Some(EventRole::System);
                message.event_type = EventType::Message;
                message.text = warp_decode_system_query(proto_len(data, &mut pos)?)?;
                message.complete_text = Some(message.text.clone());
            }
            (15, 2) => {
                message.kind = "agent_reasoning";
                message.role = Some(EventRole::Assistant);
                message.event_type = EventType::Message;
                message.text =
                    proto_nested_string_field(proto_len(data, &mut pos)?, 1)?.unwrap_or_default();
                message.complete_text = Some(message.text.clone());
            }
            (16, 2) => {
                message.kind = "summarization";
                message.role = Some(EventRole::Assistant);
                message.event_type = EventType::Message;
                message.text = warp_decode_summarization(proto_len(data, &mut pos)?)?;
                message.complete_text = Some(message.text.clone());
            }
            (21, 2) => {
                message.kind = "debug_output";
                message.event_type = EventType::Notice;
                message.text = "debug output".to_owned();
                proto_skip(data, &mut pos, wire)?;
            }
            (24, 2) => {
                message.kind = "messages_received_from_agents";
                message.role = Some(EventRole::Assistant);
                message.event_type = EventType::Message;
                message.text = warp_decode_received_messages(proto_len(data, &mut pos)?)?;
                message.complete_text = Some(message.text.clone());
            }
            _ => proto_skip(data, &mut pos, wire)?,
        }
    }
    Ok(message)
}

/// Extracts one exact textual result body from Warp's typed protobuf result.
///
/// This intentionally supports only variants whose provider schema exposes a
/// single authoritative text payload. Status-only, compound, image, and
/// binary variants retain their typed event but do not receive a content
/// locator. Unknown fields are skipped by the protobuf rules and can never be
/// mistaken for result text.
pub(super) fn warp_decode_tool_result_text(data: &[u8]) -> Result<Option<String>> {
    let mut pos = 0;
    while pos < data.len() {
        let (field, wire) = proto_key(data, &mut pos)?;
        match (field, wire) {
            // RunShellCommandResult.
            (2, 2) => return warp_decode_run_shell_result(proto_len(data, &mut pos)?),
            // ServerResult.serialized_result.
            (4, 2) => return nonempty(proto_nested_string_field(proto_len(data, &mut pos)?, 1)?),
            // Deprecated FileGlobResult and FileGlobV2/error-shaped results.
            (10, 2) => return warp_decode_success_or_error_text(proto_len(data, &mut pos)?, 1),
            // WriteToLongRunningShellCommandResult.
            (17, 2) | (35, 2) => {
                return warp_decode_shell_wrapper_result(proto_len(data, &mut pos)?, 1, 2)
            }
            // SubagentResult.payload.
            (23, 2) => return nonempty(proto_nested_string_field(proto_len(data, &mut pos)?, 1)?),
            // ReadShellCommandOutputResult.
            (27, 2) => return warp_decode_shell_wrapper_result(proto_len(data, &mut pos)?, 2, 3),
            // Result messages whose error arm is field 2 and carries
            // Error.message at field 1. Success arms are compound or
            // status-only and are not flattened into plausible text.
            (
                3 | 5 | 6 | 9 | 15 | 16 | 19 | 24 | 25 | 26 | 28 | 30 | 32 | 34 | 36 | 38 | 41 | 42,
                2,
            ) => return warp_decode_error_text(proto_len(data, &mut pos)?),
            // InsertReviewCommentsResult and RequestComputerUseResult use
            // field 3 for their Error arm.
            (29 | 31, 2) => return warp_decode_nested_arm_text(proto_len(data, &mut pos)?, 3, 1),
            // RunAgentsResult has two authoritative textual terminal arms.
            (39, 2) => return warp_decode_run_agents_result(proto_len(data, &mut pos)?),
            _ => proto_skip(data, &mut pos, wire)?,
        }
    }
    Ok(None)
}

fn warp_decode_run_agents_result(data: &[u8]) -> Result<Option<String>> {
    let mut pos = 0;
    while pos < data.len() {
        let (field, wire) = proto_key(data, &mut pos)?;
        match (field, wire) {
            // Denied.reason or Failure.error.
            (2 | 3, 2) => {
                return nonempty(proto_nested_string_field(proto_len(data, &mut pos)?, 1)?)
            }
            _ => proto_skip(data, &mut pos, wire)?,
        }
    }
    Ok(None)
}

fn warp_decode_run_shell_result(data: &[u8]) -> Result<Option<String>> {
    let mut pos = 0;
    let mut deprecated_output = None;
    while pos < data.len() {
        let (field, wire) = proto_key(data, &mut pos)?;
        match (field, wire) {
            (1, 2) => deprecated_output = nonempty(Some(proto_string(data, &mut pos)?))?,
            // LongRunningShellCommandSnapshot.output.
            (4, 2) => return nonempty(proto_nested_string_field(proto_len(data, &mut pos)?, 1)?),
            // ShellCommandFinished.output.
            (5, 2) => return nonempty(proto_nested_string_field(proto_len(data, &mut pos)?, 1)?),
            _ => proto_skip(data, &mut pos, wire)?,
        }
    }
    Ok(deprecated_output)
}

fn warp_decode_shell_wrapper_result(
    data: &[u8],
    snapshot_field: u32,
    finished_field: u32,
) -> Result<Option<String>> {
    let mut pos = 0;
    while pos < data.len() {
        let (field, wire) = proto_key(data, &mut pos)?;
        match (field, wire) {
            (field, 2) if field == snapshot_field || field == finished_field => {
                return nonempty(proto_nested_string_field(proto_len(data, &mut pos)?, 1)?)
            }
            _ => proto_skip(data, &mut pos, wire)?,
        }
    }
    Ok(None)
}

fn warp_decode_error_text(data: &[u8]) -> Result<Option<String>> {
    warp_decode_nested_arm_text(data, 2, 1)
}

fn warp_decode_success_or_error_text(
    data: &[u8],
    success_text_field: u32,
) -> Result<Option<String>> {
    let mut pos = 0;
    while pos < data.len() {
        let (field, wire) = proto_key(data, &mut pos)?;
        match (field, wire) {
            (1, 2) => {
                return nonempty(proto_nested_string_field(
                    proto_len(data, &mut pos)?,
                    success_text_field,
                )?)
            }
            (2, 2) => return nonempty(proto_nested_string_field(proto_len(data, &mut pos)?, 1)?),
            _ => proto_skip(data, &mut pos, wire)?,
        }
    }
    Ok(None)
}

fn warp_decode_nested_arm_text(
    data: &[u8],
    arm_field: u32,
    text_field: u32,
) -> Result<Option<String>> {
    let mut pos = 0;
    while pos < data.len() {
        let (field, wire) = proto_key(data, &mut pos)?;
        match (field, wire) {
            (field, 2) if field == arm_field => {
                return nonempty(proto_nested_string_field(
                    proto_len(data, &mut pos)?,
                    text_field,
                )?)
            }
            _ => proto_skip(data, &mut pos, wire)?,
        }
    }
    Ok(None)
}

fn nonempty(value: Option<String>) -> Result<Option<String>> {
    Ok(value.filter(|text| !text.is_empty()))
}

pub(super) fn warp_decode_timestamp(data: &[u8]) -> Result<Option<DateTime<Utc>>> {
    let mut pos = 0;
    let mut seconds = None;
    let mut nanos = 0u32;
    while pos < data.len() {
        let (field, wire) = proto_key(data, &mut pos)?;
        match (field, wire) {
            (1, 0) => seconds = Some(proto_varint(data, &mut pos)? as i64),
            (2, 0) => nanos = proto_varint(data, &mut pos)? as u32,
            _ => proto_skip(data, &mut pos, wire)?,
        }
    }
    Ok(seconds.and_then(|secs| DateTime::<Utc>::from_timestamp(secs, nanos)))
}

pub(super) fn warp_decode_system_query(data: &[u8]) -> Result<String> {
    let Some(field) = proto_first_len_field(data)? else {
        return Ok("system query".to_owned());
    };
    Ok(match field {
        1 => "system query: auto code diff".to_owned(),
        3 => "system query: resume conversation".to_owned(),
        4 => "system query: generate passive suggestions".to_owned(),
        5 => proto_nested_string_field_for_oneof(data, 5, 1)?
            .map(|query| format!("system query: create new project\n{query}"))
            .unwrap_or_else(|| "system query: create new project".to_owned()),
        6 => "system query: clone repository".to_owned(),
        7 => proto_nested_string_field_for_oneof(data, 7, 1)?
            .map(|prompt| format!("system query: summarize conversation\n{prompt}"))
            .unwrap_or_else(|| "system query: summarize conversation".to_owned()),
        8 => "system query: fetch review comments".to_owned(),
        9 => "system query: handoff rehydration".to_owned(),
        _ => format!("system query: field {field}"),
    })
}

pub(super) fn warp_decode_summarization(data: &[u8]) -> Result<String> {
    proto_nested_string_field_for_oneof(data, 1, 1)?
        .map(|summary| format!("conversation summary\n{summary}"))
        .or_else(|| {
            proto_first_len_field(data)
                .ok()
                .flatten()
                .map(|field| format!("summarization: field {field}"))
        })
        .ok_or_else(|| CaptureError::InvalidPayload("Warp summarization has no summary".into()))
}

pub(super) fn warp_decode_received_messages(data: &[u8]) -> Result<String> {
    let mut pos = 0;
    let mut parts = Vec::new();
    while pos < data.len() {
        let (field, wire) = proto_key(data, &mut pos)?;
        match (field, wire) {
            (1, 2) => {
                let received = proto_len(data, &mut pos)?;
                let subject = proto_nested_string_field(received, 4)?.unwrap_or_default();
                let body = proto_nested_string_field(received, 5)?.unwrap_or_default();
                let text = if subject.is_empty() {
                    body
                } else if body.is_empty() {
                    subject
                } else {
                    format!("{subject}\n{body}")
                };
                if !text.is_empty() {
                    parts.push(text);
                }
            }
            _ => proto_skip(data, &mut pos, wire)?,
        }
    }
    Ok(parts.join("\n\n"))
}

pub(super) fn proto_nested_string_field_for_oneof(
    data: &[u8],
    outer_field: u32,
    inner_field: u32,
) -> Result<Option<String>> {
    let mut pos = 0;
    while pos < data.len() {
        let (field, wire) = proto_key(data, &mut pos)?;
        match (field, wire) {
            (field, 2) if field == outer_field => {
                return proto_nested_string_field(proto_len(data, &mut pos)?, inner_field);
            }
            _ => proto_skip(data, &mut pos, wire)?,
        }
    }
    Ok(None)
}

pub(super) fn proto_nested_string_field(data: &[u8], desired_field: u32) -> Result<Option<String>> {
    let mut pos = 0;
    while pos < data.len() {
        let (field, wire) = proto_key(data, &mut pos)?;
        match (field, wire) {
            (field, 2) if field == desired_field => return Ok(Some(proto_string(data, &mut pos)?)),
            _ => proto_skip(data, &mut pos, wire)?,
        }
    }
    Ok(None)
}

pub(super) fn proto_first_len_field(data: &[u8]) -> Result<Option<u32>> {
    let mut pos = 0;
    while pos < data.len() {
        let (field, wire) = proto_key(data, &mut pos)?;
        if wire == 2 {
            return Ok(Some(field));
        }
        proto_skip(data, &mut pos, wire)?;
    }
    Ok(None)
}

fn warp_tool_result_field(data: &[u8]) -> Result<Option<u32>> {
    let mut pos = 0;
    while pos < data.len() {
        let (field, wire) = proto_key(data, &mut pos)?;
        if wire == 2 && !matches!(field, 1 | 11) {
            return Ok(Some(field));
        }
        proto_skip(data, &mut pos, wire)?;
    }
    Ok(None)
}

pub(super) fn proto_key(data: &[u8], pos: &mut usize) -> Result<(u32, u8)> {
    let key = proto_varint(data, pos)?;
    Ok(((key >> 3) as u32, (key & 0x07) as u8))
}

pub(super) fn proto_string(data: &[u8], pos: &mut usize) -> Result<String> {
    let bytes = proto_len(data, pos)?;
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|err| {
            CaptureError::InvalidPayload(format!("invalid UTF-8 in Warp protobuf: {err}"))
        })
}

pub(super) fn proto_len<'a>(data: &'a [u8], pos: &mut usize) -> Result<&'a [u8]> {
    let len = proto_varint(data, pos)? as usize;
    let end = pos.checked_add(len).ok_or_else(|| {
        CaptureError::InvalidPayload("overflow while decoding Warp protobuf".into())
    })?;
    if end > data.len() {
        return Err(CaptureError::InvalidPayload(
            "truncated length-delimited field in Warp protobuf".into(),
        ));
    }
    let bytes = &data[*pos..end];
    *pos = end;
    Ok(bytes)
}

pub(super) fn proto_varint(data: &[u8], pos: &mut usize) -> Result<u64> {
    let mut value = 0u64;
    for shift in (0..70).step_by(7) {
        if *pos >= data.len() {
            return Err(CaptureError::InvalidPayload(
                "truncated varint in Warp protobuf".into(),
            ));
        }
        let byte = data[*pos];
        *pos += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(CaptureError::InvalidPayload(
        "oversized varint in Warp protobuf".into(),
    ))
}

pub(super) fn proto_skip(data: &[u8], pos: &mut usize, wire: u8) -> Result<()> {
    match wire {
        0 => {
            let _ = proto_varint(data, pos)?;
        }
        1 => {
            *pos = pos.checked_add(8).ok_or_else(|| {
                CaptureError::InvalidPayload("overflow while skipping fixed64".into())
            })?;
        }
        2 => {
            let _ = proto_len(data, pos)?;
        }
        5 => {
            *pos = pos.checked_add(4).ok_or_else(|| {
                CaptureError::InvalidPayload("overflow while skipping fixed32".into())
            })?;
        }
        other => {
            return Err(CaptureError::InvalidPayload(format!(
                "unsupported Warp protobuf wire type {other}"
            )));
        }
    }
    if *pos > data.len() {
        return Err(CaptureError::InvalidPayload(
            "truncated field while skipping Warp protobuf".into(),
        ));
    }
    Ok(())
}

pub(super) fn warp_tool_name(field: u32) -> &'static str {
    match field {
        2 => "run_shell_command",
        3 => "search_codebase",
        5 => "read_files",
        6 => "apply_file_diffs",
        7 => "suggest_plan",
        8 => "suggest_create_plan",
        9 => "grep",
        11 => "read_mcp_resource",
        12 => "call_mcp_tool",
        13 => "write_to_long_running_shell_command",
        14 => "suggest_new_conversation",
        15 => "file_glob",
        17 => "open_code_review",
        18 => "init_project",
        19 => "subagent",
        20 => "read_documents",
        21 => "edit_documents",
        22 => "create_documents",
        23 => "read_shell_command_output",
        24 => "use_computer",
        26 => "read_skill",
        28 => "fetch_conversation",
        29 => "start_agent",
        30 => "send_message_to_agent",
        31 => "transfer_shell_command_control_to_user",
        _ => "unknown",
    }
}

pub(super) fn warp_tool_result_name(field: u32) -> &'static str {
    match field {
        2 => "run_shell_command",
        3 => "search_codebase",
        4 => "server",
        5 => "read_files",
        6 => "apply_file_diffs",
        7 => "suggest_plan",
        8 => "suggest_create_plan",
        9 => "grep",
        10 => "file_glob",
        14 => "cancel",
        15 => "read_mcp_resource",
        16 => "call_mcp_tool",
        17 => "write_to_long_running_shell_command",
        18 => "suggest_new_conversation",
        19 => "file_glob_v2",
        20 => "suggest_prompt",
        21 => "open_code_review",
        22 => "init_project",
        23 => "subagent",
        24 => "read_documents",
        25 => "edit_documents",
        26 => "create_documents",
        27 => "read_shell_command_output",
        28 => "use_computer",
        29 => "insert_review_comments",
        30 => "read_skill",
        31 => "request_computer_use",
        32 => "fetch_conversation",
        33 => "start_agent",
        34 => "send_message_to_agent",
        35 => "transfer_shell_command_control_to_user",
        36 => "ask_user_question",
        38 => "upload_file_artifact",
        39 => "run_agents",
        40 => "wait_for_events",
        41 => "start_recording",
        42 => "stop_recording",
        _ => "unknown",
    }
}

#[cfg(test)]
#[path = "proto_tests.rs"]
mod tests;
