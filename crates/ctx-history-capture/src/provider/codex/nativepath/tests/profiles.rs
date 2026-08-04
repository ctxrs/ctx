use super::*;

#[test]
fn catalog_discovery_is_deterministic_and_rejects_bad_exact_observations() {
    let (_temp, path) = write_source(&session_meta("catalog-owner"));
    let valid = catalog_session(&path, "catalog-owner");
    let mut wrong_provider = valid.clone();
    wrong_provider.provider = CaptureProvider::Claude;
    let mut missing_token = valid.clone();
    missing_token.source_path = path
        .with_file_name("missing-token.jsonl")
        .display()
        .to_string();
    missing_token.metadata = json!({});

    let discovery =
        discover_codex_catalog_sources(&[wrong_provider, missing_token, valid.clone(), valid]);
    assert_eq!(discovery.ineligible, 1);
    assert!(discovery.sources.is_empty());
    assert_eq!(
        discovery
            .rejections
            .iter()
            .map(|rejection| rejection.reason)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "Codex catalog change token is missing",
            "duplicate Codex catalog path",
        ])
    );
}

#[test]
fn raw_ordinals_include_headers_outputs_malformed_and_ignored_records() {
    let contents = [
        session_meta("ordinal-owner"),
        message("user", "first retained"),
        tool_output("call-1", "excluded body"),
        "{malformed json}\n".to_owned(),
        tool_call("call-2"),
        jsonl(json!({"type": "turn_context", "payload": {"cwd": "/workspace/turn"}})),
        message("assistant", "last retained"),
    ]
    .concat();
    let (_temp, path) = write_source(&contents);
    let (scan, sink) = scan_collect(discover_one(&path, "ordinal-owner"), None);

    assert_eq!(
        sink.rows
            .iter()
            .map(|row| row.raw_ordinal)
            .collect::<Vec<_>>(),
        vec![1, 2, 4, 6]
    );
    assert_eq!(scan.next_raw_ordinal, 7);
    assert_eq!(scan.counters.complete_records, 7);
    assert_eq!(scan.counters.native_result_records, 1);
    assert_eq!(scan.counters.malformed_records, 1);
    assert_eq!(scan.counters.rejected_complete_records, 1);
    assert_eq!(
        sink.rows
            .iter()
            .map(|row| row.session_cwd.as_deref())
            .collect::<Vec<_>>(),
        vec![
            Some("/workspace"),
            Some("/workspace"),
            Some("/workspace"),
            Some("/workspace/turn")
        ]
    );
}

#[test]
fn valid_retained_records_without_indexable_text_are_ignored_not_rejected() {
    let contents = [
        session_meta("unmaterialized-owner"),
        jsonl(json!({
            "timestamp": "2026-01-01T00:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "reasoning",
                "encrypted_content": "opaque",
                "summary": []
            }
        })),
        jsonl(json!({
            "timestamp": "2026-01-01T00:00:02Z",
            "type": "compacted",
            "payload": {
                "message": "replacement history is source-only",
                "replacement_history": []
            }
        })),
    ]
    .concat();
    let (_temp, path) = write_source(&contents);
    let (scan, sink) = scan_collect(discover_one(&path, "unmaterialized-owner"), None);

    assert!(sink.rows.is_empty());
    assert_eq!(scan.counters.rejected_complete_records, 0);
    assert_eq!(scan.counters.complete_records, 3);
    assert_eq!(scan.counters.ignored_records, 2);
    assert_eq!(scan.counters.malformed_records, 0);
}

