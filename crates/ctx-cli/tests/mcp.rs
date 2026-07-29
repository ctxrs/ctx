mod support;

use support::*;

#[test]
fn mcp_status_and_tools_list_are_read_only_without_initialized_epoch() {
    let temp = tempdir();
    let responses = mcp_roundtrip(
        &temp,
        &[
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "ctx-test", "version": "0" }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/list"
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "status",
                    "arguments": {}
                }
            }),
        ],
    );

    assert_eq!(responses.len(), 3);
    assert_eq!(responses[0]["result"]["serverInfo"]["name"], "ctx");
    assert_eq!(
        responses[0]["result"]["capabilities"]["tools"]["listChanged"],
        false
    );

    let tools = responses[1]["result"]["tools"].as_array().unwrap();
    for expected in [
        "status",
        "sources",
        "search",
        "sql",
        "show_session",
        "show_event",
    ] {
        assert!(
            tools.iter().any(|tool| tool["name"] == expected),
            "missing MCP tool {expected} in {tools:#?}"
        );
    }
    assert!(
        tools.iter().all(|tool| tool["name"] != "research"),
        "MCP research tool should not be exposed in {tools:#?}"
    );
    let search_tool = tools.iter().find(|tool| tool["name"] == "search").unwrap();
    for name in ["show_session", "show_event"] {
        let show_tool = tools.iter().find(|tool| tool["name"] == name).unwrap();
        assert_eq!(
            show_tool["inputSchema"]["properties"]["content"]["enum"],
            json!(["indexed", "complete"])
        );
        assert_eq!(
            show_tool["inputSchema"]["properties"]["content"]["default"],
            "indexed"
        );
    }
    let providers = search_tool["inputSchema"]["properties"]["provider"]["enum"]
        .as_array()
        .unwrap();
    assert!(providers.iter().any(|provider| provider == "copilot-cli"));
    assert!(providers.iter().any(|provider| provider == "copilot_cli"));
    assert!(providers.iter().any(|provider| provider == "qwen-code"));
    assert!(providers.iter().any(|provider| provider == "qwen_code"));
    assert!(providers.iter().any(|provider| provider == "kimi-code-cli"));
    assert!(providers.iter().any(|provider| provider == "kimi_code_cli"));
    assert!(providers.iter().any(|provider| provider == "kiro-cli"));
    assert!(providers.iter().any(|provider| provider == "kiro_cli"));
    assert!(providers.iter().any(|provider| provider == "mimocode"));
    assert!(providers.iter().any(|provider| provider == "lingma"));
    assert!(providers.iter().any(|provider| provider == "codebuddy"));
    assert!(providers.iter().any(|provider| provider == "auggie"));
    assert!(providers.iter().any(|provider| provider == "zed"));
    assert!(providers.iter().any(|provider| provider == "forgecode"));
    assert!(providers.iter().any(|provider| provider == "deepagents"));
    assert!(providers.iter().any(|provider| provider == "mistral-vibe"));
    assert!(providers.iter().any(|provider| provider == "mistral_vibe"));
    assert!(providers.iter().any(|provider| provider == "mux"));
    assert!(providers.iter().any(|provider| provider == "rovodev"));
    assert!(providers.iter().any(|provider| provider == "cline"));
    assert!(providers.iter().any(|provider| provider == "roo"));
    assert!(providers.iter().any(|provider| provider == "roo_code"));
    let backend_values = search_tool["inputSchema"]["properties"]["backend"]["enum"]
        .as_array()
        .unwrap();
    for expected in ["hybrid", "semantic", "lexical"] {
        assert!(backend_values.iter().any(|value| value == expected));
    }
    assert!(search_tool["inputSchema"]["properties"]["backend"]["default"].is_null());
    assert_eq!(
        search_tool["inputSchema"]["properties"]["semantic_weight"]["default"],
        0.35
    );
    let status = &responses[2]["result"]["structuredContent"];
    assert_eq!(status["schema_version"], 2);
    assert_eq!(status["initialized"], false);
    assert!(status["indexed_sessions"].is_null());
    assert!(status["indexed_events"].is_null());
    assert_eq!(status["history_epoch"]["status"], "unavailable");
    assert_eq!(status["lexical"]["status"], "unavailable");
    assert_eq!(
        status["lexical"]["path"],
        json!(temp.path().join("search/lexical"))
    );
    assert_eq!(status["refresh"]["status"], "unavailable");
    assert_eq!(status["relational"]["status"], "unavailable");
    assert_eq!(status["prior_epoch"]["status"], "absent");
    assert_eq!(status["prior_epoch"]["authority"], "non_authoritative");
    assert_eq!(status["prior_epoch"]["opened"], false);
    assert_eq!(status["read_only"], true);
    assert_eq!(status["semantic"]["status"], "disabled");
    assert_eq!(
        status["semantic"]["flat_f32"]["path"],
        json!(temp.path().join("search/semantic"))
    );
    assert_eq!(status["daemon"]["enabled"], true);
    assert_useful_mcp_text(
        &responses[2]["result"],
        &[
            "ctx status",
            "initialized: false",
            "history_epoch: status=unavailable, reason=epoch_not_initialized",
            "lexical: status=unavailable, reason=epoch_not_initialized",
            "source_refresh: status=unavailable, reason=daemon_unavailable",
            "relational: status=unavailable, reason=lexical_generation_unavailable",
            "prior_epoch: status=absent",
            "read_only: true",
            "local_only: true",
            "semantic: status=disabled",
            "flat_f32: status=unavailable, reason=lexical_generation_unavailable",
            "semantic_path:",
            "daemon: enabled=true",
            "mode=full",
            "daemon_lock:",
            "daemon_endpoint:",
            "daemon_jobs: source_backed_refresh=",
        ],
    );
    assert!(
        std::fs::read_dir(temp.path()).unwrap().next().is_none(),
        "MCP status should not create any file in a pristine data root"
    );
}

