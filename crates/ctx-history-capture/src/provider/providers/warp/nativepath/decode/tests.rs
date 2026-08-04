use super::*;

const SERVER_A: &str = "11111111-1111-4111-8111-111111111111";
const SERVER_B: &str = "22222222-2222-4222-8222-222222222222";

#[test]
fn qualified_mcp_success_error_cancellation_and_nontext_results_link_exactly() {
    let messages = vec![
        mcp_call_message(
            "success",
            SERVER_A,
            "shared_tool",
            struct_string("side", "a"),
        ),
        mcp_success_message(
            "success",
            vec![
                mcp_text_content("first"),
                mcp_image_content(),
                mcp_resource_text_content("resource text"),
                mcp_text_content("last"),
            ],
        ),
        mcp_call_message("error", SERVER_B, "shared_tool", Vec::new()),
        mcp_error_message("error", "sanitized tool error"),
        mcp_call_message("cancel", SERVER_A, "cancel_tool", Vec::new()),
        cancel_message("cancel"),
        mcp_call_message("nontext", SERVER_B, "binary_tool", Vec::new()),
        mcp_success_message("nontext", vec![mcp_image_content()]),
    ];
    let decoded = decode_warp_native_task(&task_messages(messages)).unwrap();

    for (index, server, tool, outcome, body) in [
        (
            1,
            SERVER_A,
            "shared_tool",
            OutputOutcome::Success,
            "first\nresource text\nlast",
        ),
        (
            3,
            SERVER_B,
            "shared_tool",
            OutputOutcome::Failure,
            "sanitized tool error",
        ),
        (5, SERVER_A, "cancel_tool", OutputOutcome::Unknown, ""),
        (7, SERVER_B, "binary_tool", OutputOutcome::Success, ""),
    ] {
        let WarpDecodedMessagePayload::Output(output) = &decoded.messages[index].payload else {
            panic!("terminal MCP result at {index} was not retained");
        };
        let invocation = output.mcp_invocation.as_ref().unwrap();
        assert_eq!(invocation.server_id, server);
        assert_eq!(invocation.tool_name, tool);
        assert_eq!(output.outcome, outcome);
        assert_eq!(output.body, body);
    }
    let WarpDecodedMessagePayload::Retained(call) = &decoded.messages[0].payload else {
        panic!("MCP call was not retained");
    };
    assert_eq!(call.call_id.as_deref(), Some("success"));
    assert_eq!(call.mcp_invocation.as_ref().unwrap().args["side"], "a");
}

#[test]
fn invalid_duplicate_orphan_and_ambiguous_mcp_relations_abstain() {
    let mut mismatched_shell = Vec::new();
    push_length_delimited(&mut mismatched_shell, 5, &nested_text("other result"));
    let messages = vec![
        mcp_call_message("malformed", "not-a-uuid", "tool", Vec::new()),
        mcp_success_message("malformed", vec![]),
        mcp_call_message("missing", "", "tool", Vec::new()),
        mcp_success_message("missing", vec![]),
        mcp_call_message("duplicate-call", SERVER_A, "tool", Vec::new()),
        mcp_call_message("duplicate-call", SERVER_B, "tool", Vec::new()),
        mcp_success_message("duplicate-call", vec![]),
        mcp_call_message("duplicate-result", SERVER_A, "tool", Vec::new()),
        mcp_success_message("duplicate-result", vec![]),
        mcp_error_message("duplicate-result", "second result"),
        mcp_call_message("mismatch", SERVER_A, "tool", Vec::new()),
        tool_result_message("mismatch", 2, &mismatched_shell),
        mcp_success_message("mismatch", vec![]),
        mcp_success_message("orphan", vec![]),
    ];
    let decoded = decode_warp_native_task(&task_messages(messages)).unwrap();
    for message in &decoded.messages {
        if let WarpDecodedMessagePayload::Output(output) = &message.payload {
            assert!(output.mcp_invocation.is_none());
        }
    }

    for server in [SERVER_A, SERVER_B] {
        let decoded = decode_warp_native_task(&task_messages(vec![
            mcp_call_message("reused", server, "scoped_tool", Vec::new()),
            mcp_success_message("reused", vec![]),
        ]))
        .unwrap();
        let WarpDecodedMessagePayload::Output(output) = &decoded.messages[1].payload else {
            panic!("scoped result was not retained");
        };
        assert_eq!(output.mcp_invocation.as_ref().unwrap().server_id, server);
    }
}

