use std::{
    fs,
    path::{Path, PathBuf},
};

use super::*;

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/agent-history-v1/fixtures")
}

#[test]
fn parses_all_shared_fixtures_into_typed_envelopes() {
    let mut seen = 0;
    for entry in fs::read_dir(fixture_root()).unwrap() {
        let entry = entry.unwrap();
        if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let fixture = fs::read_to_string(entry.path()).unwrap();
        let envelope: AgentHistoryEnvelope = serde_json::from_str(&fixture).unwrap();
        assert_eq!(envelope.contract_version, CONTRACT_VERSION);
        assert_eq!(envelope.schema_version, SCHEMA_VERSION);
        match envelope.operation {
            AgentHistoryOperation::Status | AgentHistoryOperation::Init => {
                assert!(envelope.status.is_some(), "{:?}", entry.path());
            }
            AgentHistoryOperation::Sources => {
                assert!(envelope.sources.is_some(), "{:?}", entry.path())
            }
            AgentHistoryOperation::Import | AgentHistoryOperation::Sync => {
                assert!(envelope.import_result.is_some(), "{:?}", entry.path());
            }
            AgentHistoryOperation::Search => {
                let search = envelope.search.as_ref().expect("search fixture payload");
                let result_window = search
                    .result_window
                    .as_ref()
                    .expect("search fixture resultWindow");
                assert_eq!(result_window.returned, search.results.len() as u64);
                assert!(search.pagination.is_some(), "{:?}", entry.path());
                if let Some(hit) = search.results.first() {
                    assert_eq!(hit.provider.as_deref(), Some("codex"));
                    assert_eq!(
                        hit.provider_session_id.as_deref(),
                        Some("codex-fixture-session")
                    );
                    assert_eq!(hit.source_format.as_deref(), Some("codex_session_jsonl"));
                }
            }
            AgentHistoryOperation::ShowEvent => {
                let event = envelope
                    .event
                    .as_ref()
                    .and_then(|result| result.event.as_ref())
                    .expect("show-event fixture selected event");
                assert_eq!(event.provider.as_deref(), Some("codex"));
                assert_eq!(
                    event.provider_session_id.as_deref(),
                    Some("codex-fixture-session")
                );
                assert_eq!(event.source_format.as_deref(), Some("codex_session_jsonl"));
                assert_eq!(
                    event.structured_content.as_ref().unwrap()["kind"],
                    "toolResult"
                );
                assert_eq!(
                    event.structured_content.as_ref().unwrap()["payload"]["items"][2]["nested"][1],
                    false
                );
                assert_eq!(
                    event
                        .content
                        .as_ref()
                        .map(|content| (&content.policy_status, content.complete)),
                    Some((&CoreContentPolicyStatus::Selected, true))
                );
            }
            AgentHistoryOperation::ShowSession => {
                let summary = envelope
                    .session
                    .as_ref()
                    .and_then(|result| result.session.as_ref())
                    .expect("show-session fixture summary");
                assert_eq!(summary.provider.as_deref(), Some("codex"));
                assert_eq!(
                    summary.provider_session_id.as_deref(),
                    Some("codex-fixture-session")
                );
                assert_eq!(
                    summary.source_format.as_deref(),
                    Some("codex_session_jsonl")
                );
                assert_eq!(
                    envelope.session.as_ref().unwrap().events[0]
                        .structured_content
                        .as_ref()
                        .unwrap()[1]["complete"],
                    true
                );
            }
            AgentHistoryOperation::Error => {
                assert!(envelope.error.is_some(), "{:?}", entry.path())
            }
        }
        seen += 1;
    }
    assert!(seen > 0, "expected shared agent-history-v1 fixtures");
}

#[test]
fn preserves_additive_fields() {
    let fixture = r#"{
        "contractVersion": "agent-history-v1",
        "schemaVersion": 1,
        "operation": "status",
        "status": {
            "initialized": true,
            "localOnly": true,
            "futureField": {"enabled": true}
        },
        "futureEnvelopeField": "kept"
    }"#;
    let envelope: AgentHistoryEnvelope = serde_json::from_str(fixture).unwrap();
    let status = envelope.status.unwrap();
    assert_eq!(status.extra["futureField"]["enabled"], true);
    assert_eq!(envelope.extra["futureEnvelopeField"], "kept");
}

