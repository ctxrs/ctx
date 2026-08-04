use ctx_history_core::McpToolCallAttribution;

use super::*;
use crate::commands::source_index::mcp_show_event;

#[test]
fn exact_show_surfaces_omit_absence_and_keep_tool_outputs_log_only() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let exact = McpToolCallAttribution {
        server: "server\n\u{202e}|`[]".to_owned(),
        tool: "tool\\literal\t*#".to_owned(),
    };
    let mut message_event = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 93, 1);
    message_event.role = Some("user".to_owned());
    let message = fixture_core_event(&message_event, "user message");
    let mut tool_event = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 93, 2);
    tool_event.event_type = "tool_output".to_owned();
    tool_event.role = Some("tool".to_owned());
    let mut tool_output = fixture_core_event(&tool_event, "tool result");
    tool_output.core_record.mcp_tool_call = Some(exact.clone());
    tool_output.core_record.validate_contract().unwrap();
    append_fixture_session(temp.path(), &[message.clone(), tool_output.clone()], 93);
    let index = open_index(temp.path()).unwrap();
    let session = SessionRecord::from(&message.event);

    let rendered_message = render_event_value(&message);
    let rendered_tool_output = render_event_value(&tool_output);
    assert!(rendered_message.get("mcp_tool_call").is_none());
    assert_eq!(
        rendered_tool_output["mcp_tool_call"],
        serde_json::to_value(&exact).unwrap()
    );

    for (mode, expected_events, expects_attribution) in [
        (TranscriptMode::Log, 2, true),
        (TranscriptMode::Full, 1, false),
        (TranscriptMode::Lite, 1, false),
    ] {
        let (mut ui, stdout) = test_ui();
        stream_cli_session(
            &index,
            &session,
            mode,
            OutputFormat::Json,
            None,
            None,
            &mut ui,
        )
        .unwrap();
        ui.flush().unwrap();
        let transcript: Value = serde_json::from_slice(&stdout.bytes()).unwrap();
        assert_eq!(
            transcript["events"].as_array().unwrap().len(),
            expected_events
        );
        assert_eq!(
            transcript["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|event| event.get("mcp_tool_call").is_some()),
            expects_attribution
        );
        if expects_attribution {
            assert_eq!(
                transcript["events"][1]["mcp_tool_call"],
                serde_json::to_value(&exact).unwrap()
            );
        }
    }

    let mcp_session = mcp_show_session(
        temp.path(),
        &session.session_id.as_uuid().to_string(),
        TranscriptMode::Log,
        10,
        None,
        crate::presentation_limit::MCP_PRESENTATION_MAX_OUTPUT_BYTES,
    )
    .unwrap();
    assert_eq!(
        mcp_session["events"][1]["mcp_tool_call"],
        serde_json::to_value(&exact).unwrap()
    );
    let mcp_event = mcp_show_event(
        temp.path(),
        &tool_output.event_id.as_uuid().to_string(),
        0,
        0,
        None,
        crate::presentation_limit::MCP_PRESENTATION_MAX_OUTPUT_BYTES,
    )
    .unwrap();
    assert_eq!(
        mcp_event["event"]["mcp_tool_call"],
        serde_json::to_value(&exact).unwrap()
    );

    let absent_event = mcp_show_event(
        temp.path(),
        &message.event_id.as_uuid().to_string(),
        0,
        0,
        None,
        crate::presentation_limit::MCP_PRESENTATION_MAX_OUTPUT_BYTES,
    )
    .unwrap();
    assert!(absent_event["event"].get("mcp_tool_call").is_none());
}