#[test]
fn malformed_retained_shapes_remain_rejected_without_hiding_later_records() {
    let contents = [
        session_meta("malformed-retained-owner"),
        jsonl(json!({
            "timestamp": "2026-01-01T00:00:01Z",
            "type": "response_item",
            "payload": {"type": "message", "role": "user"}
        })),
        jsonl(json!({
            "timestamp": "2026-01-01T00:00:02Z",
            "type": "response_item",
            "payload": {"type": "message", "role": "operator", "content": "invalid role"}
        })),
        jsonl(json!({
            "timestamp": "2026-01-01T00:00:03Z",
            "type": "response_item",
            "payload": {"type": "reasoning", "summary": []}
        })),
        jsonl(json!({
            "timestamp": "2026-01-01T00:00:04Z",
            "type": "response_item",
            "payload": {
                "type": "reasoning",
                "encrypted_content": "opaque",
                "summary": {"unexpected": "shape"}
            }
        })),
        jsonl(json!({
            "timestamp": "2026-01-01T00:00:05Z",
            "type": "compacted",
            "payload": {"message": "missing replacement history"}
        })),
        jsonl(json!({
            "timestamp": "2026-01-01T00:00:06Z",
            "type": "compacted",
            "payload": {
                "message": "invalid replacement history",
                "replacement_history": "not an array"
            }
        })),
    ]
    .concat();
    let (_temp, path) = write_source(&contents);
    let (scan, sink) = scan_collect(discover_one(&path, "malformed-retained-owner"), None);

    assert!(sink.rows.is_empty());
    assert_eq!(scan.counters.complete_records, 7);
    assert_eq!(scan.counters.ignored_records, 0);
    assert_eq!(scan.counters.malformed_records, 6);
    assert_eq!(scan.counters.rejected_complete_records, 6);
}

#[test]
fn source_backed_row_preserves_full_lexical_text_and_native_identity() {
    let header = session_meta("direct-core-owner");
    let complete_message = format!("MESSAGE_BEGIN-{}-MESSAGE_END", "m".repeat(20_000));
    let message_record = message("assistant", &complete_message);
    let contents = [header, message_record.clone()].concat();
    let (_temp, path) = write_source(&contents);
    let (scan, sink) = scan_collect(discover_one(&path, "direct-core-owner"), None);

    assert_eq!(scan.counters.rejected_complete_records, 0);
    assert_eq!(sink.rows.len(), 1);
    let row = &sink.rows[0];
    assert_eq!(row.raw_ordinal, 1);
    assert!(row.lexical_body.contains("MESSAGE_BEGIN"));
    assert!(row.lexical_body.ends_with("MESSAGE_END"));
    assert_eq!(row.session_cwd.as_deref(), Some("/workspace"));
}

#[test]
fn output_heavy_scan_retains_complete_result_bodies() {
    let secret = "RESULT_ONLY_MARKER_".repeat(32_768);
    let contents = [
        session_meta("output-owner"),
        message("user", "small request"),
        tool_call("call-output"),
        tool_output("call-output", &secret),
        message("assistant", "small response"),
    ]
    .concat();
    let (_temp, path) = write_source(&contents);
    let (scan, sink) = scan_collect(discover_one(&path, "output-owner"), None);

    assert_eq!(sink.rows.len(), 4);
    assert_eq!(scan.counters.native_result_records, 1);
    assert!(scan.counters.native_result_record_bytes > secret.len() as u64);
    assert_eq!(scan.counters.structural_json_parses, 5);
    assert_eq!(scan.counters.structural_output_probes, 1);
    assert_eq!(scan.counters.typed_json_parses, 5);
    assert_eq!(scan.counters.retained_json_parses, 3);
    assert_eq!(sink.rows[2].lexical_body, secret);
    assert!(format!("{:?}", sink.rows).contains("RESULT_ONLY_MARKER_"));
}

#[test]
fn mcp_direct_result_minimal_synthetic_retains_text_and_linkage() {
    let call_id = "exec-mcp-direct";
    let output = "direct MCP result survives";
    let contents = [
        session_meta("mcp-direct-owner"),
        jsonl(json!({
            "timestamp": "2026-01-01T00:00:03Z",
            "type": "event_msg",
            "payload": {
                "type": "mcp_tool_call_end",
                "call_id": call_id,
                "invocation": {
                    "server": "example",
                    "tool": "read",
                    "arguments": {"path": "/workspace/input.txt"}
                },
                "duration": {"secs": 0, "nanos": 42},
                "result": {
                    "Ok": {
                        "content": [{"type": "text", "text": output}],
                        "isError": false
                    }
                }
            }
        })),
    ]
    .concat();
    let (_temp, path) = write_source(&contents);
    let (scan, sink) = scan_collect(discover_one(&path, "mcp-direct-owner"), None);

    assert_eq!(scan.counters.rejected_complete_records, 0);
    assert_eq!(sink.rows.len(), 1);
    let row = &sink.rows[0];
    assert_eq!(row.event_type, EventType::ToolOutput);
    assert_eq!(row.lexical_body, output);
    let identity = row.provider_event_identity.as_ref().unwrap();
    assert_eq!(identity.kind.as_str(), "call_id");
    assert_eq!(identity.value, call_id);
    let native = &row.structured_content.as_ref().unwrap()["provider_native_tool_result"];
    assert_eq!(native["item_type"], "tool_result");
    assert_eq!(native["call_id"], call_id);
    assert_eq!(native["result_variant"], "Ok");
    assert_eq!(native["result_content_location"], "normalized_body");
    assert_eq!(native["result_content_complete"], true);
    assert_eq!(native["result_metadata"]["content"][0]["type"], "text");
    assert_eq!(native["invocation"]["server"], "example");
    assert!(!serde_json::to_string(native).unwrap().contains(output));
    assert_eq!(
        row.mcp_tool_call.as_ref().unwrap(),
        &ctx_history_core::McpToolCallAttribution {
            server: "example".to_owned(),
            tool: "read".to_owned(),
        }
    );
}