#[test]
fn mcp_tool_call_is_exact_bounded_and_omitted_when_absent() {
    let fixture = fs::read_to_string(fixture_root().join("show-event.mcp-tool-call.json")).unwrap();
    let envelope: AgentHistoryEnvelope = serde_json::from_str(&fixture).unwrap();
    let result = envelope.event.unwrap();
    let selected = result.event.unwrap();
    let mcp_tool_call = selected.mcp_tool_call.as_ref().unwrap();

    assert_eq!(mcp_tool_call.server, "mcp-サーバー-🦀");
    assert_eq!(mcp_tool_call.tool, "検索/工具/🛠️");
    assert_eq!(selected.extra["futureEventField"]["preserved"], true);

    let encoded = serde_json::to_value(&selected).unwrap();
    assert_eq!(encoded["mcpToolCall"]["server"], "mcp-サーバー-🦀");
    assert_eq!(encoded["mcpToolCall"]["tool"], "検索/工具/🛠️");
    assert!(encoded["mcpToolCall"].get("futureLabel").is_none());
    assert_eq!(encoded["futureEventField"]["preserved"], true);

    let without_metadata = serde_json::to_value(&result.events[0]).unwrap();
    assert!(without_metadata.get("mcpToolCall").is_none());

    for invalid in [
        serde_json::json!({"server": "only-server"}),
        serde_json::json!({"tool": "only-tool"}),
        serde_json::json!({"server": "", "tool": "tool"}),
        serde_json::json!({"server": "server", "tool": ""}),
        serde_json::json!({"server": "server", "tool": "tool", "futureLabel": true}),
        serde_json::json!({
            "server": "server",
            "tool": "a".repeat(MAX_MCP_TOOL_CALL_COMPONENT_BYTES + 1)
        }),
    ] {
        assert!(serde_json::from_value::<McpToolCall>(invalid).is_err());
    }
    assert!(
        serde_json::from_value::<AgentHistoryEvent>(serde_json::json!({"mcpToolCall": null}))
            .is_err()
    );

    let exact = serde_json::json!({
        "server": " ",
        "tool": "🦀".repeat(MAX_MCP_TOOL_CALL_COMPONENT_BYTES / 4)
    });
    let exact: McpToolCall = serde_json::from_value(exact).unwrap();
    assert_eq!(exact.tool.len(), MAX_MCP_TOOL_CALL_COMPONENT_BYTES);

    let invalid_for_encoding = McpToolCall {
        server: "server".to_owned(),
        tool: String::new(),
    };
    assert!(serde_json::to_value(invalid_for_encoding).is_err());
}