#[test]
fn mcp_initialize_negotiates_client_supported_protocol_version() {
    let temp = tempdir();
    let responses = mcp_roundtrip(
        &temp,
        &[json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "gemini-cli", "version": "0.49.0" }
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0]["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(responses[0]["result"]["serverInfo"]["name"], "ctx");
}

#[test]
fn mcp_stdio_preserves_json_rpc_ids_and_notification_semantics() {
    let temp = tempdir();
    let stdin = concat!(
        "{not-json\n",
        "[]\n",
        "{\"jsonrpc\":\"2.0\"}\n",
        "{\"jsonrpc\":\"1.0\",\"id\":\"string-id\",\"method\":\"ping\"}\n",
        "{\"jsonrpc\":\"1.0\",\"id\":42,\"method\":\"ping\"}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":null,\"method\":\"ping\"}\n",
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/unknown\"}\n",
    );
    let output = ctx(&temp)
        .args(["mcp", "serve"])
        .write_stdin(stdin)
        .assert()
        .success()
        .get_output()
        .clone();

    assert!(
        output.stderr.is_empty(),
        "MCP stderr must stay clean: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let responses = stdout
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line)
                .unwrap_or_else(|error| panic!("MCP stdout was not JSON-RPC: {error}: {line:?}"))
        })
        .collect::<Vec<_>>();

    assert_eq!(
        responses.len(),
        6,
        "notifications must not emit responses: {stdout}"
    );
    assert!(responses
        .iter()
        .all(|response| response["jsonrpc"] == "2.0"));

    for response in [&responses[0], &responses[1], &responses[2], &responses[5]] {
        assert!(
            response.as_object().unwrap().contains_key("id"),
            "JSON-RPC error response omitted id: {response}"
        );
        assert!(response["id"].is_null(), "{response}");
    }
    assert_eq!(responses[0]["error"]["code"], -32700);
    assert_eq!(responses[1]["error"]["code"], -32600);
    assert_eq!(responses[2]["error"]["code"], -32600);
    assert_eq!(responses[3]["id"], "string-id");
    assert_eq!(responses[3]["error"]["code"], -32600);
    assert_eq!(responses[4]["id"], 42);
    assert_eq!(responses[4]["error"]["code"], -32600);
    assert_eq!(responses[5]["error"]["code"], -32600);
}