#[test]
fn invalid_then_valid_required_strings_permanently_invalidate_attribution() {
    let valid_payload = mcp_call_payload("tool", Vec::new(), SERVER_A);

    let mut invalid_call_id = Vec::new();
    push_length_delimited(&mut invalid_call_id, 1, &[0xff]);
    push_length_delimited(&mut invalid_call_id, 1, b"call-id");
    push_length_delimited(&mut invalid_call_id, 12, &valid_payload);
    assert_mcp_pair_abstains(
        tool_call_message(invalid_call_id),
        mcp_success_message("call-id", vec![]),
    );

    let mut invalid_name = Vec::new();
    push_length_delimited(&mut invalid_name, 1, &[0xff]);
    push_length_delimited(&mut invalid_name, 1, b"tool");
    push_length_delimited(&mut invalid_name, 2, &[]);
    push_length_delimited(&mut invalid_name, 3, SERVER_A.as_bytes());
    assert_mcp_pair_abstains(
        mcp_call_message_with_payload("name", invalid_name),
        mcp_success_message("name", vec![]),
    );

    let mut invalid_server = Vec::new();
    push_length_delimited(&mut invalid_server, 1, b"tool");
    push_length_delimited(&mut invalid_server, 2, &[]);
    push_length_delimited(&mut invalid_server, 3, &[0xff]);
    push_length_delimited(&mut invalid_server, 3, SERVER_A.as_bytes());
    assert_mcp_pair_abstains(
        mcp_call_message_with_payload("server", invalid_server),
        mcp_success_message("server", vec![]),
    );

    let mut invalid_string_value = Vec::new();
    push_length_delimited(&mut invalid_string_value, 3, &[0xff]);
    push_length_delimited(&mut invalid_string_value, 3, b"valid");
    assert_mcp_pair_abstains(
        mcp_call_message(
            "args-string",
            SERVER_A,
            "tool",
            struct_entry("value", &invalid_string_value),
        ),
        mcp_success_message("args-string", vec![]),
    );

    let mut string_value = Vec::new();
    push_length_delimited(&mut string_value, 3, b"value");
    let mut invalid_key_entry = Vec::new();
    push_length_delimited(&mut invalid_key_entry, 1, &[0xff]);
    push_length_delimited(&mut invalid_key_entry, 1, b"valid-key");
    push_length_delimited(&mut invalid_key_entry, 2, &string_value);
    let mut invalid_key_args = Vec::new();
    push_length_delimited(&mut invalid_key_args, 1, &invalid_key_entry);
    assert_mcp_pair_abstains(
        mcp_call_message("args-key", SERVER_A, "tool", invalid_key_args),
        mcp_success_message("args-key", vec![]),
    );

    let mut result = Vec::new();
    push_length_delimited(&mut result, 1, &[0xff]);
    push_length_delimited(&mut result, 1, b"result-id");
    push_length_delimited(&mut result, 16, &mcp_success_payload(vec![]));
    assert_mcp_pair_abstains(
        mcp_call_message("result-id", SERVER_A, "tool", Vec::new()),
        tool_result_message_from_payload(result),
    );
}