#[test]
fn versioned_mcp_terminal_results_abstain_without_dropping_the_event() {
    for cli_version in ["0.200.0", "0.201.0", "0.202.0", "999.0.0"] {
        let owner = format!("versioned-mcp-{cli_version}");
        let output = format!("terminal result from {cli_version}");
        let contents = [
            jsonl(json!({
                "timestamp": "2026-01-01T00:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": owner,
                    "timestamp": "2026-01-01T00:00:00Z",
                    "cwd": "/workspace",
                    "source": "cli",
                    "cli_version": cli_version
                }
            })),
            jsonl(json!({
                "timestamp": "2026-01-01T00:00:03Z",
                "type": "event_msg",
                "payload": {
                    "type": "mcp_tool_call_end",
                    "call_id": format!("exec-mcp-{cli_version}"),
                    "invocation": {
                        "server": "versioned-server",
                        "tool": "versioned-tool",
                        "arguments": {}
                    },
                    "duration": {"secs": 0, "nanos": 42},
                    "result": {
                        "Ok": {
                            "content": [{"type": "text", "text": output}],
                            "isError": false
                        }
                    }
                }
            })),
        ]
        .concat();
        let (_temp, path) = write_source(&contents);
        let (scan, sink) = scan_collect(discover_one(&path, &owner), None);

        assert_eq!(scan.counters.rejected_complete_records, 0, "{cli_version}");
        assert_eq!(sink.rows.len(), 1, "{cli_version}");
        let row = &sink.rows[0];
        assert_eq!(row.event_type, EventType::ToolOutput, "{cli_version}");
        assert_eq!(row.lexical_body, output, "{cli_version}");
        assert!(row.structured_content.is_some(), "{cli_version}");
        assert!(row.mcp_tool_call.is_none(), "{cli_version}");
    }
}

#[test]
fn mcp_direct_result_retains_complete_multi_block_content() {
    let first = format!("MCP_RESULT_BEGIN-{}", "a".repeat(24_000));
    let second = format!("{}-MCP_RESULT_END", "b".repeat(24_000));
    let expected = format!("{first}\n{second}");
    let contents = [
        session_meta("mcp-complete-owner"),
        jsonl(json!({
            "timestamp": "2026-01-01T00:00:03Z",
            "type": "event_msg",
            "payload": {
                "type": "mcp_tool_call_end",
                "call_id": "exec-mcp-complete",
                "invocation": {"server": "example", "tool": "read", "arguments": {}},
                "duration": {"secs": 0, "nanos": 42},
                "result": {
                    "Ok": {
                        "content": [
                            {"type": "text", "text": first},
                            {"type": "text", "text": second}
                        ]
                    }
                }
            }
        })),
    ]
    .concat();
    let (_temp, path) = write_source(&contents);
    let (scan, sink) = scan_collect(discover_one(&path, "mcp-complete-owner"), None);

    assert_eq!(scan.counters.rejected_complete_records, 0);
    assert_eq!(sink.rows.len(), 1);
    assert_eq!(sink.rows[0].lexical_body, expected);
    let native = &sink.rows[0].structured_content.as_ref().unwrap()["provider_native_tool_result"];
    assert_eq!(native["result_variant"], "Ok");
    assert_eq!(native["result_metadata"]["content"][0]["type"], "text");
    assert_eq!(native["result_metadata"]["content"][1]["type"], "text");
    let encoded = serde_json::to_string(native).unwrap();
    assert!(!encoded.contains("MCP_RESULT_BEGIN"));
    assert!(!encoded.contains("MCP_RESULT_END"));
}

