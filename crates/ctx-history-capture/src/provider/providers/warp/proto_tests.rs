use super::*;

fn field(field: u32, payload: &[u8]) -> Vec<u8> {
    let mut encoded = varint(u64::from(field) << 3 | 2);
    encoded.extend(varint(payload.len() as u64));
    encoded.extend_from_slice(payload);
    encoded
}

fn varint(mut value: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        bytes.push(byte);
        if value == 0 {
            return bytes;
        }
    }
}

#[test]
fn task_and_user_message_fields_decode_without_projection() {
    let mut user_query = field(1, b"hello");
    user_query.extend(field(99, b"ignored"));
    let mut message = field(1, b"message-1");
    message.extend(field(2, &user_query));
    let mut dependency = field(1, b"parent-task");
    dependency.extend(field(2, b"ignored"));
    let mut task = field(1, b"task-1");
    task.extend(field(2, b"description"));
    task.extend(field(3, &dependency));
    task.extend(field(5, &message));
    task.extend(field(6, b"summary"));

    let decoded = warp_decode_task(&task).unwrap();
    assert_eq!(decoded.id, "task-1");
    assert_eq!(decoded.description, "description");
    assert_eq!(decoded.parent_task_id.as_deref(), Some("parent-task"));
    assert_eq!(decoded.summary, "summary");
    assert_eq!(decoded.messages.len(), 1);
    assert_eq!(decoded.messages[0].kind, "user_query");
    assert_eq!(decoded.messages[0].role, Some(EventRole::User));
    assert_eq!(decoded.messages[0].text, "hello");
}

#[test]
fn wire_failures_remain_bounded_and_fail_closed() {
    let cases: &[(&[u8], &str)] = &[
        (&[0x0a, 0x02, b'a'], "truncated length-delimited field"),
        (&[0x0a, 0x01, 0xff], "invalid UTF-8"),
        (&[0x0b], "unsupported Warp protobuf wire type 3"),
        (&[0x80; 10], "oversized varint"),
    ];
    for (encoded, expected) in cases {
        let error = warp_decode_task(encoded).unwrap_err();
        assert!(
            error.to_string().contains(expected),
            "expected {expected:?}, got {error}"
        );
    }
}

#[test]
fn shell_result_recovers_exact_output_and_ignores_unknown_fields() {
    let mut finished = field(1, b"exact shell output\nUnicode: \xf0\x9f\xa6\x80");
    finished.extend(field(99, b"future nested field"));
    let mut run_shell = field(5, &finished);
    run_shell.extend(field(98, b"future result field"));
    let mut tool_result = field(1, b"call-1");
    tool_result.extend(field(2, &run_shell));
    tool_result.extend(field(97, b"future envelope field"));
    let mut message = field(1, b"message-result-1");
    message.extend(field(5, &tool_result));

    let decoded = warp_decode_message(&message).unwrap();
    assert_eq!(decoded.event_type, EventType::ToolOutput);
    assert_eq!(decoded.kind, "tool_call_result");
    assert_eq!(
        decoded.complete_text.as_deref(),
        Some("exact shell output\nUnicode: 🦀")
    );
    assert_eq!(decoded.text, decoded.complete_text.unwrap());
}

#[test]
fn binary_only_and_status_only_results_never_become_text() {
    // CallMCPToolResult.Success.Results.Image(data = arbitrary binary).
    let image = field(1, &[0x00, 0xff, 0x80, 0x01]);
    let item = field(2, &image);
    let success = field(1, &item);
    let call_mcp = field(1, &success);
    let mut tool_result = field(1, b"call-binary");
    tool_result.extend(field(16, &call_mcp));
    let mut message = field(1, b"message-binary");
    message.extend(field(5, &tool_result));

    let decoded = warp_decode_message(&message).unwrap();
    assert_eq!(decoded.event_type, EventType::ToolOutput);
    assert_eq!(decoded.complete_text, None);
    assert_eq!(decoded.text, "tool result: call_mcp_tool");

    // PermissionDenied is a status, not a result body.
    let denied = field(6, &field(1, &[]));
    let result = field(2, &denied);
    assert_eq!(warp_decode_tool_result_text(&result).unwrap(), None);

    // Future result variants remain typed-but-unhydrated until their schema is
    // reviewed; arbitrary length-delimited bytes are never promoted to text.
    assert_eq!(
        warp_decode_tool_result_text(&field(99, b"plausible but unknown text")).unwrap(),
        None
    );
}

#[test]
fn nonstandard_error_and_terminal_text_arms_are_exact() {
    let request_computer_error = field(3, &field(1, b"computer access unavailable"));
    let request_computer_result = field(31, &request_computer_error);
    assert_eq!(
        warp_decode_tool_result_text(&request_computer_result)
            .unwrap()
            .as_deref(),
        Some("computer access unavailable")
    );

    let denied = field(2, &field(1, b"remote agents disabled"));
    let run_agents_result = field(39, &denied);
    assert_eq!(
        warp_decode_tool_result_text(&run_agents_result)
            .unwrap()
            .as_deref(),
        Some("remote agents disabled")
    );

    // A successful RunAgents result is structured and must not be flattened.
    let launched = field(1, &field(1, b"model-id"));
    assert_eq!(
        warp_decode_tool_result_text(&field(39, &launched)).unwrap(),
        None
    );
}