#[test]
fn malformed_embedded_occurrence_is_not_repaired_by_a_later_fragment() {
    let valid_args = struct_string("valid", "second");
    let mut truncated_args = vec![0x0a];
    push_varint(
        &mut truncated_args,
        u64::try_from(valid_args.len()).unwrap(),
    );
    let mut payload = Vec::new();
    push_length_delimited(&mut payload, 1, b"tool");
    push_length_delimited(&mut payload, 2, &truncated_args);
    push_length_delimited(&mut payload, 2, &valid_args);
    push_length_delimited(&mut payload, 3, SERVER_A.as_bytes());
    assert_mcp_pair_abstains(
        mcp_call_message_with_payload("fragmented-args", payload),
        mcp_success_message("fragmented-args", vec![]),
    );

    let mut tool_call = Vec::new();
    push_length_delimited(&mut tool_call, 1, b"fragmented");
    push_length_delimited(&mut tool_call, 12, &[0x0a, 0x01]);
    push_length_delimited(
        &mut tool_call,
        12,
        &mcp_call_payload("tool", Vec::new(), SERVER_A),
    );
    let malformed = task_messages(vec![tool_call_message(tool_call)]);
    assert!(decode_warp_native_task(&malformed).is_err());

    let neighboring_valid = decode_warp_native_task(&task_messages(vec![
        mcp_call_message("neighbor", SERVER_B, "neighbor_tool", Vec::new()),
        mcp_success_message("neighbor", vec![]),
    ]))
    .unwrap();
    let WarpDecodedMessagePayload::Output(output) = &neighboring_valid.messages[1].payload else {
        panic!("neighboring valid MCP result was not retained");
    };
    assert_eq!(output.mcp_invocation.as_ref().unwrap().server_id, SERVER_B);
}

#[test]
fn missing_unset_and_nonfinite_args_abstain_without_dropping_records() {
    let mut missing_args = Vec::new();
    push_length_delimited(&mut missing_args, 1, b"tool");
    push_length_delimited(&mut missing_args, 3, SERVER_A.as_bytes());
    assert_mcp_pair_abstains(
        mcp_call_message_with_payload("missing-args", missing_args),
        mcp_success_message("missing-args", vec![]),
    );

    assert_mcp_pair_abstains(
        mcp_call_message("unset-value", SERVER_A, "tool", struct_entry("unset", &[])),
        mcp_success_message("unset-value", vec![]),
    );

    for (call_id, value) in [
        ("nan", f64::NAN),
        ("positive-infinity", f64::INFINITY),
        ("negative-infinity", f64::NEG_INFINITY),
    ] {
        let mut number = Vec::new();
        push_fixed64_field(&mut number, 2, value.to_bits());
        assert_mcp_pair_abstains(
            mcp_call_message(call_id, SERVER_A, "tool", struct_entry("number", &number)),
            mcp_success_message(call_id, vec![]),
        );
    }
}

#[test]
fn repeated_message_occurrences_merge_decoded_semantics() {
    let mut nested_value = Vec::new();
    push_length_delimited(&mut nested_value, 5, &struct_string("first", "one"));
    push_length_delimited(&mut nested_value, 5, &struct_bool("second", true));

    let mut first_list = Vec::new();
    push_length_delimited(&mut first_list, 1, &varint_field(4, 0));
    let mut second_list = Vec::new();
    push_length_delimited(&mut second_list, 1, &varint_field(4, 1));
    let mut list_value = Vec::new();
    push_length_delimited(&mut list_value, 6, &first_list);
    push_length_delimited(&mut list_value, 6, &second_list);

    let mut payload = Vec::new();
    push_length_delimited(&mut payload, 1, b"merged_tool");
    push_length_delimited(&mut payload, 2, &struct_entry("nested", &nested_value));
    push_length_delimited(&mut payload, 2, &struct_entry("list", &list_value));
    push_length_delimited(&mut payload, 3, SERVER_A.as_bytes());

    let decoded = decode_warp_native_task(&task_messages(vec![
        mcp_call_message_with_payload("merged", payload),
        mcp_success_message("merged", vec![]),
    ]))
    .unwrap();
    let WarpDecodedMessagePayload::Output(output) = &decoded.messages[1].payload else {
        panic!("merged MCP result was not retained");
    };
    assert_eq!(
        output.mcp_invocation.as_ref().unwrap().args,
        serde_json::json!({
            "nested": {"first": "one", "second": true},
            "list": [false, true],
        })
    );
}