#[test]
fn mcp_exchange_is_typed_lossless_bounded_and_shape_validated() {
    let fixture = fs::read_to_string(fixture_root().join("show-event.mcp-tool-call.json")).unwrap();
    let envelope: AgentHistoryEnvelope = serde_json::from_str(&fixture).unwrap();
    let result = envelope.event.unwrap();
    let selected = result.event.unwrap();
    let exchange = selected.mcp_exchange.as_ref().unwrap();
    assert_eq!(exchange.provider_call_id, "native-call-呼び出し-🦀");

    let invocation = exchange.invocation.as_ref().unwrap();
    let McpJsonCapture::Present { value: arguments } = &invocation.arguments else {
        panic!("fixture arguments must be present");
    };
    assert_eq!(arguments["snake_key"][0], "雪");
    assert!(arguments["snake_key"][1].is_null());
    assert!(arguments["nested"]["items"][1]["deep_null"].is_null());

    let response = exchange.response.as_ref().unwrap();
    assert_eq!(response.status, McpResponseStatus::Succeeded);
    assert_eq!(response.duration_ns, Some(MAX_SAFE_INTEGER));
    assert_eq!(response.text, McpTextCapture::NormalizedBody);
    let McpJsonCapture::Present { value: payload } = &response.payload else {
        panic!("fixture payload must be present");
    };
    assert_eq!(payload["result_key"][0], "完了");
    assert!(payload["result_key"][1].is_null());

    let encoded = serde_json::to_value(&selected).unwrap();
    assert_eq!(
        encoded["mcpExchange"]["invocation"]["arguments"]["value"],
        *arguments
    );
    assert_eq!(
        encoded["mcpExchange"]["response"]["payload"]["value"],
        *payload
    );
    assert!(encoded.get("mcp_exchange").is_none());

    assert!(result.events[0].mcp_exchange.is_none());
    let capture_states = result.events[1].mcp_exchange.as_ref().unwrap();
    assert_eq!(
        capture_states.invocation.as_ref().unwrap().arguments,
        McpJsonCapture::Absent
    );
    let capture_response = capture_states.response.as_ref().unwrap();
    assert_eq!(capture_response.text, McpTextCapture::Absent);
    assert_eq!(capture_response.payload, McpJsonCapture::Unavailable);

    let omitted = result.events[2].mcp_exchange.as_ref().unwrap();
    let omitted_response = omitted.response.as_ref().unwrap();
    assert_eq!(omitted_response.status, McpResponseStatus::Failed);
    assert_eq!(
        omitted_response.failure_kind,
        Some(McpFailureKind::ToolReported)
    );
    assert_eq!(
        omitted_response.text,
        McpTextCapture::Omitted {
            reason: McpPayloadOmissionReason::SizeLimit,
            observed_encoded_bytes: Some(MAX_SAFE_INTEGER),
        }
    );
    assert_eq!(
        omitted_response.payload,
        McpJsonCapture::Omitted {
            reason: McpPayloadOmissionReason::SizeLimit,
            observed_encoded_bytes: None,
        }
    );
    let encoded_omitted = serde_json::to_value(&result.events[2]).unwrap();
    assert_eq!(
        encoded_omitted["mcpExchange"]["response"]["text"]["observedEncodedBytes"],
        MAX_SAFE_INTEGER
    );
    assert!(encoded_omitted["mcpExchange"]["response"]["text"]
        .get("observed_encoded_bytes")
        .is_none());
    assert!(result.events[3].mcp_exchange.is_none());

    for (index, invalid) in [
        serde_json::json!({"mcpExchange": null}),
        serde_json::json!({"mcpExchange": {"providerCallId": "call"}}),
        serde_json::json!({
            "mcpExchange": {
                "providerCallId": "",
                "response": {
                    "status": "succeeded",
                    "text": {"captureStatus": "absent"},
                    "payload": {"captureStatus": "absent"}
                }
            }
        }),
        serde_json::json!({
            "mcpExchange": {
                "providerCallId": "call",
                "response": {
                    "status": "succeeded",
                    "durationNs": MAX_SAFE_INTEGER + 1,
                    "text": {"captureStatus": "absent"},
                    "payload": {"captureStatus": "absent"}
                }
            }
        }),
        serde_json::json!({
            "mcpExchange": {
                "providerCallId": "call",
                "response": {
                    "status": "succeeded",
                    "text": {
                        "captureStatus": "omitted",
                        "reason": "size_limit",
                        "observedEncodedBytes": MAX_SAFE_INTEGER + 1
                    },
                    "payload": {"captureStatus": "absent"}
                }
            }
        }),
        serde_json::json!({
            "mcpExchange": {
                "providerCallId": "call",
                "invocation": null,
                "response": {
                    "status": "succeeded",
                    "text": {"captureStatus": "absent"},
                    "payload": {"captureStatus": "absent"}
                }
            }
        }),
        serde_json::json!({
            "mcpExchange": {
                "providerCallId": "call",
                "invocation": {
                    "server": "server",
                    "tool": "tool",
                    "arguments": {"captureStatus": "present", "value": null}
                }
            }
        }),
        serde_json::json!({
            "mcpExchange": {
                "providerCallId": "call",
                "response": {
                    "status": "failed",
                    "text": {"captureStatus": "absent"},
                    "payload": {"captureStatus": "absent"}
                }
            }
        }),
        serde_json::json!({
            "mcpExchange": {
                "providerCallId": "call",
                "response": {
                    "status": "succeeded",
                    "failureKind": "unknown",
                    "text": {"captureStatus": "absent"},
                    "payload": {"captureStatus": "absent"}
                }
            }
        }),
        serde_json::json!({
            "mcpExchange": {
                "providerCallId": "call",
                "response": {
                    "status": "succeeded",
                    "text": {"captureStatus": "absent", "future": true},
                    "payload": {"captureStatus": "absent"}
                }
            }
        }),
        serde_json::json!({
            "mcpExchange": {
                "providerCallId": "call",
                "response": {
                    "status": "succeeded",
                    "text": {"captureStatus": "absent"},
                    "payload": {"captureStatus": "absent"}
                },
                "future": true
            }
        }),
        serde_json::json!({
            "mcpExchange": {
                "providerCallId": "x".repeat(MAX_MCP_EXCHANGE_IDENTITY_BYTES + 1),
                "response": {
                    "status": "succeeded",
                    "text": {"captureStatus": "absent"},
                    "payload": {"captureStatus": "absent"}
                }
            }
        }),
        serde_json::json!({
            "mcpExchange": {
                "providerCallId": "call",
                "invocation": {
                    "server": "x".repeat(MAX_MCP_EXCHANGE_IDENTITY_BYTES + 1),
                    "tool": "tool",
                    "arguments": {"captureStatus": "absent"}
                }
            }
        }),
    ]
    .into_iter()
    .enumerate()
    {
        assert!(
            serde_json::from_value::<AgentHistoryEvent>(invalid.clone()).is_err(),
            "invalid MCP exchange fixture {index} decoded: {invalid}"
        );
    }

    let exact: McpExchange = serde_json::from_value(serde_json::json!({
        "providerCallId": "🦀".repeat(MAX_MCP_EXCHANGE_IDENTITY_BYTES / 4),
        "invocation": {
            "server": " ",
            "tool": "tool",
            "arguments": {"captureStatus": "absent"}
        }
    }))
    .unwrap();
    assert_eq!(
        exact.provider_call_id.len(),
        MAX_MCP_EXCHANGE_IDENTITY_BYTES
    );

    let invalid_for_encoding = McpExchange {
        provider_call_id: "call".to_owned(),
        invocation: None,
        response: None,
    };
    assert!(serde_json::to_value(invalid_for_encoding).is_err());
}

