use crate::{CaptureError, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WarpMessageArm {
    UserQuery,
    AgentOutput,
    ToolCall,
    ToolResult,
    SystemQuery,
    AgentReasoning,
    Summarization,
    Unknown(u32),
    DebugOutput,
    ReceivedMessages,
}

pub(super) fn warp_message_arm(field: u32) -> Option<WarpMessageArm> {
    match field {
        2 => Some(WarpMessageArm::UserQuery),
        3 => Some(WarpMessageArm::AgentOutput),
        4 => Some(WarpMessageArm::ToolCall),
        5 => Some(WarpMessageArm::ToolResult),
        9 => Some(WarpMessageArm::SystemQuery),
        15 => Some(WarpMessageArm::AgentReasoning),
        16 => Some(WarpMessageArm::Summarization),
        17..=20 | 22..=23 | 25..=28 => Some(WarpMessageArm::Unknown(field)),
        21 => Some(WarpMessageArm::DebugOutput),
        24 => Some(WarpMessageArm::ReceivedMessages),
        _ => None,
    }
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

pub(super) fn is_warp_tool_arm(field: u32) -> bool {
    matches!(field, 2..=28 | 30..=32 | 34..=38)
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

pub(super) fn is_warp_tool_result_arm(field: u32) -> bool {
    matches!(field, 2..=10 | 14..=32 | 34..=36 | 38..=42)
}

#[derive(Clone, Copy, Debug)]
pub(super) enum WarpWireValue<'a> {
    Varint(u64),
    Fixed64(u64),
    LengthDelimited(&'a [u8]),
    Fixed32,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct WarpWireField<'a> {
    pub(super) number: u32,
    pub(super) value: WarpWireValue<'a>,
}

pub(super) struct WarpWireCursor<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> WarpWireCursor<'a> {
    pub(super) fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }

    pub(super) fn next(&mut self) -> Result<Option<WarpWireField<'a>>> {
        if self.position == self.data.len() {
            return Ok(None);
        }
        let key = read_varint(self.data, &mut self.position)?;
        let number = u32::try_from(key >> 3).map_err(|_| {
            CaptureError::InvalidPayload("Warp protobuf field number overflowed".to_owned())
        })?;
        if number == 0 {
            return Err(CaptureError::InvalidPayload(
                "Warp protobuf field number must be nonzero".to_owned(),
            ));
        }
        let wire_type = (key & 0x07) as u8;
        let value = match wire_type {
            0 => WarpWireValue::Varint(read_varint(self.data, &mut self.position)?),
            1 => {
                let bytes: [u8; 8] = self
                    .take(8, "fixed64")?
                    .try_into()
                    .map_err(|_| CaptureError::SystemInvariant("Warp fixed64 width changed"))?;
                WarpWireValue::Fixed64(u64::from_le_bytes(bytes))
            }
            2 => {
                let length =
                    usize::try_from(read_varint(self.data, &mut self.position)?).map_err(|_| {
                        CaptureError::InvalidPayload(
                            "Warp protobuf length exceeds the platform range".to_owned(),
                        )
                    })?;
                WarpWireValue::LengthDelimited(self.take(length, "length-delimited field")?)
            }
            5 => {
                let _ = self.take(4, "fixed32")?;
                WarpWireValue::Fixed32
            }
            other => {
                return Err(CaptureError::InvalidPayload(format!(
                    "unsupported Warp protobuf wire type {other}"
                )))
            }
        };
        Ok(Some(WarpWireField { number, value }))
    }

    fn take(&mut self, length: usize, label: &str) -> Result<&'a [u8]> {
        let end = self.position.checked_add(length).ok_or_else(|| {
            CaptureError::InvalidPayload(format!("overflow while decoding Warp {label}"))
        })?;
        if end > self.data.len() {
            return Err(CaptureError::InvalidPayload(format!(
                "truncated {label} in Warp protobuf"
            )));
        }
        let value = &self.data[self.position..end];
        self.position = end;
        Ok(value)
    }
}

pub(super) fn warp_wire_text(data: &[u8]) -> Result<&str> {
    std::str::from_utf8(data).map_err(|error| {
        CaptureError::InvalidPayload(format!("invalid UTF-8 in Warp protobuf: {error}"))
    })
}

fn read_varint(data: &[u8], position: &mut usize) -> Result<u64> {
    let mut value = 0_u64;
    for shift in (0..70).step_by(7) {
        let byte = *data.get(*position).ok_or_else(|| {
            CaptureError::InvalidPayload("truncated varint in Warp protobuf".to_owned())
        })?;
        *position += 1;
        if shift == 63 && byte & 0xfe != 0 {
            return Err(CaptureError::InvalidPayload(
                "oversized varint in Warp protobuf".to_owned(),
            ));
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(CaptureError::InvalidPayload(
        "oversized varint in Warp protobuf".to_owned(),
    ))
}