#[test]
fn validated_uuid_text_is_preserved_exactly() {
    const RAW_SERVER: &str = "ABCDEFAB-CDEF-4ABC-8DEF-ABCDEFABCDEF";
    let decoded = decode_warp_native_task(&task_messages(vec![
        mcp_call_message("raw-server", RAW_SERVER, "tool", Vec::new()),
        mcp_success_message("raw-server", vec![]),
    ]))
    .unwrap();
    let WarpDecodedMessagePayload::Output(output) = &decoded.messages[1].payload else {
        panic!("raw-server MCP result was not retained");
    };
    assert_eq!(
        output.mcp_invocation.as_ref().unwrap().server_id,
        RAW_SERVER
    );
}

#[test]
fn reordered_repeated_unknown_and_last_oneof_fields_follow_protobuf_semantics() {
    let stale = mcp_call_payload("stale_tool", struct_string("stale", "discarded"), SERVER_B);
    let final_part_one = mcp_call_payload("intermediate", struct_string("first", "one"), SERVER_A);
    let final_part_two = mcp_call_payload("final_tool", struct_bool("second", true), SERVER_A);

    let mut first_tool_fragment = Vec::new();
    push_length_delimited(&mut first_tool_fragment, 1, b"stale-id");
    push_length_delimited(&mut first_tool_fragment, 12, &stale);
    push_length_delimited(&mut first_tool_fragment, 99, b"unknown");
    push_length_delimited(&mut first_tool_fragment, 2, &[]);
    let mut final_tool_fragment = Vec::new();
    push_length_delimited(&mut final_tool_fragment, 2, &[]);
    push_length_delimited(&mut final_tool_fragment, 12, &final_part_one);
    push_length_delimited(&mut final_tool_fragment, 12, &final_part_two);
    push_length_delimited(&mut final_tool_fragment, 1, b"final-id");
    let mut call_message = Vec::new();
    push_length_delimited(&mut call_message, 4, &first_tool_fragment);
    push_length_delimited(&mut call_message, 3, &nested_text("discarded output arm"));
    push_length_delimited(&mut call_message, 4, &final_tool_fragment);

    let mut error = Vec::new();
    push_length_delimited(&mut error, 1, b"discarded error");
    let mut mcp_error = Vec::new();
    push_length_delimited(&mut mcp_error, 2, &error);
    let mut success_one = Vec::new();
    push_length_delimited(&mut success_one, 1, &mcp_text_content("first result"));
    let mut success_two = Vec::new();
    push_length_delimited(&mut success_two, 1, &mcp_text_content("second result"));
    let mut mcp_success_one = Vec::new();
    push_length_delimited(&mut mcp_success_one, 1, &success_one);
    let mut mcp_success_two = Vec::new();
    push_length_delimited(&mut mcp_success_two, 1, &success_two);
    let mut result_fragment_one = Vec::new();
    push_length_delimited(&mut result_fragment_one, 1, b"stale-result-id");
    push_length_delimited(&mut result_fragment_one, 16, &mcp_error);
    push_length_delimited(&mut result_fragment_one, 14, &[]);
    push_length_delimited(&mut result_fragment_one, 99, b"unknown");
    let mut result_fragment_two = Vec::new();
    push_length_delimited(&mut result_fragment_two, 16, &mcp_error);
    push_length_delimited(&mut result_fragment_two, 14, &[]);
    push_length_delimited(&mut result_fragment_two, 16, &mcp_success_one);
    push_length_delimited(&mut result_fragment_two, 16, &mcp_success_two);
    push_length_delimited(&mut result_fragment_two, 1, b"final-id");
    let mut result_message = Vec::new();
    push_length_delimited(&mut result_message, 5, &result_fragment_one);
    push_length_delimited(&mut result_message, 4, &[]);
    push_length_delimited(&mut result_message, 5, &result_fragment_two);

    let decoded =
        decode_warp_native_task(&task_messages(vec![call_message, result_message])).unwrap();
    let WarpDecodedMessagePayload::Retained(call) = &decoded.messages[0].payload else {
        panic!("final tool-call arm was not retained");
    };
    let invocation = call.mcp_invocation.as_ref().unwrap();
    assert_eq!(call.call_id.as_deref(), Some("final-id"));
    assert_eq!(invocation.server_id, SERVER_A);
    assert_eq!(invocation.tool_name, "final_tool");
    assert_eq!(
        invocation.args,
        serde_json::json!({"first": "one", "second": true})
    );

    let WarpDecodedMessagePayload::Output(output) = &decoded.messages[1].payload else {
        panic!("final result arm was not retained");
    };
    assert_eq!(output.call_id.as_deref(), Some("final-id"));
    assert_eq!(output.outcome, OutputOutcome::Success);
    assert_eq!(output.body, "first result\nsecond result");
    assert_eq!(
        output.mcp_invocation.as_ref().unwrap().tool_name,
        "final_tool"
    );
}