#[test]
fn mcp_rejects_oversized_input_line_and_continues() {
    let temp = tempdir();
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "ctx-test", "version": "0" }
        }
    });
    let mut stdin = "x".repeat(1024 * 1024 + 1);
    stdin.push('\n');
    stdin.push_str(&serde_json::to_string(&initialize).unwrap());
    stdin.push('\n');

    let responses = mcp_raw_roundtrip(&temp, stdin);
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["error"]["code"], -32700);
    assert!(responses[0].as_object().unwrap().contains_key("id"));
    assert!(responses[0]["id"].is_null());
    assert!(
        responses[0]["error"]["data"]["error"]
            .as_str()
            .unwrap()
            .contains("exceeds max line bytes"),
        "{:#}",
        responses[0]
    );
    assert_eq!(responses[1]["result"]["serverInfo"]["name"], "ctx");
}

#[test]
fn mcp_rejects_invalid_utf8_input_line_and_continues() {
    let temp = tempdir();
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "ctx-test", "version": "0" }
        }
    });
    let mut stdin = vec![0xff, b'\n'];
    stdin.extend_from_slice(serde_json::to_string(&initialize).unwrap().as_bytes());
    stdin.push(b'\n');

    let responses = mcp_raw_roundtrip_bytes(&temp, stdin);
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["error"]["code"], -32700);
    assert!(responses[0].as_object().unwrap().contains_key("id"));
    assert!(responses[0]["id"].is_null());
    assert_eq!(
        responses[0]["error"]["data"]["error"],
        "MCP message is not valid UTF-8"
    );
    assert_eq!(responses[1]["result"]["serverInfo"]["name"], "ctx");
}

#[test]
fn mcp_sql_tool_returns_structured_json_and_rejects_writes() {
    let temp = tempdir();
    let generation_id = initialize_generation_only_sql_projection(temp.path());
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "relational projection setup must not create prior-epoch storage"
    );

    let responses = mcp_roundtrip(
        &temp,
        &[
            json!({
                "jsonrpc": "2.0",
                "id": "init",
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "ctx-test", "version": "0" }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "sql",
                "method": "tools/call",
                "params": {
                    "name": "sql",
                    "arguments": {
                        "sql": "SELECT core_generation_id, status, (SELECT COUNT(*) FROM ctx_sessions) AS sessions FROM ctx_projection_metadata",
                        "max_rows": 5
                    }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "unsupported",
                "method": "tools/call",
                "params": {
                    "name": "sql",
                    "arguments": {
                        "sql": "SELECT * FROM events"
                    }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "write",
                "method": "tools/call",
                "params": {
                    "name": "sql",
                    "arguments": {
                        "sql": "CREATE TABLE nope(x INTEGER)"
                    }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "budget",
                "method": "tools/call",
                "params": {
                    "name": "sql",
                    "arguments": {
                        "sql": format!(
                            "SELECT {}",
                            (0..256).map(|index| format!("1 AS c{index}")).collect::<Vec<_>>().join(", ")
                        ),
                        "max_rows": 10000,
                        "max_columns": 256,
                        "max_value_bytes": 32
                    }
                }
            }),
        ],
    );

    let sql = &responses[1]["result"]["structuredContent"];
    assert_eq!(sql["payload_type"], "sql_result");
    assert_eq!(sql["read_only"], true);
    assert_eq!(sql["share_safe"], false);
    assert_eq!(
        sql["columns"],
        json!(["core_generation_id", "status", "sessions"])
    );
    assert_eq!(sql["rows"], json!([[generation_id, "ready", 0]]));
    assert_useful_mcp_text(
        &responses[1]["result"],
        &[
            "ctx sql",
            "returned_rows: 1",
            "truncated: rows=false, values=false",
            "core_generation_id",
            "ready",
        ],
    );

    let unsupported = &responses[2]["result"];
    assert_eq!(unsupported["isError"], true);
    assert!(unsupported["structuredContent"]["error"]
        .as_str()
        .unwrap()
        .contains("no such table: events"));
    assert!(mcp_content_text(unsupported).contains("no such table: events"));

    let write = &responses[3]["result"];
    assert_eq!(write["isError"], true);
    assert!(write["structuredContent"]["error"]
        .as_str()
        .unwrap()
        .contains("SQL query must be read-only"));
    assert!(mcp_content_text(write).contains("SQL query must be read-only"));

    let budget = &responses[4]["result"];
    assert_eq!(budget["isError"], true);
    assert!(budget["structuredContent"]["error"]
        .as_str()
        .unwrap()
        .contains("SQL result preview budget"));
    assert!(mcp_content_text(budget).contains("SQL result preview budget"));
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "MCP SQL must remain in the fresh relational projection"
    );
}

