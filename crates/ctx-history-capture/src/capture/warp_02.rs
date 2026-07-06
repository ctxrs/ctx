#[allow(unused_imports)]
use super::*;

pub(crate) fn warp_decode_system_query(data: &[u8]) -> Result<String> {
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

pub(crate) fn warp_decode_summarization(data: &[u8]) -> Result<String> {
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

pub(crate) fn warp_decode_received_messages(data: &[u8]) -> Result<String> {
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

pub(crate) fn proto_string(data: &[u8], pos: &mut usize) -> Result<String> {
    let bytes = proto_len(data, pos)?;
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|err| {
            CaptureError::InvalidPayload(format!("invalid UTF-8 in Warp protobuf: {err}"))
        })
}

pub(crate) fn proto_len<'a>(data: &'a [u8], pos: &mut usize) -> Result<&'a [u8]> {
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

pub(crate) fn proto_varint(data: &[u8], pos: &mut usize) -> Result<u64> {
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

pub(crate) fn warp_tool_name(field: u32) -> &'static str {
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

pub(crate) fn warp_tool_result_name(field: u32) -> &'static str {
    match field {
        2 => "run_shell_command",
        3 => "search_codebase",
        5 => "read_files",
        6 => "apply_file_diffs",
        8 => "suggest_create_plan",
        9 => "grep",
        15 => "read_mcp_resource",
        16 => "call_mcp_tool",
        17 => "write_to_long_running_shell_command",
        18 => "suggest_new_conversation",
        19 => "file_glob",
        21 => "open_code_review",
        22 => "init_project",
        23 => "subagent",
        24 => "read_documents",
        25 => "edit_documents",
        26 => "create_documents",
        27 => "read_shell_command_output",
        28 => "use_computer",
        30 => "read_skill",
        32 => "fetch_conversation",
        33 => "start_agent",
        34 => "send_message_to_agent",
        35 => "transfer_shell_command_control_to_user",
        _ => "unknown",
    }
}