#[test]
fn protobuf_struct_args_preserve_json_types_and_map_last_value_semantics() {
    let mut number = Vec::new();
    push_fixed64_field(&mut number, 2, 42.5_f64.to_bits());
    let null_value = varint_field(1, 0);
    let mut first = Vec::new();
    push_length_delimited(&mut first, 3, b"discarded");
    push_varint_field(&mut first, 4, 1);

    let mut list = Vec::new();
    let mut list_string = Vec::new();
    push_length_delimited(&mut list_string, 3, b"item");
    push_length_delimited(&mut list, 1, &list_string);
    push_length_delimited(&mut list, 1, &varint_field(4, 0));
    let mut list_value = Vec::new();
    push_length_delimited(&mut list_value, 6, &list);

    let nested = struct_string("key", "value");
    let mut nested_value = Vec::new();
    push_length_delimited(&mut nested_value, 5, &nested);

    let args = [
        struct_entry("number", &number),
        struct_entry("null", &null_value),
        struct_entry("last", &first),
        struct_string("last", "final"),
        struct_entry("list", &list_value),
        struct_entry("nested", &nested_value),
    ]
    .concat();
    assert_eq!(
        decode_protobuf_struct(&args, 0).unwrap(),
        serde_json::json!({
            "number": 42.5,
            "null": Value::Null,
            "last": "final",
            "list": ["item", false],
            "nested": {"key": "value"},
        })
    );
}

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
    task_messages(vec![message])
}

fn task_messages(messages: Vec<Vec<u8>>) -> Vec<u8> {
    let mut task = Vec::new();
    for message in messages {
        push_length_delimited(&mut task, 5, &message);
    }
    task
}

fn assert_mcp_pair_abstains(call: Vec<u8>, result: Vec<u8>) {
    let decoded = decode_warp_native_task(&task_messages(vec![call, result])).unwrap();
    assert!(matches!(
        decoded.messages[0].payload,
        WarpDecodedMessagePayload::Retained(_)
    ));
    let WarpDecodedMessagePayload::Output(output) = &decoded.messages[1].payload else {
        panic!("malformed MCP result record was not retained");
    };
    assert!(output.mcp_invocation.is_none());
}

fn mcp_call_message(call_id: &str, server: &str, tool: &str, args: Vec<u8>) -> Vec<u8> {
    mcp_call_message_with_payload(call_id, mcp_call_payload(tool, args, server))
}

fn mcp_call_message_with_payload(call_id: &str, payload: Vec<u8>) -> Vec<u8> {
    let mut tool_call = Vec::new();
    push_length_delimited(&mut tool_call, 1, call_id.as_bytes());
    push_length_delimited(&mut tool_call, 12, &payload);
    tool_call_message(tool_call)
}

fn tool_call_message(tool_call: Vec<u8>) -> Vec<u8> {
    let mut message = Vec::new();
    push_length_delimited(&mut message, 4, &tool_call);
    message
}

fn mcp_call_payload(tool: &str, args: Vec<u8>, server: &str) -> Vec<u8> {
    let mut payload = Vec::new();
    push_length_delimited(&mut payload, 1, tool.as_bytes());
    push_length_delimited(&mut payload, 2, &args);
    if !server.is_empty() {
        push_length_delimited(&mut payload, 3, server.as_bytes());
    }
    payload
}