#[test]
fn mcp_sql_fresh_root_initializes_only_relational_projection() {
    let temp = tempdir();
    let responses = mcp_roundtrip(
        &temp,
        &[
            json!({
                "jsonrpc": "2.0",
                "id": "init",
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "ctx-test", "version": "0" }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "sql",
                "method": "tools/call",
                "params": {
                    "name": "sql",
                    "arguments": {
                        "sql": "SELECT 1 AS one"
                    }
                }
            }),
        ],
    );

    assert_eq!(
        responses[1]["result"]["structuredContent"]["rows"],
        json!([[1]])
    );
    assert!(temp.path().join("relational.sqlite").is_file());
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "fresh-root MCP SQL must not create prior-epoch storage"
    );
}

#[test]
fn mcp_search_returns_structured_json_without_refresh() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");
    let (_daemon, imported) = import_codex_fixture_through_daemon(&temp, &fixture);
    assert!(imported["sources"][0]["published_generation"].is_string());

    let search_responses = mcp_roundtrip(
        &temp,
        &[
            json!({
                "jsonrpc": "2.0",
                "id": "init",
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "ctx-test", "version": "0" }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "search",
                "method": "tools/call",
                "params": {
                    "name": "search",
                    "arguments": {
                        "query": "onboarding",
                        "provider": "codex",
                        "limit": 5,
                        "backend": "hybrid",
                        "semantic_weight": 0.4
                    }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "result-window",
                "method": "tools/call",
                "params": {
                    "name": "search",
                    "arguments": {
                        "query": "search",
                        "provider": "codex",
                        "include_subagents": true,
                        "limit": 1,
                        "backend": "lexical"
                    }
                }
            }),
        ],
    );
    let search = &search_responses[1]["result"]["structuredContent"];
    assert_eq!(search["schema_version"], 1);
    assert_eq!(search["payload_type"], "search_results");
    assert_eq!(search["query"], "onboarding");
    assert_eq!(search["freshness"]["mode"], "off");
    assert_eq!(search["freshness"]["status"], "existing_generation");
    assert_eq!(search["retrieval"]["requested_mode"], "hybrid");
    assert_eq!(search["retrieval"]["effective_mode"], "lexical");
    let semantic_weight = search["retrieval"]["semantic_weight"].as_f64().unwrap();
    assert!((semantic_weight - 0.4).abs() < 1e-6, "{search:#}");
    assert_eq!(
        search["retrieval"]["semantic_fallback_code"],
        "semantic_disabled"
    );
    assert_eq!(search["retrieval"]["semantic_status"], "disabled");
    assert_eq!(
        search["retrieval"]["semantic_fallback"],
        "local semantic retrieval is disabled"
    );
    assert_eq!(
        search["result_window"],
        json!({
            "limit": 5,
            "returned": 1,
            "more_available": false,
        })
    );
    assert!(search.get("pagination").is_none(), "{search:#}");
    assert_useful_mcp_text(
        &search_responses[1]["result"],
        &[
            "ctx search",
            "query: onboarding",
            "freshness: off/existing_generation",
            "retrieval: requested=hybrid, effective=lexical",
            "semantic_weight=0.4",
            "semantic_status=disabled",
            "semantic_fallback: semantic_disabled",
            "semantic_fallback_detail: local semantic retrieval is disabled",
            "filters: provider=codex",
            "results: 1",
            "ctx_session_id:",
            "ctx_event_id:",
            "snippet:",
            "next: ctx show",
        ],
    );
    let first_result = &search["results"][0];
    assert!(first_result["result_type"].is_string());
    assert!(first_result["ctx_session_id"].is_string());
    assert!(first_result["ctx_event_id"].is_string());
    assert!(!mcp_content_text(&search_responses[1]["result"]).contains("More results available."));

    let result_window = &search_responses[2]["result"]["structuredContent"];
    assert_eq!(result_window["results"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        result_window["result_window"],
        json!({
            "limit": 1,
            "returned": 1,
            "more_available": true,
        })
    );
    assert!(
        result_window.get("pagination").is_none(),
        "{result_window:#}"
    );
    assert!(result_window["truncation"]["candidate_pool"].is_number());
    assert!(mcp_content_text(&search_responses[2]["result"]).ends_with("More results available.\n"));
}

