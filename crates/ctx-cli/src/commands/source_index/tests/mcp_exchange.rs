use ctx_history_core::{
    EventCopyProofKind, EventOrigin, McpExchangeContent, McpInvocationContent, McpJsonCapture,
    McpTerminalResponseContent, McpTerminalStatus, McpTextCapture, McpToolCallAttribution,
    SessionRelationshipKind,
};

use super::*;
use crate::commands::source_index::mcp_show_event;

const ARGUMENT_SEARCH_CANARY: &str = "zzargumentcanary8h63";
const CALL_ID_SEARCH_CANARY: &str = "zzcallidcanary7g52";
const RESPONSE_SEARCH_CANARY: &str = "zzresponsecanary9j74";
const COPIED_SEARCH_CANARY: &str = "zzcopiedlineagecanary6k41";

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

#[test]
fn search_snippets_use_mcp_invocation_arguments_but_exclude_response_and_call_id() {
    let temp = tempdir().unwrap();
    let event = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 96, 1);
    let exchange = McpExchangeContent {
        provider_call_id: CALL_ID_SEARCH_CANARY.to_owned(),
        invocation: Some(McpInvocationContent {
            server: "mcp-検索サーバー".to_owned(),
            tool: "nested_lookup_tool".to_owned(),
            arguments: McpJsonCapture::Present {
                value: json!({
                    "outer": {
                        "雪": ["東京", {"argument_only": ARGUMENT_SEARCH_CANARY}],
                    },
                }),
            },
        }),
        response: Some(McpTerminalResponseContent {
            status: McpTerminalStatus::Succeeded,
            failure_kind: None,
            duration_ns: Some(42),
            text: McpTextCapture::NormalizedBody,
            payload: McpJsonCapture::Present {
                value: json!({"response_only": RESPONSE_SEARCH_CANARY}),
            },
        }),
    };
    let exact_exchange = serde_json::to_value(&exchange).unwrap();
    let mut stored = fixture_core_event(&event, "ordinary stored response body");
    stored.core_record.mcp_tool_call = Some(McpToolCallAttribution {
        server: "mcp-検索サーバー".to_owned(),
        tool: "nested_lookup_tool".to_owned(),
    });
    stored.core_record.content.mcp_exchange = Some(exchange);
    stored.core_record.validate_contract().unwrap();
    append_fixture_session(temp.path(), std::slice::from_ref(&stored), 96);

    let mut argument_request = request(RefreshArg::Off);
    argument_request.query = ARGUMENT_SEARCH_CANARY.to_owned();
    argument_request.events = true;
    argument_request.limit = 1;
    let (value, collection, _) = search_existing_generation(
        &argument_request,
        open_index(temp.path()).unwrap(),
        temp.path(),
        argument_request.semantic_weight,
        "existing_generation",
        1,
    )
    .unwrap();

    assert_eq!(collection.result_window.hits.len(), 1);
    assert_eq!(
        value["results"][0]["ctx_event_id"],
        json!(event.event_id.as_uuid())
    );
    let snippet = value["results"][0]["snippet"].as_str().unwrap();
    assert!(snippet.contains(ARGUMENT_SEARCH_CANARY));
    assert!(snippet.contains("東京"));
    assert!(!snippet.contains(CALL_ID_SEARCH_CANARY));
    assert!(!snippet.contains(RESPONSE_SEARCH_CANARY));

    let (mcp_value, _) = mcp_search(argument_request, temp.path()).unwrap();
    assert_eq!(mcp_value["results"][0]["snippet"], snippet);
    assert_eq!(
        mcp_value["results"][0]["session_relationship"],
        value["results"][0]["session_relationship"]
    );
    assert_eq!(
        mcp_value["results"][0]["event_origin"],
        value["results"][0]["event_origin"]
    );

    let shown = mcp_show_event(
        temp.path(),
        &stored.event_id.as_uuid().to_string(),
        0,
        0,
        None,
        crate::presentation_limit::MCP_PRESENTATION_MAX_OUTPUT_BYTES,
    )
    .unwrap();
    assert_eq!(shown["event"]["text"], "ordinary stored response body");
    assert_eq!(shown["event"]["mcp_exchange"], exact_exchange);

    for excluded in [CALL_ID_SEARCH_CANARY, RESPONSE_SEARCH_CANARY] {
        let mut excluded_request = request(RefreshArg::Off);
        excluded_request.query = excluded.to_owned();
        excluded_request.events = true;
        let (value, collection, _) = search_existing_generation(
            &excluded_request,
            open_index(temp.path()).unwrap(),
            temp.path(),
            excluded_request.semantic_weight,
            "existing_generation",
            1,
        )
        .unwrap();
        assert!(collection.result_window.hits.is_empty());
        assert!(value["results"].as_array().unwrap().is_empty());
    }
}

#[test]
fn copied_lineage_is_hidden_from_mcp_search_but_visible_in_show_and_query_events() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let ancestor = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 97, 1);
    let mut copied = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 98, 1);
    copied.parent_session_id = Some(ancestor.session_id);
    copied.root_session_id = ancestor.session_id;
    copied.session_relationship = SessionRelationshipKind::Forked;
    copied.event_origin = EventOrigin::CopiedFromAncestor {
        ancestor_session_id: ancestor.session_id,
        ancestor_event_id: ancestor.event_id,
        proof: EventCopyProofKind::NativeEventIdentity,
    };
    let ancestor = fixture_core_event(&ancestor, "ancestor body");
    let copied = fixture_core_event(&copied, COPIED_SEARCH_CANARY);
    append_fixture_session(temp.path(), std::slice::from_ref(&ancestor), 97);
    append_fixture_session(temp.path(), std::slice::from_ref(&copied), 98);

    let mut search = request(RefreshArg::Off);
    search.query = COPIED_SEARCH_CANARY.to_owned();
    search.events = true;
    search.limit = 10;
    let (searched, _) = mcp_search(search, temp.path()).unwrap();
    assert!(searched["results"].as_array().unwrap().is_empty());

    let shown = mcp_show_event(
        temp.path(),
        &copied.event_id.as_uuid().to_string(),
        0,
        0,
        None,
        crate::presentation_limit::MCP_PRESENTATION_MAX_OUTPUT_BYTES,
    )
    .unwrap();
    let shown_event = &shown["event"];
    assert_eq!(shown_event["session_relationship"], "forked");
    assert_eq!(
        shown_event["event_origin"],
        event_origin_json(&copied.event_origin)
    );
    assert_eq!(shown_event["text"], COPIED_SEARCH_CANARY);

    let queried = crate::mcp::query_events_for_test(
        &json!({"content": "full", "limit": 100}),
        temp.path(),
    )
    .unwrap();
    let queried_event = queried["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["ctx_event_id"] == json!(copied.event_id.as_uuid()))
        .expect("copied event remains addressable through query_events");
    assert_eq!(queried_event["session_relationship"], "forked");
    assert_eq!(queried_event["event_origin"], shown_event["event_origin"]);
    assert_eq!(queried_event["text"], COPIED_SEARCH_CANARY);
}
