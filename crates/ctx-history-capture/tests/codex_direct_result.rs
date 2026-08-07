use std::{fs, io::Write, path::Path};

use ctx_history_capture::{
    provider_source_for_path, refresh_source_backed_generation,
    register_landed_source_backed_route, SourceBackedProviderRegistry, SourceBackedRefreshReceipt,
    SourceBackedRouteSelection,
};
use ctx_history_core::{
    CaptureProvider, CoreRecord, McpFailureKind, McpJsonCapture, McpPayloadOmissionReason,
    McpTerminalStatus, McpTextCapture, McpToolCallAttribution,
};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use serde_json::{json, Value};

const ATTRIBUTION_SERVER_CANARY: &str = "zzsrvattributioncanary7e41qphx";
const ATTRIBUTION_TOOL_CANARY: &str = "zztoolattributioncanary9b26wkmd";
const INVOCATION_ARGUMENT_CANARY: &str = "zzinvocationargumentcanary5c83rmqv";

fn write_session(root: &Path, native_session_id: &str, events: &[Value]) {
    fs::create_dir_all(root).unwrap();
    let mut lines = vec![json!({
        "timestamp": "2026-08-01T12:00:00Z",
        "type": "session_meta",
        "payload": {
            "id": native_session_id,
            "timestamp": "2026-08-01T12:00:00Z",
            "cwd": "/workspace",
            "source": "cli"
        }
    })];
    lines.extend_from_slice(events);
    let mut contents = lines
        .into_iter()
        .map(|line| serde_json::to_string(&line).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    contents.push('\n');
    fs::write(
        root.join(format!("rollout-{native_session_id}.jsonl")),
        contents,
    )
    .unwrap();
}

fn publish_codex_sessions(session_root: &Path, index_root: &Path) -> SourceBackedRefreshReceipt {
    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        provider_source_for_path(CaptureProvider::Codex, session_root.to_path_buf()),
        SourceBackedRouteSelection::ExplicitManual,
    )
    .unwrap();
    refresh_source_backed_generation(index_root, &registry, WriterOptions::default()).unwrap()
}

fn mcp_result(call_id: &str, result: Value) -> Value {
    mcp_result_with_invocation(
        call_id,
        json!({
            "server": "example",
            "tool": "read",
            "arguments": {"path": "/workspace/result.txt"}
        }),
        result,
    )
}

fn mcp_result_with_invocation(call_id: &str, invocation: Value, result: Value) -> Value {
    json!({
        "timestamp": "2026-08-01T12:00:01Z",
        "type": "event_msg",
        "payload": {
            "type": "mcp_tool_call_end",
            "call_id": call_id,
            "invocation": invocation,
            "duration": {"secs": 1, "nanos": 7},
            "result": result
        }
    })
}

fn mcp_result_with_duration(call_id: &str, duration: Value, result: Value) -> Value {
    let mut event = mcp_result(call_id, result);
    event["payload"]["duration"] = duration;
    event
}

fn core_for_marker(index: &VerifiedIndex, marker: &str) -> CoreRecord {
    let candidate = index
        .search_event_candidates(marker, 10)
        .unwrap()
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("missing indexed marker {marker}"));
    index
        .core_record_by_id(candidate.event.event_id.as_uuid())
        .unwrap()
        .unwrap()
}

#[test]
fn over_8_mib_mcp_result_is_admitted_once_and_indexable() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let native_session_id = "019fa000-0000-7000-8000-000000000024";
    let result_tail = "mcp_large_result_tail_indexable";
    let full_result = format!("{} {result_tail}", "x".repeat(9 * 1024 * 1024));
    assert!(full_result.len() > 8 * 1024 * 1024);
    write_session(
        &sessions,
        native_session_id,
        &[mcp_result(
            "exec-mcp-large",
            json!({
                "Ok": {
                    "content": [{"type": "text", "text": full_result}],
                    "isError": false,
                    "_meta": {"surface": "fixture"}
                }
            }),
        )],
    );

    publish_codex_sessions(&sessions, &index);
    let verified = VerifiedIndex::open(&index).unwrap();
    let candidate = verified
        .search_event_candidates(result_tail, 10)
        .unwrap()
        .into_iter()
        .next()
        .expect("large result tail is indexed");
    let core = verified
        .core_record_by_id(candidate.event.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(
        core.content.normalized_body.as_deref(),
        Some(full_result.as_str())
    );
    let structured = core.content.structured_content.as_ref().unwrap();
    assert_eq!(
        structured["provider_native_tool_result"]["result_variant"],
        "Ok"
    );
    assert_eq!(
        structured["provider_native_tool_result"]["result_metadata"]["_meta"]["surface"],
        "fixture"
    );
    let encoded = serde_json::to_string(structured).unwrap();
    assert!(encoded.len() < 4 * 1024);
    assert!(!encoded.contains(result_tail));
    assert_eq!(
        core.mcp_tool_call,
        Some(McpToolCallAttribution {
            server: "example".to_owned(),
            tool: "read".to_owned(),
        })
    );
    let exchange = core.content.mcp_exchange.as_ref().unwrap();
    assert_eq!(exchange.provider_call_id, "exec-mcp-large");
    let invocation = exchange.invocation.as_ref().unwrap();
    assert_eq!(invocation.server, "example");
    assert_eq!(invocation.tool, "read");
    assert_eq!(
        invocation.arguments,
        McpJsonCapture::Present {
            value: json!({"path": "/workspace/result.txt"})
        }
    );
    let response = exchange.response.as_ref().unwrap();
    assert_eq!(response.status, McpTerminalStatus::Succeeded);
    assert_eq!(response.failure_kind, None);
    assert_eq!(response.duration_ns, Some(1_000_000_007));
    assert_eq!(response.text, McpTextCapture::NormalizedBody);
    assert!(matches!(
        &response.payload,
        McpJsonCapture::Omitted {
            reason: McpPayloadOmissionReason::SizeLimit,
            observed_encoded_bytes: Some(bytes),
        } if *bytes > 8 * 1024 * 1024
    ));
}