#[test]
fn mcp_direct_result_accepts_native_error_variants() {
    let contents = [
        session_meta("mcp-error-owner"),
        jsonl(json!({
            "timestamp": "2026-01-01T00:00:03Z",
            "type": "event_msg",
            "payload": {
                "type": "mcp_tool_call_end",
                "call_id": "exec-mcp-tool-error",
                "invocation": {"server": "example", "tool": "read", "arguments": {}},
                "duration": {"secs": 0, "nanos": 42},
                "result": {
                    "Ok": {
                        "content": [{"type": "text", "text": "tool-level failure"}],
                        "isError": true
                    }
                }
            }
        })),
        jsonl(json!({
            "timestamp": "2026-01-01T00:00:04Z",
            "type": "event_msg",
            "payload": {
                "type": "mcp_tool_call_end",
                "call_id": "exec-mcp-protocol-error",
                "invocation": {"server": "example", "tool": "read", "arguments": {}},
                "duration": {"secs": 0, "nanos": 43},
                "result": {"Err": "protocol-level failure"}
            }
        })),
    ]
    .concat();
    let (_temp, path) = write_source(&contents);
    let (scan, sink) = scan_collect(discover_one(&path, "mcp-error-owner"), None);

    assert_eq!(scan.counters.rejected_complete_records, 0);
    assert_eq!(sink.rows.len(), 2);
    assert_eq!(sink.rows[0].lexical_body, "tool-level failure");
    assert_eq!(sink.rows[1].lexical_body, "protocol-level failure");
    assert!(sink.rows.iter().all(|row| {
        row.mcp_tool_call.as_ref()
            == Some(&ctx_history_core::McpToolCallAttribution {
                server: "example".to_owned(),
                tool: "read".to_owned(),
            })
    }));
    assert_eq!(
        sink.rows[1].structured_content.as_ref().unwrap()["provider_native_tool_result"]
            ["result_variant"],
        "Err"
    );
    assert!(!serde_json::to_string(&sink.rows[1].structured_content)
        .unwrap()
        .contains("protocol-level failure"));
}

#[test]
fn malformed_mcp_attribution_never_drops_an_ordinary_terminal_result() {
    let oversized = "x".repeat(ctx_history_core::MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES + 1);
    let cases = [
        ("missing-invocation", None),
        ("wrong-invocation", Some(json!(false))),
        (
            "missing-server",
            Some(json!({"tool": "read", "arguments": {}})),
        ),
        (
            "missing-tool",
            Some(json!({"server": "example", "arguments": {}})),
        ),
        (
            "wrong-server",
            Some(json!({"server": 7, "tool": "read", "arguments": {}})),
        ),
        (
            "wrong-tool",
            Some(json!({"server": "example", "tool": ["read"], "arguments": {}})),
        ),
        (
            "empty-server",
            Some(json!({"server": "", "tool": "read", "arguments": {}})),
        ),
        (
            "empty-tool",
            Some(json!({"server": "example", "tool": "", "arguments": {}})),
        ),
        (
            "oversized-server",
            Some(json!({"server": oversized, "tool": "read", "arguments": {}})),
        ),
        (
            "oversized-tool",
            Some(json!({"server": "example", "tool": oversized, "arguments": {}})),
        ),
    ];
    let mut contents = session_meta("mcp-invalid-attribution-owner");
    for (index, (marker, invocation)) in cases.iter().enumerate() {
        let mut payload = json!({
            "type": "mcp_tool_call_end",
            "call_id": format!("exec-mcp-invalid-attribution-{index}"),
            "duration": {"secs": 0, "nanos": 42},
            "result": {
                "Ok": {
                    "content": [{"type": "text", "text": marker}],
                    "isError": false
                }
            }
        });
        if let Some(invocation) = invocation {
            payload
                .as_object_mut()
                .unwrap()
                .insert("invocation".to_owned(), invocation.clone());
        }
        contents.push_str(&jsonl(json!({
            "timestamp": "2026-01-01T00:00:03Z",
            "type": "event_msg",
            "payload": payload,
        })));
    }
    let (_temp, path) = write_source(&contents);
    let (scan, sink) = scan_collect(discover_one(&path, "mcp-invalid-attribution-owner"), None);

    assert_eq!(scan.counters.rejected_complete_records, 0);
    assert_eq!(sink.rows.len(), cases.len());
    assert!(sink.rows.iter().all(|row| row.mcp_tool_call.is_none()));
    assert_eq!(
        sink.rows
            .iter()
            .map(|row| row.lexical_body.as_str())
            .collect::<Vec<_>>(),
        cases.iter().map(|(marker, _)| *marker).collect::<Vec<_>>()
    );
    assert_eq!(
        sink.rows[1].structured_content.as_ref().unwrap()["provider_native_tool_result"]
            ["invocation"],
        false
    );
}