#[test]
fn mcp_search_applies_source_backed_identity_filters() {
    let temp = tempdir();
    let (_daemon, _) = import_custom_history_fixture_source_backed(&temp, "basic.jsonl");
    let responses = mcp_roundtrip(
        &temp,
        &[
            json!({
                "jsonrpc": "2.0",
                "id": "init",
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "ctx-test", "version": "0" }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "history-source",
                "method": "tools/call",
                "params": {
                    "name": "search",
                    "arguments": {
                        "query": "parser test",
                        "history_source": "demo-agent/demo-source"
                    }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "identity-parts",
                "method": "tools/call",
                "params": {
                    "name": "search",
                    "arguments": {
                        "query": "parser test",
                        "provider_key": "demo-agent",
                        "source_id": "demo-source"
                    }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "conjunctive",
                "method": "tools/call",
                "params": {
                    "name": "search",
                    "arguments": {
                        "query": "parser test",
                        "history_source": "demo-agent/demo-source",
                        "provider_key": "demo-agent",
                        "source_id": "demo-source"
                    }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "unknown",
                "method": "tools/call",
                "params": {
                    "name": "search",
                    "arguments": {
                        "query": "parser test",
                        "source_id": "unknown"
                    }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "conflicting",
                "method": "tools/call",
                "params": {
                    "name": "search",
                    "arguments": {
                        "query": "parser test",
                        "history_source": "demo-agent/demo-source",
                        "source_id": "other-source"
                    }
                }
            }),
        ],
    );

    for response in &responses[1..4] {
        let search = &response["result"]["structuredContent"];
        assert_eq!(search["retrieval"]["index"], "source_backed", "{search:#}");
        assert_eq!(search["filters"]["provider"], "custom", "{search:#}");
        assert_eq!(search["results"].as_array().map(Vec::len), Some(1));
    }
    assert_eq!(
        responses[1]["result"]["structuredContent"]["filters"]["history_source"],
        "demo-agent/demo-source"
    );
    assert_eq!(
        responses[2]["result"]["structuredContent"]["filters"]["provider_key"],
        "demo-agent"
    );
    assert_eq!(
        responses[2]["result"]["structuredContent"]["filters"]["source_id"],
        "demo-source"
    );
    for response in &responses[4..] {
        let search = &response["result"]["structuredContent"];
        assert_eq!(search["retrieval"]["index"], "source_backed", "{search:#}");
        assert_eq!(search["results"].as_array().map(Vec::len), Some(0));
    }
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "MCP source identity search must not create previous-epoch storage"
    );
}

#[test]
fn mcp_search_validates_inputs_and_reports_uninitialized_source_index() {
    let temp = tempdir();
    let responses = mcp_roundtrip(
        &temp,
        &[
            json!({
                "jsonrpc": "2.0",
                "id": "init",
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "ctx-test", "version": "0" }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "search",
                "method": "tools/call",
                "params": {
                    "name": "search",
                    "arguments": {
                        "provider": "codex",
                        "limit": 5
                    }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "search-hidden-provider",
                "method": "tools/call",
                "params": {
                    "name": "search",
                    "arguments": {
                        "query": "hidden provider probe",
                        "provider": "not-a-real-provider",
                        "limit": 5
                    }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "search-provider-alias",
                "method": "tools/call",
                "params": {
                    "name": "search",
                    "arguments": {
                        "query": "provider alias probe",
                        "provider": "roo_code",
                        "limit": 5
                    }
                }
            }),
        ],
    );

    let result = &responses[1]["result"];
    assert_eq!(result["isError"], true);
    assert_eq!(result["structuredContent"]["error_code"], "invalid_request");
    assert!(result["structuredContent"]["error"]
        .as_str()
        .unwrap()
        .contains("search needs a query or file"));
    assert!(mcp_content_text(result).contains("search needs a query or file"));
    let hidden_provider = &responses[2]["result"];
    assert_eq!(hidden_provider["isError"], true);
    assert_eq!(
        hidden_provider["structuredContent"]["error_code"],
        "invalid_request"
    );
    assert!(hidden_provider["structuredContent"]["error"]
        .as_str()
        .unwrap()
        .contains("provider must be one of"));
    assert!(mcp_content_text(hidden_provider).contains("provider must be one of"));
    let alias_result = &responses[3]["result"];
    assert_eq!(alias_result["isError"], true);
    assert!(alias_result["structuredContent"]["error_code"].is_null());
    assert!(alias_result["structuredContent"]["error"]
        .as_str()
        .unwrap()
        .contains("the source-backed index does not exist; retry with daemon refresh enabled"));
    assert!(mcp_content_text(alias_result)
        .contains("the source-backed index does not exist; retry with daemon refresh enabled"));
    assert!(
        !temp.path().join("search/lexical").exists(),
        "MCP search must not create an uninitialized source-backed index"
    );
    assert!(!temp.path().join("relational.sqlite").exists());
}

#[test]
fn mcp_invalid_search_session_is_typed_before_source_index_open() {
    let temp = tempdir();
    let responses = mcp_roundtrip(
        &temp,
        &[
            json!({
                "jsonrpc": "2.0",
                "id": "init",
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "ctx-test", "version": "0" }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "invalid-session",
                "method": "tools/call",
                "params": {
                    "name": "search",
                    "arguments": {
                        "query": "session validation",
                        "session": "!!!"
                    }
                }
            }),
        ],
    );

    let result = &responses[1]["result"];
    assert_eq!(result["isError"], true);
    assert_eq!(result["structuredContent"]["error_code"], "invalid_request");
    assert!(result["structuredContent"]["error"]
        .as_str()
        .unwrap()
        .contains("session id prefix must be at least 8 hex characters"));
    assert!(
        !temp.path().join("search/lexical").exists(),
        "invalid session syntax must fail before opening the source-backed index"
    );
    assert!(!temp.path().join("relational.sqlite").exists());
}

#[test]
fn mcp_sources_matches_cli_discovery_issues() {
    let temp = tempdir();
    let cli = json_output(
        ctx(&temp)
            .env("CLAUDE_CONFIG_DIR", "relative-account")
            .args(["sources", "--format=json"]),
    );
    let responses = mcp_roundtrip_with_env(
        &temp,
        &[
            json!({
                "jsonrpc": "2.0",
                "id": "init",
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "ctx-test", "version": "0" }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "sources",
                "method": "tools/call",
                "params": {
                    "name": "sources",
                    "arguments": {}
                }
            }),
        ],
        &[("CLAUDE_CONFIG_DIR", "relative-account")],
    );
    let mcp = &responses[1]["result"]["structuredContent"];

    assert_eq!(mcp["schema_version"], cli["schema_version"]);
    assert_eq!(mcp["issues"], cli["issues"]);
    assert_eq!(mcp["issues_truncated"], cli["issues_truncated"]);
    assert_eq!(mcp["read_only"], true);
    assert!(mcp["issues"].as_array().unwrap().iter().any(|issue| {
        issue["provider"] == "claude"
            && issue["code"] == "selector_unreconstructible"
            && issue["message_truncated"] == false
    }));
}

#[test]
fn mcp_sources_reports_plugins_without_exposing_a_legacy_ingestion_path() {
    let temp = tempdir();
    let plugin =
        write_history_source_plugin_with_refresh(&temp, "hermes", true, Some("auto"), None);

    let responses = mcp_roundtrip_with_env(
        &temp,
        &[
            json!({
                "jsonrpc": "2.0",
                "id": "init",
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "ctx-test", "version": "0" }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "sources",
                "method": "tools/call",
                "params": {
                    "name": "sources",
                    "arguments": {}
                }
            }),
        ],
        &[(
            "CTX_HISTORY_PLUGIN_PATH",
            plugin.manifest_dir.to_str().unwrap(),
        )],
    );

    let sources = responses[1]["result"]["structuredContent"]["sources"]
        .as_array()
        .unwrap();
    let source = sources
        .iter()
        .find(|source| source["history_source"] == "hermes/default")
        .unwrap();
    assert_eq!(source["status"], "unsupported");
    assert_eq!(source["importable"], false);
    assert!(!plugin.run_marker.exists());
    assert_useful_mcp_text(
        &responses[1]["result"],
        &[
            "ctx sources",
            "sources:",
            "importable:",
            "more sources omitted from text",
        ],
    );
}