#[test]
fn malformed_mcp_results_are_rejected_without_hiding_later_valid_content() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let native_session_id = "019fa000-0000-7000-8000-000000000025";
    let rejected_marker = "rejectonlyzzqv7421";
    let tool_error_marker = "toolerrorokqjx8137";
    let protocol_error_marker = "protocolerrorokmzn9264";
    let implicit_success_marker = "missingiserrorsucceedszzqv7421";
    let valid_marker = "later_valid_content_is_indexed";
    let malformed = [
        json!({
            "Ok": {"content": [{"type": "text", "text": rejected_marker}]},
            "Err": "ambiguous wrapper"
        }),
        json!({"Unknown": {"content": []}}),
        json!({"Ok": null}),
        json!({"Ok": {"content": "not an array"}}),
        json!({"Ok": {"content": [{"type": "text"}]}}),
        json!({"Ok": {"content": [], "isError": "not a boolean"}}),
        json!({"Err": null}),
    ];
    let mut events = malformed
        .into_iter()
        .enumerate()
        .map(|(index, result)| mcp_result(&format!("exec-mcp-malformed-{index}"), result))
        .collect::<Vec<_>>();
    events.push(mcp_result(
        "exec-mcp-tool-error",
        json!({
            "Ok": {
                "content": [{"type": "text", "text": tool_error_marker}],
                "isError": true
            }
        }),
    ));
    events.push(mcp_result(
        "exec-mcp-protocol-error",
        json!({"Err": protocol_error_marker}),
    ));
    events.push(mcp_result(
        "exec-mcp-implicit-success",
        json!({
            "Ok": {
                "content": [{"type": "text", "text": implicit_success_marker}]
            }
        }),
    ));
    events.push(json!({
        "timestamp": "2026-08-01T12:00:02Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": valid_marker}]
        }
    }));
    write_session(&sessions, native_session_id, &events);

    let receipt = publish_codex_sessions(&sessions, &index);
    assert_eq!(receipt.commit.indexed_documents, 4);
    let verified = VerifiedIndex::open(&index).unwrap();
    assert!(verified
        .search_event_candidates(rejected_marker, 10)
        .unwrap()
        .is_empty());
    assert_eq!(
        verified
            .search_event_candidates(tool_error_marker, 10)
            .unwrap()
            .len(),
        1
    );
    for marker in [tool_error_marker, protocol_error_marker] {
        assert_eq!(
            core_for_marker(&verified, marker).mcp_tool_call,
            Some(McpToolCallAttribution {
                server: "example".to_owned(),
                tool: "read".to_owned(),
            })
        );
    }
    let tool_error = core_for_marker(&verified, tool_error_marker);
    let response = tool_error
        .content
        .mcp_exchange
        .as_ref()
        .unwrap()
        .response
        .as_ref()
        .unwrap();
    assert_eq!(response.status, McpTerminalStatus::Failed);
    assert_eq!(response.failure_kind, Some(McpFailureKind::ToolReported));
    assert_eq!(response.duration_ns, Some(1_000_000_007));
    assert!(matches!(&response.payload, McpJsonCapture::Present { .. }));

    let protocol_error = core_for_marker(&verified, protocol_error_marker);
    let response = protocol_error
        .content
        .mcp_exchange
        .as_ref()
        .unwrap()
        .response
        .as_ref()
        .unwrap();
    assert_eq!(response.status, McpTerminalStatus::Failed);
    assert_eq!(response.failure_kind, Some(McpFailureKind::Invocation));
    assert_eq!(
        response.payload,
        McpJsonCapture::Present {
            value: Value::String(protocol_error_marker.to_owned())
        }
    );

    let implicit_success = core_for_marker(&verified, implicit_success_marker);
    let response = implicit_success
        .content
        .mcp_exchange
        .as_ref()
        .unwrap()
        .response
        .as_ref()
        .unwrap();
    assert_eq!(response.status, McpTerminalStatus::Succeeded);
    assert_eq!(response.failure_kind, None);
    assert_eq!(
        response.payload,
        McpJsonCapture::Present {
            value: json!({
                "content": [{"type": "text", "text": implicit_success_marker}]
            })
        }
    );
    assert_eq!(
        verified
            .search_event_candidates(protocol_error_marker, 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        verified
            .search_event_candidates(valid_marker, 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn redacted_real_shape_fixture_is_admitted_with_linkage_and_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("rollout-redacted-mcp-direct-result.jsonl"),
        include_str!(
            "../src/provider/codex/nativepath/tests/fixtures/mcp_tool_call_end_direct_result.jsonl"
        ),
    )
    .unwrap();

    let receipt = publish_codex_sessions(&sessions, &index);
    assert_eq!(receipt.commit.indexed_documents, 1);
    let verified = VerifiedIndex::open(&index).unwrap();
    let candidate = verified
        .search_event_candidates("REAL_SHAPE_DIRECT_RESULT", 10)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let core = verified
        .core_record_by_id(candidate.event.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(
        core.content.normalized_body.as_deref(),
        Some("REAL_SHAPE_DIRECT_RESULT")
    );
    let native = &core.content.structured_content.as_ref().unwrap()["provider_native_tool_result"];
    assert_eq!(native["call_id"], "exec-redacted-real-shape");
    assert_eq!(native["result_variant"], "Ok");
    assert_eq!(native["result_metadata"]["isError"], false);
    assert_eq!(
        native["result_metadata"]["_meta"]["codex/toolSurface"]["kind"],
        "browserUse"
    );
    assert_eq!(native["invocation"]["server"], "node_repl");
    assert_eq!(native["invocation"]["tool"], "js");
    assert_eq!(
        core.mcp_tool_call,
        Some(McpToolCallAttribution {
            server: "node_repl".to_owned(),
            tool: "js".to_owned(),
        })
    );
    let exchange = core.content.mcp_exchange.as_ref().unwrap();
    assert_eq!(exchange.provider_call_id, "exec-redacted-real-shape");
    let invocation = exchange.invocation.as_ref().unwrap();
    assert_eq!(invocation.server, "node_repl");
    assert_eq!(invocation.tool, "js");
    assert_eq!(
        invocation.arguments,
        McpJsonCapture::Present {
            value: json!({
                "code": "/* redacted */",
                "title": "Redacted fixture",
                "timeout_ms": 30000
            })
        }
    );
    let response = exchange.response.as_ref().unwrap();
    assert_eq!(response.status, McpTerminalStatus::Succeeded);
    assert_eq!(response.failure_kind, None);
    assert_eq!(response.duration_ns, Some(916_426_054));
    assert_eq!(response.text, McpTextCapture::NormalizedBody);
    assert_eq!(
        response.payload,
        McpJsonCapture::Present {
            value: json!({
                "content": [{"type": "text", "text": "REAL_SHAPE_DIRECT_RESULT"}],
                "isError": false,
                "_meta": {
                    "browser_use": {},
                    "codex/toolSurface": {"backend": "iab", "kind": "browserUse"}
                }
            })
        }
    );

    let initial_event_id = core.event_id;
    let initial_exchange = core.content.mcp_exchange.clone();
    let initial_generation = receipt.commit.generation_id;
    drop(verified);
    let replay = publish_codex_sessions(&sessions, &index);
    assert_eq!(replay.commit.generation_id, initial_generation);
    let replayed = VerifiedIndex::open(&index).unwrap();
    let current = core_for_marker(&replayed, "REAL_SHAPE_DIRECT_RESULT");
    assert_eq!(current.event_id, initial_event_id);
    assert_eq!(current.content.mcp_exchange, initial_exchange);
    assert_eq!(
        current.content.normalized_body.as_deref(),
        Some("REAL_SHAPE_DIRECT_RESULT")
    );
}

#[test]
fn exact_error_attribution_and_ambiguous_pair_abstention_survive_publication() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("rollout-redacted-mcp-attribution.jsonl"),
        include_str!(
            "../src/provider/codex/nativepath/tests/fixtures/mcp_tool_call_attribution_adversarial.jsonl"
        ),
    )
    .unwrap();

    let receipt = publish_codex_sessions(&sessions, &index);
    assert_eq!(receipt.commit.indexed_documents, 14);
    let verified = VerifiedIndex::open(&index).unwrap();
    let exact = core_for_marker(&verified, "EXACT_MCP_OK");
    assert_eq!(
        exact.mcp_tool_call,
        Some(McpToolCallAttribution {
            server: "srv__/雪\u{1}::opaque".to_owned(),
            tool: "tool//λ__name\u{2}".to_owned(),
        })
    );
    for marker in ["EXACT_MCP_TOOL_ERROR", "EXACT_MCP_NATIVE_ERR"] {
        assert_eq!(
            core_for_marker(&verified, marker).mcp_tool_call,
            Some(McpToolCallAttribution {
                server: "node_repl".to_owned(),
                tool: "js".to_owned(),
            })
        );
    }
    for marker in ["AMBIGUOUS_MCP_SERVER", "AMBIGUOUS_MCP_INVOCATION"] {
        let core = core_for_marker(&verified, marker);
        assert!(core.mcp_tool_call.is_none());
        assert_eq!(core.content.normalized_body.as_deref(), Some(marker));
        let exchange = core.content.mcp_exchange.as_ref().unwrap();
        assert!(exchange.invocation.is_none());
        assert_eq!(
            exchange.response.as_ref().unwrap().status,
            McpTerminalStatus::Succeeded
        );
    }
    for marker in [
        "AMBIGUOUS_MCP_RESULT",
        "DUPLICATE_SAME_FIRST",
        "DUPLICATE_SAME_SECOND",
        "DUPLICATE_CONFLICT_FIRST",
        "DUPLICATE_CONFLICT_SECOND",
        "SEQUENTIAL_REUSE_FIRST",
        "SEQUENTIAL_REUSE_SECOND",
        "AMBIGUOUS_MCP_TERMINAL_SELECTORS",
    ] {
        let core = core_for_marker(&verified, marker);
        assert!(
            core.mcp_tool_call.is_none(),
            "unexpected attribution for {marker}"
        );
        assert_eq!(core.content.normalized_body.as_deref(), Some(marker));
        if marker == "AMBIGUOUS_MCP_TERMINAL_SELECTORS" {
            assert!(core.content.mcp_exchange.is_none());
        } else {
            assert!(core.content.mcp_exchange.is_some());
        }
    }
    let ambiguous_result = core_for_marker(&verified, "AMBIGUOUS_MCP_RESULT");
    let response = ambiguous_result
        .content
        .mcp_exchange
        .as_ref()
        .unwrap()
        .response
        .as_ref()
        .unwrap();
    assert_eq!(response.status, McpTerminalStatus::Unknown);
    assert_eq!(response.failure_kind, None);
    assert_eq!(response.payload, McpJsonCapture::Unavailable);
    assert_eq!(
        core_for_marker(&verified, "SEQUENTIAL_REUSE_SEPARATOR")
            .content
            .normalized_body
            .as_deref(),
        Some("SEQUENTIAL_REUSE_SEPARATOR")
    );
}