#[test]
fn exact_mcp_attribution_preserves_opaque_names_and_component_bound() {
    let exact_server = "  srv__/雪\u{1}::opaque  ";
    let exact_tool = "tool//λ__name\u{2}";
    let max_server = "m".repeat(ctx_history_core::MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES);
    let contents = [
        session_meta("mcp-exact-attribution-owner"),
        jsonl(json!({
            "timestamp": "2026-01-01T00:00:03Z",
            "type": "event_msg",
            "payload": {
                "type": "mcp_tool_call_end",
                "call_id": "exec-mcp-opaque",
                "invocation": {
                    "server": exact_server,
                    "tool": exact_tool,
                    "arguments": {}
                },
                "duration": {"secs": 0, "nanos": 42},
                "result": {"Ok": {"content": [{"type": "text", "text": "opaque"}]}}
            }
        })),
        jsonl(json!({
            "timestamp": "2026-01-01T00:00:04Z",
            "type": "event_msg",
            "payload": {
                "type": "mcp_tool_call_end",
                "call_id": "exec-mcp-max",
                "invocation": {
                    "server": max_server,
                    "tool": "max-tool",
                    "arguments": {}
                },
                "duration": {"secs": 0, "nanos": 43},
                "result": {"Err": "max-bound"}
            }
        })),
    ]
    .concat();
    let (_temp, path) = write_source(&contents);
    let (scan, sink) = scan_collect(discover_one(&path, "mcp-exact-attribution-owner"), None);

    assert_eq!(scan.counters.rejected_complete_records, 0);
    assert_eq!(sink.rows.len(), 2);
    let attributed_owned_bytes = sink.rows[0].estimated_owned_bytes().unwrap();
    let mut unattributed = sink.rows[0].clone();
    unattributed.mcp_tool_call = None;
    let unattributed_owned_bytes = unattributed.estimated_owned_bytes().unwrap();
    assert!(
        attributed_owned_bytes.saturating_sub(unattributed_owned_bytes)
            >= exact_server.len() + exact_tool.len()
    );
    assert_eq!(
        sink.rows[0].mcp_tool_call.as_ref().unwrap(),
        &ctx_history_core::McpToolCallAttribution {
            server: exact_server.to_owned(),
            tool: exact_tool.to_owned(),
        }
    );
    assert_eq!(
        sink.rows[1].mcp_tool_call.as_ref().unwrap().server.len(),
        ctx_history_core::MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES
    );
    assert!(sink
        .pages
        .iter()
        .all(|(_, bytes)| { *bytes <= MAX_CODEX_SOURCE_BACKED_SINGLE_ROW_PAGE_BYTES }));
}