fn mcp_success_message(call_id: &str, contents: Vec<Vec<u8>>) -> Vec<u8> {
    tool_result_message(call_id, 16, &mcp_success_payload(contents))
}

fn mcp_success_payload(contents: Vec<Vec<u8>>) -> Vec<u8> {
    let mut success = Vec::new();
    for content in contents {
        push_length_delimited(&mut success, 1, &content);
    }
    let mut mcp_result = Vec::new();
    push_length_delimited(&mut mcp_result, 1, &success);
    mcp_result
}

fn mcp_error_message(call_id: &str, text: &str) -> Vec<u8> {
    let mut error = Vec::new();
    push_length_delimited(&mut error, 1, text.as_bytes());
    let mut mcp_result = Vec::new();
    push_length_delimited(&mut mcp_result, 2, &error);
    tool_result_message(call_id, 16, &mcp_result)
}

fn cancel_message(call_id: &str) -> Vec<u8> {
    tool_result_message(call_id, 14, &[])
}

fn tool_result_message(call_id: &str, arm: u32, result: &[u8]) -> Vec<u8> {
    let mut tool_result = Vec::new();
    push_length_delimited(&mut tool_result, 1, call_id.as_bytes());
    push_length_delimited(&mut tool_result, arm, result);
    tool_result_message_from_payload(tool_result)
}

fn tool_result_message_from_payload(tool_result: Vec<u8>) -> Vec<u8> {
    let mut message = Vec::new();
    push_length_delimited(&mut message, 5, &tool_result);
    message
}

fn mcp_text_content(text: &str) -> Vec<u8> {
    let mut text_payload = Vec::new();
    push_length_delimited(&mut text_payload, 1, text.as_bytes());
    let mut result = Vec::new();
    push_length_delimited(&mut result, 1, &text_payload);
    result
}

fn mcp_image_content() -> Vec<u8> {
    let mut image = Vec::new();
    push_length_delimited(&mut image, 1, b"c2FuaXRpemVk");
    push_length_delimited(&mut image, 2, b"image/png");
    let mut result = Vec::new();
    push_length_delimited(&mut result, 2, &image);
    result
}

fn mcp_resource_text_content(text: &str) -> Vec<u8> {
    let mut resource_text = Vec::new();
    push_length_delimited(&mut resource_text, 1, text.as_bytes());
    let mut resource = Vec::new();
    push_length_delimited(&mut resource, 1, b"sanitized://resource");
    push_length_delimited(&mut resource, 2, &resource_text);
    let mut result = Vec::new();
    push_length_delimited(&mut result, 3, &resource);
    result
}

fn struct_string(key: &str, value: &str) -> Vec<u8> {
    let mut string_value = Vec::new();
    push_length_delimited(&mut string_value, 3, value.as_bytes());
    struct_entry(key, &string_value)
}

fn struct_bool(key: &str, value: bool) -> Vec<u8> {
    let mut bool_value = Vec::new();
    push_varint_field(&mut bool_value, 4, u64::from(value));
    struct_entry(key, &bool_value)
}

fn struct_entry(key: &str, value: &[u8]) -> Vec<u8> {
    let mut entry = Vec::new();
    push_length_delimited(&mut entry, 1, key.as_bytes());
    push_length_delimited(&mut entry, 2, value);
    let mut structure = Vec::new();
    push_length_delimited(&mut structure, 1, &entry);
    structure
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

fn push_varint_field(target: &mut Vec<u8>, field: u32, value: u64) {
    push_varint(target, u64::from(field) << 3);
    push_varint(target, value);
}

fn varint_field(field: u32, value: u64) -> Vec<u8> {
    let mut encoded = Vec::new();
    push_varint_field(&mut encoded, field, value);
    encoded
}

fn push_fixed64_field(target: &mut Vec<u8>, field: u32, value: u64) {
    push_varint(target, u64::from(field) << 3 | 1);
    target.extend_from_slice(&value.to_le_bytes());
}

fn push_varint(target: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        target.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    target.push(value as u8);
}