#[test]
fn nested_duplicate_json_is_unavailable_without_losing_terminal_text() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let native_session_id = "019fa000-0000-7000-8000-000000000031";
    let arguments_marker = "duplicate_nested_arguments_body_survives";
    let payload_marker = "duplicate_nested_payload_body_survives";
    let status_marker = "duplicate_is_error_body_survives";
    fs::create_dir_all(&sessions).unwrap();
    let contents = format!(
        concat!(
            "{{\"timestamp\":\"2026-08-01T12:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"{native_session_id}\",\"timestamp\":\"2026-08-01T12:00:00Z\",\"cwd\":\"/workspace\",\"source\":\"cli\"}}}}\n",
            "{{\"timestamp\":\"2026-08-01T12:00:01Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"mcp_tool_call_end\",\"call_id\":\"exec-duplicate-arguments\",\"invocation\":{{\"server\":\"example\",\"tool\":\"read\",\"arguments\":{{\"path\":\"first\",\"path\":\"second\"}}}},\"duration\":{{\"secs\":1,\"nanos\":7}},\"result\":{{\"Ok\":{{\"content\":[{{\"type\":\"text\",\"text\":\"{arguments_marker}\"}}],\"isError\":false}}}}}}}}\n",
            "{{\"timestamp\":\"2026-08-01T12:00:02Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"mcp_tool_call_end\",\"call_id\":\"exec-duplicate-payload\",\"invocation\":{{\"server\":\"example\",\"tool\":\"read\",\"arguments\":{{}}}},\"duration\":{{\"secs\":1,\"nanos\":7}},\"result\":{{\"Ok\":{{\"content\":[{{\"type\":\"text\",\"text\":\"{payload_marker}\"}}],\"isError\":false,\"_meta\":{{\"surface\":1,\"surface\":2}}}}}}}}}}\n",
            "{{\"timestamp\":\"2026-08-01T12:00:03Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"mcp_tool_call_end\",\"call_id\":\"exec-duplicate-status\",\"invocation\":{{\"server\":\"example\",\"tool\":\"read\",\"arguments\":{{}}}},\"duration\":{{\"secs\":1,\"nanos\":7}},\"result\":{{\"Ok\":{{\"content\":[{{\"type\":\"text\",\"text\":\"{status_marker}\"}}],\"isError\":false,\"isError\":true}}}}}}}}\n"
        ),
        native_session_id = native_session_id,
        arguments_marker = arguments_marker,
        payload_marker = payload_marker,
        status_marker = status_marker,
    );
    fs::write(
        sessions.join(format!("rollout-{native_session_id}.jsonl")),
        contents,
    )
    .unwrap();

    let receipt = publish_codex_sessions(&sessions, &index);
    assert_eq!(receipt.commit.indexed_documents, 3);
    let verified = VerifiedIndex::open(&index).unwrap();

    let arguments = core_for_marker(&verified, arguments_marker);
    assert_eq!(
        arguments.content.normalized_body.as_deref(),
        Some(arguments_marker)
    );
    let exchange = arguments.content.mcp_exchange.as_ref().unwrap();
    assert_eq!(
        exchange.invocation.as_ref().unwrap().arguments,
        McpJsonCapture::Unavailable
    );
    let response = exchange.response.as_ref().unwrap();
    assert_eq!(response.status, McpTerminalStatus::Succeeded);
    assert!(matches!(&response.payload, McpJsonCapture::Present { .. }));

    let payload = core_for_marker(&verified, payload_marker);
    assert_eq!(
        payload.content.normalized_body.as_deref(),
        Some(payload_marker)
    );
    let response = payload
        .content
        .mcp_exchange
        .as_ref()
        .unwrap()
        .response
        .as_ref()
        .unwrap();
    assert_eq!(response.status, McpTerminalStatus::Succeeded);
    assert_eq!(response.payload, McpJsonCapture::Unavailable);

    let status = core_for_marker(&verified, status_marker);
    assert_eq!(
        status.content.normalized_body.as_deref(),
        Some(status_marker)
    );
    let response = status
        .content
        .mcp_exchange
        .as_ref()
        .unwrap()
        .response
        .as_ref()
        .unwrap();
    assert_eq!(response.status, McpTerminalStatus::Unknown);
    assert_eq!(response.failure_kind, None);
    assert_eq!(response.payload, McpJsonCapture::Unavailable);
}