#[test]
fn malformed_or_ambiguous_mcp_direct_results_are_rejected_locally() {
    let malformed_results = [
        json!({}),
        json!({"Ok": {"content": []}, "Err": "ambiguous"}),
        json!({"Unknown": {"content": []}}),
        json!({"Ok": null}),
        json!({"Ok": {"content": "not an array"}}),
        json!({"Ok": {"content": [{"type": "text"}]}}),
        json!({"Err": null}),
    ];
    let mut contents = session_meta("mcp-malformed-owner");
    for (index, result) in malformed_results.iter().enumerate() {
        contents.push_str(&jsonl(json!({
            "timestamp": "2026-01-01T00:00:03Z",
            "type": "event_msg",
            "payload": {
                "type": "mcp_tool_call_end",
                "call_id": format!("exec-mcp-malformed-{index}"),
                "invocation": {"server": "example", "tool": "read", "arguments": {}},
                "duration": {"secs": 0, "nanos": 42},
                "result": result
            }
        })));
    }
    contents.push_str(&message("assistant", "later valid record"));
    let (_temp, path) = write_source(&contents);
    let (scan, sink) = scan_collect(discover_one(&path, "mcp-malformed-owner"), None);

    assert_eq!(sink.rows.len(), 1);
    assert_eq!(sink.rows[0].lexical_body, "later valid record");
    assert_eq!(
        scan.counters.malformed_records,
        malformed_results.len() as u64
    );
    assert_eq!(
        scan.counters.rejected_complete_records,
        malformed_results.len() as u64
    );
}

#[test]
fn redacted_real_shape_fixture_retains_mcp_direct_result() {
    let contents = include_str!("fixtures/mcp_tool_call_end_direct_result.jsonl");
    let (_temp, path) = write_source(contents);
    let (scan, sink) = scan_collect(discover_one(&path, "redacted-mcp-direct-result"), None);

    assert_eq!(scan.counters.rejected_complete_records, 0);
    assert_eq!(sink.rows.len(), 1);
    assert_eq!(sink.rows[0].lexical_body, "REAL_SHAPE_DIRECT_RESULT");
    let native = &sink.rows[0].structured_content.as_ref().unwrap()["provider_native_tool_result"];
    assert_eq!(native["call_id"], "exec-redacted-real-shape");
    assert_eq!(native["result_variant"], "Ok");
    assert_eq!(native["result_metadata"]["isError"], false);
    assert_eq!(
        native["result_metadata"]["_meta"]["codex/toolSurface"]["kind"],
        "browserUse"
    );
    assert_eq!(
        sink.rows[0].mcp_tool_call.as_ref().unwrap(),
        &ctx_history_core::McpToolCallAttribution {
            server: "node_repl".to_owned(),
            tool: "js".to_owned(),
        }
    );
}

#[test]
fn synthetic_adversarial_fixture_requires_source_unique_terminal_authority() {
    let contents = include_str!("fixtures/mcp_tool_call_attribution_adversarial.jsonl");
    let (_temp, path) = write_source(contents);
    let (scan, sink) = scan_collect(discover_one(&path, "redacted-mcp-attribution"), None);

    assert_eq!(scan.counters.rejected_complete_records, 0);
    assert_eq!(sink.rows.len(), 14);
    assert_eq!(scan.counters.peak_mcp_terminal_authority_entries, 10);
    assert!(scan.counters.peak_mcp_terminal_authority_bytes > 10 * 32);
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
        let row = sink
            .rows
            .iter()
            .find(|row| row.lexical_body == marker)
            .unwrap_or_else(|| panic!("missing terminal marker {marker}"));
        assert!(
            row.mcp_tool_call.is_none(),
            "unexpected attribution for {marker}"
        );
    }
    assert_eq!(
        sink.rows
            .iter()
            .filter(|row| row.mcp_tool_call.is_some())
            .count(),
        3
    );
}