#[test]
fn mcp_search_excludes_active_codex_session_by_default_when_available() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");
    let (_daemon, _) = import_codex_fixture_through_daemon(&temp, &fixture);

    let excluded = mcp_roundtrip_with_env(
        &temp,
        &[
            json!({
                "jsonrpc": "2.0",
                "id": "init",
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "ctx-test", "version": "0" }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "search",
                "method": "tools/call",
                "params": {
                    "name": "search",
                    "arguments": {
                        "query": "onboarding",
                        "provider": "codex",
                        "limit": 5
                    }
                }
            }),
        ],
        &[("CODEX_THREAD_ID", "codex-session-root")],
    );
    let excluded_search = &excluded[1]["result"]["structuredContent"];
    assert_eq!(excluded_search["results"].as_array().unwrap().len(), 0);
    assert!(excluded_search["filters"]["include_current_session"].is_null());

    let included = mcp_roundtrip_with_env(
        &temp,
        &[
            json!({
                "jsonrpc": "2.0",
                "id": "init",
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "ctx-test", "version": "0" }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "search",
                "method": "tools/call",
                "params": {
                    "name": "search",
                    "arguments": {
                        "query": "onboarding",
                        "provider": "codex",
                        "limit": 5,
                        "include_current_session": true
                    }
                }
            }),
        ],
        &[("CODEX_THREAD_ID", "codex-session-root")],
    );
    let included_search = &included[1]["result"]["structuredContent"];
    let included_results = included_search["results"].as_array().unwrap();
    assert_eq!(included_results.len(), 1, "{included_search:#}");
    assert_eq!(
        included_results[0]["provider"], "codex",
        "{included_search:#}"
    );
    assert_eq!(
        included_results[0]["provider_session_id"], "codex-session-root",
        "{included_search:#}"
    );
    assert_eq!(
        included_search["filters"]["include_current_session"], true,
        "{included_search:#}"
    );
}