#[test]
fn malformed_duplicate_terminals_abstain_without_losing_public_content_or_ids() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let native_session_id = "019fa000-0000-7000-8000-000000000029";
    let result_call_id = "exec-mcp-malformed-result-duplicate";
    let duration_call_id = "exec-mcp-malformed-duration-duplicate";
    let result_marker = "valid_after_malformed_result_is_published";
    let duration_marker = "valid_before_malformed_duration_is_published";
    let neighbor_before_marker = "malformed_duplicate_neighbor_before_is_attributed";
    let neighbor_after_marker = "malformed_duplicate_neighbor_after_is_attributed";
    let malformed_result_marker = "zzrejectresultqv7421";
    let malformed_duration_marker = "zzrejectdurationmzn9264";
    write_session(
        &sessions,
        native_session_id,
        &[
            mcp_result(
                "exec-mcp-neighbor-before",
                json!({"Err": neighbor_before_marker}),
            ),
            mcp_result(
                result_call_id,
                json!({"Ok": {"content": malformed_result_marker}}),
            ),
            mcp_result(result_call_id, json!({"Err": result_marker})),
            mcp_result(duration_call_id, json!({"Err": duration_marker})),
            mcp_result_with_duration(
                duration_call_id,
                json!({"secs": 1, "nanos": 1_000_000_000_u64}),
                json!({"Err": malformed_duration_marker}),
            ),
            mcp_result(
                "exec-mcp-neighbor-after",
                json!({"Err": neighbor_after_marker}),
            ),
        ],
    );

    let receipt = publish_codex_sessions(&sessions, &index);
    assert_eq!(receipt.commit.indexed_documents, 4);
    let verified = VerifiedIndex::open(&index).unwrap();
    let result = core_for_marker(&verified, result_marker);
    let duration = core_for_marker(&verified, duration_marker);
    for core in [&result, &duration] {
        assert!(core.mcp_tool_call.is_none());
        assert_eq!(
            core.content.structured_content.as_ref().unwrap()["provider_native_tool_result"]
                ["result_variant"],
            "Err"
        );
    }
    for marker in [neighbor_before_marker, neighbor_after_marker] {
        assert_eq!(
            core_for_marker(&verified, marker).mcp_tool_call,
            Some(McpToolCallAttribution {
                server: "example".to_owned(),
                tool: "read".to_owned(),
            })
        );
    }
    for marker in [malformed_result_marker, malformed_duration_marker] {
        assert!(verified
            .search_event_candidates(marker, 10)
            .unwrap()
            .is_empty());
    }
    let stable = [
        core_for_marker(&verified, neighbor_before_marker),
        result,
        duration,
        core_for_marker(&verified, neighbor_after_marker),
    ];
    drop(verified);

    publish_codex_sessions(&sessions, &index);
    let republished = VerifiedIndex::open(&index).unwrap();
    for prior in stable {
        let marker = prior.content.normalized_body.as_deref().unwrap();
        let current = core_for_marker(&republished, marker);
        assert_eq!(current.event_id, prior.event_id);
        assert_eq!(current.session_id, prior.session_id);
        assert_eq!(current.source, prior.source);
        assert_eq!(current.native_event_id, prior.native_event_id);
        assert_eq!(current.event_sequence, prior.event_sequence);
        assert_eq!(
            current.content.normalized_body,
            prior.content.normalized_body
        );
    }
}