#[test]
fn malformed_same_id_mcp_terminals_abstain_before_and_after_valid_results() {
    fn terminal(call_id: &str, duration: Value, result: Value) -> String {
        jsonl(json!({
            "timestamp": "2026-08-01T12:00:01Z",
            "type": "event_msg",
            "payload": {
                "type": "mcp_tool_call_end",
                "call_id": call_id,
                "invocation": {"server": "example", "tool": "read", "arguments": {}},
                "duration": duration,
                "result": result
            }
        }))
    }

    fn valid_terminal(call_id: &str, marker: &str) -> String {
        terminal(
            call_id,
            json!({"secs": 0, "nanos": 7}),
            json!({"Err": marker}),
        )
    }

    let cases = [
        (
            "neighbor-before",
            "mcp_malformed_duplicate_neighbor_before",
            true,
        ),
        (
            "same-id-result-before",
            "mcp_valid_after_malformed_result",
            false,
        ),
        (
            "same-id-duration-before",
            "mcp_valid_after_malformed_duration",
            false,
        ),
        (
            "same-id-result-after",
            "mcp_valid_before_malformed_result",
            false,
        ),
        (
            "same-id-duration-after",
            "mcp_valid_before_malformed_duration",
            false,
        ),
        (
            "neighbor-after",
            "mcp_malformed_duplicate_neighbor_after",
            true,
        ),
    ];
    let contents = [
        session_meta("mcp-malformed-duplicate-owner"),
        valid_terminal(cases[0].0, cases[0].1),
        terminal(
            cases[1].0,
            json!({"secs": 0, "nanos": 7}),
            json!({"Ok": {"content": "malformed_result_before_rejected"}}),
        ),
        valid_terminal(cases[1].0, cases[1].1),
        terminal(
            cases[2].0,
            json!({"secs": "0", "nanos": 7}),
            json!({"Err": "malformed_duration_before_rejected"}),
        ),
        valid_terminal(cases[2].0, cases[2].1),
        valid_terminal(cases[3].0, cases[3].1),
        terminal(
            cases[3].0,
            json!({"secs": 0, "nanos": 7}),
            json!({"Ok": {"content": "malformed_result_after_rejected"}}),
        ),
        valid_terminal(cases[4].0, cases[4].1),
        terminal(
            cases[4].0,
            json!({"secs": 0, "nanos": 1_000_000_000_u64}),
            json!({"Err": "malformed_duration_after_rejected"}),
        ),
        valid_terminal(cases[5].0, cases[5].1),
    ]
    .concat();
    let (_temp, path) = write_source(&contents);
    let (scan, sink) = scan_collect(discover_one(&path, "mcp-malformed-duplicate-owner"), None);

    assert_eq!(scan.counters.malformed_records, 4);
    assert_eq!(scan.counters.rejected_complete_records, 4);
    assert_eq!(scan.counters.peak_mcp_terminal_authority_entries, 6);
    assert_eq!(sink.rows.len(), cases.len());
    for (row, (call_id, marker, attributed)) in sink.rows.iter().zip(cases) {
        assert_eq!(row.lexical_body, marker);
        let identity = row.provider_event_identity.as_ref().unwrap();
        assert_eq!(identity.kind.as_str(), "call_id");
        assert_eq!(identity.value, call_id);
        assert_eq!(row.mcp_tool_call.is_some(), attributed, "{marker}");
        assert_eq!(
            row.structured_content.as_ref().unwrap()["provider_native_tool_result"]
                ["result_variant"],
            "Err"
        );
    }
}

#[test]
fn source_backed_projection_prefilters_with_exact_scan_accounting() {
    let ignored = jsonl(json!({
        "type": "event_msg",
        "payload": {"type": "token_count", "info": {"total_token_usage": {"input_tokens": 42}}}
    }));
    let ineligible_result = jsonl(json!({
        "type": "event_msg",
        "payload": {"type": "patch_apply_end", "success": true}
    }));
    let retained = message("user", "source-backed retained body");
    let contents = [
        session_meta("source-backed-prefilter-owner"),
        ignored,
        ineligible_result.clone(),
        retained.clone(),
    ]
    .concat();
    let (_temp, path) = write_source(&contents);
    let (scan, sink) = scan_collect(discover_one(&path, "source-backed-prefilter-owner"), None);

    assert_eq!(sink.rows.len(), 1);
    assert_eq!(sink.pages.len(), 1);
    assert_eq!(scan.counters.complete_records, 4);
    assert_eq!(scan.counters.retained_records, 1);
    assert_eq!(scan.counters.ignored_records, 1);
    assert_eq!(scan.counters.native_result_records, 1);
    assert_eq!(
        scan.counters.native_result_record_bytes,
        ineligible_result.len() as u64
    );
    assert_eq!(scan.counters.prefiltered_records, 1);
    assert_eq!(scan.counters.structural_json_parses, 3);
    assert_eq!(scan.counters.typed_json_parses, 2);

    let row = &sink.rows[0];
    assert_eq!(row.raw_ordinal, 3);
    assert_eq!(row.session_cwd.as_deref(), Some("/workspace"));
    assert_eq!(scan.counters.legacy_body_json_serializations, 0);
    assert_eq!(scan.counters.legacy_row_json_serializations, 0);
    assert_eq!(scan.counters.legacy_json_serialized_bytes, 0);
    assert_eq!(scan.counters.legacy_file_touch_rows_created, 0);
    assert_eq!(scan.counters.legacy_page_owner_json_serializations, 0);
    assert_eq!(
        scan.counters.legacy_page_identity_owner_json_serializations,
        0
    );
    assert_eq!(
        scan.counters.legacy_page_identity_row_json_serializations,
        0
    );
}