#[test]
fn mcp_tool_input_validation_returns_stable_invalid_request_and_server_recovers() {
    let temp = tempdir();
    let cases = [
        (
            "bad-search-limit",
            "search",
            json!({"query": "onboarding", "limit": "five"}),
            "limit must be a non-negative integer",
        ),
        ("bad-sql", "sql", json!({}), "sql is required"),
        (
            "bad-show",
            "show_event",
            json!({"ctx_event_id": "not-a-uuid"}),
            "invalid ctx_event_id",
        ),
        (
            "bad-since",
            "search",
            json!({"query": "onboarding", "since": "yesterday"}),
            "invalid --since value",
        ),
        (
            "bad-event-type",
            "search",
            json!({"query": "onboarding", "event_type": "not-an-event"}),
            "invalid EventType value",
        ),
        (
            "bad-history-source",
            "search",
            json!({
                "query": "onboarding",
                "provider": "custom",
                "history_source": "missing-separator"
            }),
            "--history-source expects plugin/source or provider_key/source_id",
        ),
        (
            "bad-source-id",
            "search",
            json!({"query": "onboarding", "source_id": " "}),
            "--source-id cannot be empty",
        ),
        (
            "bad-source-provider",
            "search",
            json!({
                "query": "onboarding",
                "provider": "codex",
                "provider_key": "custom-provider"
            }),
            "custom history source filters can only be combined with --provider custom",
        ),
        (
            "bad-pro-target-kind",
            "blame",
            json!({"target": {"kind": "unknown", "oid": "abc123"}}),
            "target.kind must be file, commit, or pull_request",
        ),
        (
            "bad-pro-target-argument",
            "blame",
            json!({
                "target": {"kind": "commit", "oid": "abc123", "unexpected": true}
            }),
            "unknown target argument unexpected",
        ),
        (
            "bad-pro-argument",
            "blame",
            json!({
                "target": {"kind": "commit", "oid": "abc123"},
                "unexpected": true
            }),
            "unknown argument unexpected",
        ),
        (
            "bad-pro-limit",
            "blame",
            json!({"target": {"kind": "commit", "oid": "abc123"}, "limit": 0}),
            "limit must be between 1 and 8",
        ),
        (
            "bad-pro-cursor",
            "blame",
            json!({"target": {"kind": "commit", "oid": "abc123"}, "cursor": ""}),
            "cursor must contain 1 to",
        ),
        (
            "bad-pro-cursor-encoding",
            "blame",
            json!({"target": {"kind": "commit", "oid": "abc123"}, "cursor": "é"}),
            "cursor must contain 1 to",
        ),
        (
            "bad-pro-selector",
            "blame",
            json!({"target": {"kind": "pull_request", "selector": "0", "repository": "ctxrs/ctx"}}),
            "pull request selector must be a positive decimal number",
        ),
        (
            "bad-pro-lines",
            "blame",
            json!({"target": {"kind": "file", "path": "src/lib.rs", "lines": {"start": 4, "end": 2}}}),
            "line range must be positive and inclusive",
        ),
    ];

    let mut requests = vec![json!({
        "jsonrpc": "2.0",
        "id": "init",
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "ctx-test", "version": "0" }
        }
    })];
    requests.extend(cases.iter().map(|(id, name, arguments, _)| {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": arguments
            }
        })
    }));
    requests.push(json!({
        "jsonrpc": "2.0",
        "id": "index-failure",
        "method": "tools/call",
        "params": {
            "name": "search",
            "arguments": {
                "query": "valid input reaches the source-backed index",
                "provider": "roo_code"
            }
        }
    }));
    requests.push(json!({
        "jsonrpc": "2.0",
        "id": "status",
        "method": "tools/call",
        "params": {
            "name": "status",
            "arguments": {}
        }
    }));

    let responses = mcp_roundtrip(&temp, &requests);

    for (offset, (id, _, _, detail)) in cases.iter().enumerate() {
        let result = &responses[offset + 1]["result"];
        assert_eq!(responses[offset + 1]["id"], *id);
        assert_eq!(result["isError"], true);
        assert_eq!(result["structuredContent"]["error_code"], "invalid_request");
        assert!(
            result["structuredContent"]["error"]
                .as_str()
                .unwrap()
                .contains(detail),
            "{id}: {result:#?}"
        );
        assert!(mcp_content_text(result).contains(detail));
    }

    let index_failure = &responses[cases.len() + 1]["result"];
    assert_eq!(index_failure["isError"], true);
    assert!(index_failure["structuredContent"]["error_code"].is_null());
    assert!(index_failure["structuredContent"]["error"]
        .as_str()
        .unwrap()
        .contains("the source-backed index does not exist; retry with daemon refresh enabled"));

    let recovered = &responses[cases.len() + 2]["result"];
    assert!(recovered["isError"].is_null());
    assert_eq!(recovered["structuredContent"]["schema_version"], 2);
    assert_eq!(recovered["structuredContent"]["read_only"], true);
}

#[test]
fn mcp_object_argument_errors_are_typed_but_non_objects_remain_json_rpc_invalid_params() {
    let temp = tempdir();
    let responses = mcp_roundtrip(
        &temp,
        &[
            json!({
                "jsonrpc": "2.0",
                "id": "init",
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "ctx-test", "version": "0" }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "unknown-argument",
                "method": "tools/call",
                "params": {
                    "name": "search",
                    "arguments": {
                        "query": "onboarding",
                        "refresh": "wait"
                    }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "non-object",
                "method": "tools/call",
                "params": {
                    "name": "search",
                    "arguments": []
                }
            }),
        ],
    );

    let tool_error = &responses[1]["result"];
    assert_eq!(tool_error["isError"], true);
    assert_eq!(
        tool_error["structuredContent"]["error_code"],
        "invalid_request"
    );
    assert!(tool_error["structuredContent"]["error"]
        .as_str()
        .unwrap()
        .contains("unknown argument refresh"));

    let error = &responses[2]["error"];
    assert_eq!(error["code"], -32602);
    assert!(error["data"]["error"]
        .as_str()
        .unwrap()
        .contains("params.arguments must be an object"));
}