#[test]
fn invalid_attribution_preserves_terminal_content_and_all_stable_identities() {
    let temp = tempfile::tempdir().unwrap();
    let exact_sessions = temp.path().join("exact-sessions");
    let invalid_sessions = temp.path().join("invalid-sessions");
    let exact_index = temp.path().join("exact-index");
    let invalid_index = temp.path().join("invalid-index");
    let native_session_id = "019fa000-0000-7000-8000-000000000026";
    let marker = "same_terminal_result_survives_invalid_attribution";
    let result = json!({
        "Ok": {
            "content": [{"type": "text", "text": marker}],
            "isError": false
        }
    });
    write_session(
        &exact_sessions,
        native_session_id,
        &[mcp_result_with_invocation(
            "exec-mcp-stable-identity",
            json!({"server": "example", "tool": "read", "arguments": {}}),
            result.clone(),
        )],
    );
    write_session(
        &invalid_sessions,
        native_session_id,
        &[mcp_result_with_invocation(
            "exec-mcp-stable-identity",
            json!({"server": "example", "tool": ["read"], "arguments": {}}),
            result,
        )],
    );

    publish_codex_sessions(&exact_sessions, &exact_index);
    publish_codex_sessions(&invalid_sessions, &invalid_index);
    let exact_verified = VerifiedIndex::open(&exact_index).unwrap();
    let invalid_verified = VerifiedIndex::open(&invalid_index).unwrap();
    let exact = core_for_marker(&exact_verified, marker);
    let invalid = core_for_marker(&invalid_verified, marker);

    assert_eq!(exact.event_id, invalid.event_id);
    assert_eq!(exact.session_id, invalid.session_id);
    assert_eq!(exact.source, invalid.source);
    assert_eq!(exact.native_event_id, invalid.native_event_id);
    assert_eq!(
        exact.content.normalized_body,
        invalid.content.normalized_body
    );
    assert_eq!(
        exact.parser_revision,
        "codex-nativepath-core-record-v21-transitive-root-normalization"
    );
    assert_eq!(exact.parser_revision, invalid.parser_revision);
    assert_eq!(
        exact.mcp_tool_call,
        Some(McpToolCallAttribution {
            server: "example".to_owned(),
            tool: "read".to_owned(),
        })
    );
    assert!(invalid.mcp_tool_call.is_none());
}