#[test]
fn source_backed_projection_batches_ignored_records_in_one_bounded_page() {
    const IGNORED_RECORDS: usize = 256;

    let ignored = jsonl(json!({
        "type": "event_msg",
        "payload": {"type": "token_count", "info": {"total_token_usage": {"input_tokens": 42}}}
    }));
    let mut contents = session_meta("source-backed-batching-owner");
    for _ in 0..IGNORED_RECORDS {
        contents.push_str(&ignored);
    }
    contents.push_str(&message("assistant", "one retained projection"));
    let (_temp, path) = write_source(&contents);
    let (scan, sink) = scan_collect(discover_one(&path, "source-backed-batching-owner"), None);

    assert_eq!(sink.physical_records, vec![(IGNORED_RECORDS + 2) as u64]);
    assert_eq!(scan.counters.emitted_pages, 1);
    assert_eq!(sink.rows.len(), 1);
    assert_eq!(scan.next_raw_ordinal, (IGNORED_RECORDS + 2) as u64);
}

#[test]
fn pending_call_checkpoint_keeps_fresh_and_append_source_backed_outputs_identical() {
    let initial = [
        session_meta("split-owner"),
        tool_call("split-success"),
        tool_call("split-failure"),
        tool_call("split-timeout"),
        tool_call("split-unknown"),
    ]
    .concat();
    let appended = [
        successful_tool_output("split-success", "success"),
        failed_tool_output("split-failure", "failure"),
        timed_out_tool_output("split-timeout", "timeout"),
        tool_output("split-unknown", ""),
    ]
    .concat();
    let complete = format!("{initial}{appended}");
    let (_temp, path) = write_source(&initial);

    let (initial_scan, _) = scan_collect(discover_one(&path, "split-owner"), None);
    let proof = initial_scan
        .bind_checkpoint("canonical-split", CodexCheckpointGeneration::new(90))
        .unwrap()
        .unwrap();
    let checkpoint_wire =
        serde_json::from_slice::<Value>(&proof.checkpoint.encode().unwrap()).unwrap();
    assert_eq!(
        checkpoint_wire["pending_tool_authorities"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    let checkpoint_text = serde_json::to_string(&checkpoint_wire).unwrap();
    assert!(!checkpoint_text.contains("split-success"));
    assert!(!checkpoint_text.contains("printf retained"));

    fs::write(&path, &complete).unwrap();
    let (append_scan, append) = scan_collect(discover_one(&path, "split-owner"), Some(&proof));
    let (fresh_scan, fresh) = scan_collect(discover_one(&path, "split-owner"), None);
    let fresh_output_rows = fresh
        .rows
        .iter()
        .filter(|row| {
            matches!(
                row.event_type,
                EventType::CommandOutput | EventType::ToolOutput
            )
        })
        .cloned()
        .collect::<Vec<_>>();

    assert_eq!(append.rows, fresh_output_rows);
    assert_eq!(
        append
            .rows
            .iter()
            .map(|row| row.raw_ordinal)
            .collect::<Vec<_>>(),
        vec![5, 6, 7, 8]
    );
    assert_eq!(
        append_scan.counters.rejected_complete_records,
        fresh_scan.counters.rejected_complete_records
    );
    assert_eq!(append_scan.counters.bytes_read, complete.len() as u64);
    assert_eq!(
        append_scan.counters.checkpoint_validation_bytes,
        initial.len() as u64
    );
    assert_eq!(
        append_scan.complete_prefix_sha256,
        fresh_scan.complete_prefix_sha256
    );
    assert_eq!(append_scan.next_raw_ordinal, fresh_scan.next_raw_ordinal);
}
