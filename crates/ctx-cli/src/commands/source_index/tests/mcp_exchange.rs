use ctx_history_core::{
    McpExchangeContent, McpInvocationContent, McpJsonCapture, McpTerminalResponseContent,
    McpTerminalStatus, McpTextCapture, McpToolCallAttribution,
};

use super::*;
use crate::commands::source_index::mcp_show_event;

fn complete_exchange(payload: Value) -> McpExchangeContent {
    McpExchangeContent {
        provider_call_id: "native-call-呼び出し-🦀".to_owned(),
        invocation: Some(McpInvocationContent {
            server: "mcp-サーバー".to_owned(),
            tool: "検索-tool".to_owned(),
            arguments: McpJsonCapture::Present {
                value: json!({
                    "snake_key": ["雪", null, {"camelKey": true}],
                    "nested": {"deep_null": null},
                }),
            },
        }),
        response: Some(McpTerminalResponseContent {
            status: McpTerminalStatus::Succeeded,
            failure_kind: None,
            duration_ns: Some(42),
            text: McpTextCapture::NormalizedBody,
            payload: McpJsonCapture::Present { value: payload },
        }),
    }
}

#[test]
fn full_show_surfaces_mcp_exchange_losslessly_and_accounts_for_its_output_bytes() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let event = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 94, 1);
    let payload = json!({
        "result_key": ["完了", null, {"mixedCase": [false, 3]}],
        "large": "x".repeat(8 * 1024),
    });
    let exchange = complete_exchange(payload.clone());
    let exact_exchange = serde_json::to_value(&exchange).unwrap();
    let mut core_event = fixture_core_event(&event, "normalized response body");
    core_event.core_record.mcp_tool_call = Some(McpToolCallAttribution {
        server: "mcp-サーバー".to_owned(),
        tool: "検索-tool".to_owned(),
    });
    core_event.core_record.content.mcp_exchange = Some(exchange);
    core_event.core_record.validate_contract().unwrap();
    append_fixture_session(temp.path(), std::slice::from_ref(&core_event), 94);

    let rendered = render_event_value(&core_event);
    assert_eq!(rendered["mcp_exchange"], exact_exchange);
    assert_eq!(
        rendered["mcp_exchange"]["response"]["payload"]["value"],
        payload
    );
    assert!(rendered["mcp_exchange"]["response"]["payload"]["value"]["result_key"][1].is_null());
    assert_eq!(rendered["text"], "normalized response body");
    assert_eq!(rendered["mcp_tool_call"]["server"], "mcp-サーバー");

    let shown = mcp_show_event(
        temp.path(),
        &core_event.event_id.as_uuid().to_string(),
        0,
        0,
        None,
        crate::presentation_limit::MCP_PRESENTATION_MAX_OUTPUT_BYTES,
    )
    .unwrap();
    assert_eq!(shown["event"]["mcp_exchange"], exact_exchange);
    let session = SessionRecord::from(&core_event.event);
    let shown_session = mcp_show_session(
        temp.path(),
        &session.session_id.as_uuid().to_string(),
        TranscriptMode::Log,
        10,
        None,
        crate::presentation_limit::MCP_PRESENTATION_MAX_OUTPUT_BYTES,
    )
    .unwrap();
    assert_eq!(shown_session["events"][0]["mcp_exchange"], exact_exchange);

    let content = &core_event.core_record.content;
    let expected_preflight_bytes = 2_usize
        .saturating_add(
            crate::presentation_limit::serialized_json_bytes(&content.normalized_body).unwrap(),
        )
        .saturating_add(
            crate::presentation_limit::serialized_json_bytes(&content.structured_content).unwrap(),
        )
        .saturating_add(
            crate::presentation_limit::serialized_json_bytes(&content.mcp_exchange).unwrap(),
        );
    let error = render_event_values(&[&core_event], expected_preflight_bytes - 1).unwrap_err();
    let typed = error
        .downcast_ref::<crate::presentation_limit::PresentationOutputLimitError>()
        .expect("MCP exchange should participate in the content preflight");
    assert_eq!(typed.actual_bytes, expected_preflight_bytes);
    assert_eq!(typed.maximum_bytes, expected_preflight_bytes - 1);

    let bounded_error = mcp_show_event(
        temp.path(),
        &core_event.event_id.as_uuid().to_string(),
        0,
        0,
        None,
        1024,
    )
    .unwrap_err();
    let bounded = bounded_error
        .downcast_ref::<crate::presentation_limit::PresentationOutputLimitError>()
        .expect("MCP show-event should reject an oversized exchange response");
    assert_eq!(bounded.maximum_bytes, 1024);
    assert!(bounded.actual_bytes > bounded.maximum_bytes);

    let absent = fixture_core_event(
        &fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 95, 1),
        "no exchange",
    );
    assert!(render_event_value(&absent).get("mcp_exchange").is_none());
}