#[test]
fn exact_raw_limit_omits_oversized_arguments_but_publishes_result() {
    const MAX_CODEX_RECORD_BYTES: usize = 16 * 1024 * 1024;

    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let native_session_id = "019fa000-0000-7000-8000-000000000027";
    let marker = "near_limit_terminal_result_survives";
    let mut event = json!({
        "type": "event_msg",
        "payload": {
            "type": "mcp_tool_call_end",
            "call_id": "exec-mcp-near-limit",
            "invocation": {
                "server": "example",
                "tool": "read",
                "arguments": {"padding": ""}
            },
            "duration": {"secs": 1, "nanos": 7},
            "result": {"Err": marker}
        }
    });
    let base_len = serde_json::to_vec(&event).unwrap().len();
    let padding_len = MAX_CODEX_RECORD_BYTES.checked_sub(base_len).unwrap();
    event["payload"]["invocation"]["arguments"]["padding"] = Value::String("p".repeat(padding_len));
    assert_eq!(
        serde_json::to_vec(&event).unwrap().len(),
        MAX_CODEX_RECORD_BYTES
    );
    write_session(&sessions, native_session_id, &[event]);

    let receipt = publish_codex_sessions(&sessions, &index);
    assert_eq!(receipt.commit.indexed_documents, 1);
    let verified = VerifiedIndex::open(&index).unwrap();
    let core = core_for_marker(&verified, marker);
    assert_eq!(core.content.normalized_body.as_deref(), Some(marker));
    assert_eq!(
        core.mcp_tool_call,
        Some(McpToolCallAttribution {
            server: "example".to_owned(),
            tool: "read".to_owned(),
        })
    );
    let structured = core.content.structured_content.as_ref().unwrap();
    let native = &structured["provider_native_tool_result"];
    assert_eq!(native["result_variant"], "Err");
    assert!(native.get("invocation").is_none());
    assert!(
        serde_json::to_vec(structured).unwrap().len() <= ctx_history_core::MAX_CORE_CONTENT_BYTES
    );
    let exchange = core.content.mcp_exchange.as_ref().unwrap();
    let invocation = exchange.invocation.as_ref().unwrap();
    assert_eq!(invocation.server, "example");
    assert_eq!(invocation.tool, "read");
    assert!(matches!(
        &invocation.arguments,
        McpJsonCapture::Omitted {
            reason: McpPayloadOmissionReason::SizeLimit,
            observed_encoded_bytes: Some(bytes),
        } if *bytes > 15 * 1024 * 1024
    ));
    let response = exchange.response.as_ref().unwrap();
    assert_eq!(response.status, McpTerminalStatus::Failed);
    assert_eq!(response.failure_kind, Some(McpFailureKind::Invocation));
    assert_eq!(
        response.payload,
        McpJsonCapture::Present {
            value: Value::String(marker.to_owned())
        }
    );
    assert!(
        core.content.encoded_content_bytes().unwrap() <= ctx_history_core::MAX_CORE_CONTENT_BYTES
    );
}