#[test]
fn mcp_exchange_direct_decode_rejects_duplicate_captured_json_and_bad_event_text() {
    for name in [
        "duplicate-mcp-exchange-captured-value.json",
        "invalid-mcp-exchange-normalized-body-missing-event-text.json",
        "invalid-mcp-exchange-normalized-body-empty-event-text.json",
        "invalid-mcp-exchange-unsafe-duration-ns.json",
        "invalid-mcp-exchange-unsafe-observed-encoded-bytes.json",
    ] {
        let fixture = fs::read_to_string(fixture_root().join("adversarial").join(name)).unwrap();
        assert!(
            serde_json::from_str::<EventResult>(&fixture).is_err(),
            "direct protocol decode accepted {name}"
        );
    }
}

#[test]
fn status_counters_accept_the_exact_cross_sdk_maximum() {
    let status: AgentHistoryStatus = serde_json::from_value(serde_json::json!({
        "initialized": true,
        "localOnly": true,
        "indexedItems": MAX_SAFE_STATUS_COUNTER,
        "indexedSessions": MAX_SAFE_STATUS_COUNTER,
        "indexedEvents": MAX_SAFE_STATUS_COUNTER,
        "indexedSources": MAX_SAFE_STATUS_COUNTER
    }))
    .unwrap();

    assert_eq!(status.indexed_items, Some(MAX_SAFE_STATUS_COUNTER));
    assert_eq!(status.indexed_sessions, Some(MAX_SAFE_STATUS_COUNTER));
    assert_eq!(status.indexed_events, Some(MAX_SAFE_STATUS_COUNTER));
    assert_eq!(status.indexed_sources, Some(MAX_SAFE_STATUS_COUNTER));
    serde_json::to_value(status).unwrap();
}

#[test]
fn status_counters_reject_values_above_the_exact_cross_sdk_maximum() {
    for rejected in [MAX_SAFE_STATUS_COUNTER + 2, u64::MAX] {
        let error = serde_json::from_value::<AgentHistoryStatus>(serde_json::json!({
            "initialized": true,
            "localOnly": true,
            "indexedItems": rejected
        }))
        .unwrap_err();
        assert!(
            error.to_string().contains("status counter exceeds maximum"),
            "{error}"
        );
    }

    let mut status: AgentHistoryStatus = serde_json::from_value(serde_json::json!({
        "initialized": true,
        "localOnly": true
    }))
    .unwrap();
    status.indexed_items = Some(MAX_SAFE_STATUS_COUNTER + 2);
    let error = serde_json::to_value(status).unwrap_err();
    assert!(
        error.to_string().contains("status counter exceeds maximum"),
        "{error}"
    );
}

#[test]
fn camelizes_private_cli_keys_recursively() {
    let raw = serde_json::json!({
        "payload_type": "search_results",
        "generated_at": "now",
        "results": [{
            "record_type": "event",
            "item_type": "event",
            "ctx_event_id": "event",
            "result_type": "event",
            "result_scope": "event",
            "source_format": "codex_session_jsonl",
            "citations": [{
                "target_type": "event"
            }]
        }]
    });
    let camel = camelize_object_keys(&raw);
    assert!(camel.get("payloadType").is_none());
    assert_eq!(camel["generatedAt"], "now");
    assert!(camel["results"][0].get("recordType").is_none());
    assert!(camel["results"][0].get("itemType").is_none());
    assert_eq!(camel["results"][0]["ctxEventId"], "event");
    assert_eq!(camel["results"][0]["resultType"], "event");
    assert_eq!(camel["results"][0]["citations"][0]["targetType"], "event");
    assert_eq!(camel["results"][0]["sourceFormat"], "codex_session_jsonl");
}

#[test]
fn event_structured_json_preserves_present_null() {
    for source in [
        serde_json::json!({"structuredContent":null}),
        serde_json::json!({}),
    ] {
        let event: AgentHistoryEvent = serde_json::from_value(source.clone()).unwrap();
        assert_eq!(
            event.structured_content.as_ref(),
            source.get("structuredContent")
        );
        assert_eq!(
            serde_json::to_value(event)
                .unwrap()
                .get("structuredContent"),
            source.get("structuredContent")
        );
    }
}