#[test]
fn appended_duplicate_terminal_retracts_prior_attribution_and_preserves_ids() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let native_session_id = "019fa000-0000-7000-8000-000000000028";
    let first_marker = "append_duplicate_first_result_survives";
    let second_marker = "append_duplicate_second_result_survives";
    let call_id = "exec-mcp-append-duplicate";
    write_session(
        &sessions,
        native_session_id,
        &[mcp_result_with_invocation(
            call_id,
            json!({"server": "first", "tool": "read", "arguments": {}}),
            json!({"Err": first_marker}),
        )],
    );

    publish_codex_sessions(&sessions, &index);
    let initial = VerifiedIndex::open(&index).unwrap();
    let initial_core = core_for_marker(&initial, first_marker);
    let initial_event_id = initial_core.event_id;
    let initial_session_id = initial_core.session_id;
    let initial_source = initial_core.source.clone();
    let initial_native_event_id = initial_core.native_event_id.clone();
    let initial_event_sequence = initial_core.event_sequence;
    assert!(initial_core.mcp_tool_call.is_some());
    drop(initial);

    let path = sessions.join(format!("rollout-{native_session_id}.jsonl"));
    let duplicate = mcp_result_with_invocation(
        call_id,
        json!({"server": "second", "tool": "write", "arguments": {}}),
        json!({"Err": second_marker}),
    );
    let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(file, "{}", serde_json::to_string(&duplicate).unwrap()).unwrap();
    drop(file);

    let receipt = publish_codex_sessions(&sessions, &index);
    assert_eq!(receipt.commit.indexed_documents, 2);
    let verified = VerifiedIndex::open(&index).unwrap();
    let first = core_for_marker(&verified, first_marker);
    let second = core_for_marker(&verified, second_marker);
    assert_eq!(first.event_id, initial_event_id);
    assert_eq!(first.session_id, initial_session_id);
    assert_eq!(first.source, initial_source);
    assert_eq!(first.native_event_id, initial_native_event_id);
    assert_eq!(first.event_sequence, initial_event_sequence);
    assert_ne!(first.event_id, second.event_id);
    assert!(first.event_sequence < second.event_sequence);
    assert!(first.mcp_tool_call.is_none());
    assert!(second.mcp_tool_call.is_none());
    assert_eq!(first.content.normalized_body.as_deref(), Some(first_marker));
    assert_eq!(
        second.content.normalized_body.as_deref(),
        Some(second_marker)
    );
}

#[test]
fn appended_malformed_duplicate_retracts_attribution_without_touching_neighbor_ids() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let native_session_id = "019fa000-0000-7000-8000-000000000030";
    let call_id = "exec-mcp-append-malformed-duplicate";
    let target_marker = "append_malformed_duplicate_target_survives";
    let neighbor_marker = "append_malformed_duplicate_neighbor_survives";
    let malformed_result_marker = "zzappendrejectresultbk5138";
    let malformed_duration_marker = "zzappendrejectdurationhp8042";
    write_session(
        &sessions,
        native_session_id,
        &[
            mcp_result(call_id, json!({"Err": target_marker})),
            mcp_result(
                "exec-mcp-append-malformed-neighbor",
                json!({"Err": neighbor_marker}),
            ),
        ],
    );

    publish_codex_sessions(&sessions, &index);
    let initial = VerifiedIndex::open(&index).unwrap();
    let initial_target = core_for_marker(&initial, target_marker);
    let initial_neighbor = core_for_marker(&initial, neighbor_marker);
    assert!(initial_target.mcp_tool_call.is_some());
    assert!(initial_neighbor.mcp_tool_call.is_some());
    drop(initial);

    let path = sessions.join(format!("rollout-{native_session_id}.jsonl"));
    let malformed = [
        mcp_result(call_id, json!({"Ok": {"content": malformed_result_marker}})),
        mcp_result_with_duration(
            call_id,
            json!({"secs": "1", "nanos": 7}),
            json!({"Err": malformed_duration_marker}),
        ),
    ];
    let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
    for event in malformed {
        writeln!(file, "{}", serde_json::to_string(&event).unwrap()).unwrap();
    }
    drop(file);

    let receipt = publish_codex_sessions(&sessions, &index);
    assert_eq!(receipt.commit.indexed_documents, 2);
    let verified = VerifiedIndex::open(&index).unwrap();
    let target = core_for_marker(&verified, target_marker);
    let neighbor = core_for_marker(&verified, neighbor_marker);
    for (current, prior) in [(&target, &initial_target), (&neighbor, &initial_neighbor)] {
        assert_eq!(current.event_id, prior.event_id);
        assert_eq!(current.session_id, prior.session_id);
        assert_eq!(current.source, prior.source);
        assert_eq!(current.native_event_id, prior.native_event_id);
        assert_eq!(current.event_sequence, prior.event_sequence);
        assert_eq!(
            current.content.normalized_body,
            prior.content.normalized_body
        );
    }
    assert!(target.mcp_tool_call.is_none());
    assert_eq!(neighbor.mcp_tool_call, initial_neighbor.mcp_tool_call);
    assert_eq!(
        target.content.structured_content.as_ref().unwrap()["provider_native_tool_result"]
            ["result_variant"],
        "Err"
    );
    for marker in [malformed_result_marker, malformed_duration_marker] {
        assert!(verified
            .search_event_candidates(marker, 10)
            .unwrap()
            .is_empty());
    }
}

#[test]
fn codex_dual_layer_mcp_metadata_and_exchange_invocation_keep_search_contract() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let native_session_id = "019fa000-0000-7000-8000-000000000026";
    let body = "mcp attribution ranking body oracle";
    write_session(
        &sessions,
        native_session_id,
        &[
            mcp_result_with_invocation(
                "exec-mcp-attribution-search-canary",
                json!({
                    "server": ATTRIBUTION_SERVER_CANARY,
                    "tool": ATTRIBUTION_TOOL_CANARY,
                    "arguments": {"path": INVOCATION_ARGUMENT_CANARY}
                }),
                json!({
                    "Ok": {
                        "content": [{"type": "text", "text": body}],
                        "isError": false
                    }
                }),
            ),
            json!({
                "timestamp": "2026-08-01T12:00:02Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call_output",
                    "call_id": "exec-non-mcp-ranking-control",
                    "output": body
                }
            }),
        ],
    );

    let receipt = publish_codex_sessions(&sessions, &index);
    assert_eq!(receipt.commit.indexed_documents, 2);
    let verified = VerifiedIndex::open(&index).unwrap();
    let candidates = verified.search_event_candidates(body, 10).unwrap();
    assert_eq!(candidates.len(), 2);

    let attributed = candidates
        .iter()
        .filter_map(|candidate| {
            verified
                .core_record_by_id(candidate.event.event_id.as_uuid())
                .unwrap()
        })
        .find(|core| core.mcp_tool_call.is_some())
        .expect("MCP result remains available through its body match");
    let attributed_id = attributed.event_id;
    assert_eq!(
        attributed.mcp_tool_call,
        Some(McpToolCallAttribution {
            server: ATTRIBUTION_SERVER_CANARY.to_owned(),
            tool: ATTRIBUTION_TOOL_CANARY.to_owned(),
        })
    );
    let invocation = attributed
        .content
        .mcp_exchange
        .as_ref()
        .unwrap()
        .invocation
        .as_ref()
        .unwrap();
    assert_eq!(invocation.server, ATTRIBUTION_SERVER_CANARY);
    assert_eq!(invocation.tool, ATTRIBUTION_TOOL_CANARY);
    assert_eq!(
        invocation.arguments,
        McpJsonCapture::Present {
            value: json!({"path": INVOCATION_ARGUMENT_CANARY})
        }
    );

    for canary in [
        ATTRIBUTION_SERVER_CANARY,
        ATTRIBUTION_TOOL_CANARY,
        INVOCATION_ARGUMENT_CANARY,
    ] {
        let invocation_matches = verified.search_event_candidates(canary, 10).unwrap();
        assert_eq!(
            invocation_matches.len(),
            1,
            "selected MCP invocation data was not narrowly searchable: {canary}"
        );
        assert_eq!(
            invocation_matches[0].event.event_id, attributed_id,
            "selected MCP invocation data matched the wrong event: {canary}"
        );
    }
}
