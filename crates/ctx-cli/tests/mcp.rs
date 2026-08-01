#[path = "mcp/input_validation.rs"]
mod input_validation;
mod support;

use std::collections::BTreeMap;

use support::*;

fn tree_snapshot(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn visit(root: &Path, directory: &Path, snapshot: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        let mut entries = fs::read_dir(directory)
            .unwrap()
            .collect::<std::io::Result<Vec<_>>>()
            .unwrap();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            let file_type = entry.file_type().unwrap();
            if file_type.is_dir() {
                snapshot.insert(relative, None);
                visit(root, &path, snapshot);
            } else if file_type.is_file() {
                snapshot.insert(relative, Some(fs::read(path).unwrap()));
            } else if file_type.is_symlink() {
                snapshot.insert(
                    relative,
                    Some(
                        fs::read_link(path)
                            .unwrap()
                            .as_os_str()
                            .as_encoded_bytes()
                            .to_vec(),
                    ),
                );
            }
        }
    }

    let mut snapshot = BTreeMap::new();
    if root.is_dir() {
        visit(root, root, &mut snapshot);
    }
    snapshot
}

#[test]
fn mcp_status_exactly_matches_cli_json_for_pristine_unavailable_state() {
    let temp = tempdir();
    let root = data_root(&temp);
    fs::create_dir_all(&root).unwrap();
    let before = tree_snapshot(&root);

    let (cli, result) =
        assert_cli_mcp_status_parity(&temp, &[("CTX_LOCAL_USAGE_ENABLED", "false")]);

    assert_eq!(cli["initialized"], false, "{cli:#}");
    assert_eq!(cli["history_epoch"]["status"], "unavailable", "{cli:#}");
    assert_eq!(cli["lexical"]["status"], "unavailable", "{cli:#}");
    assert!(cli["upgrade"].is_object(), "{cli:#}");
    assert_eq!(cli["upgrade"]["auto"], "apply", "{cli:#}");
    assert_eq!(cli["upgrade"]["install"]["marker"], "absent", "{cli:#}");
    assert_eq!(cli["pro"]["installed"], false, "{cli:#}");
    assert_eq!(
        cli["local_usage"],
        json!({
            "schema_version": 2,
            "enabled": false,
            "state": "disabled",
            "definition_version": 2,
            "retention_days": 400,
            "error": null,
        }),
        "{cli:#}"
    );
    assert_eq!(cli["read_only"], true, "{cli:#}");
    assert_status_facts_stay_machine_only(&result);
    assert_eq!(
        tree_snapshot(&root),
        before,
        "CLI and MCP status must not create or mutate data-root entries"
    );
}

#[test]
fn mcp_status_exactly_matches_cli_json_for_existing_healthy_generation() {
    let temp = tempdir();
    let root = data_root(&temp);
    initialize_generation_only_sql_projection(&root);

    let (cli, result) =
        assert_cli_mcp_status_parity(&temp, &[("CTX_LOCAL_USAGE_ENABLED", "false")]);

    assert_eq!(cli["initialized"], true, "{cli:#}");
    assert_eq!(cli["history_epoch"]["status"], "ready", "{cli:#}");
    assert_eq!(cli["lexical"]["status"], "ready", "{cli:#}");
    assert_eq!(cli["relational"]["status"], "ready", "{cli:#}");
    assert_eq!(cli["read_only"], true, "{cli:#}");
    assert_status_facts_stay_machine_only(&result);
}

#[test]
fn mcp_status_matches_cli_compact_error_for_malformed_usage_store() {
    let temp = tempdir();
    let root = data_root(&temp);
    fs::create_dir_all(&root).unwrap();
    let marker = "PRIVATE_MCP_STATUS_USAGE_MARKER_7f98";
    fs::write(
        root.join("usage.sqlite"),
        format!("not sqlite: /tmp/{marker}/bearer-secret"),
    )
    .unwrap();
    let before = tree_snapshot(&root);

    let (cli, result) = assert_cli_mcp_status_parity(&temp, &[("CTX_LOCAL_USAGE_ENABLED", "true")]);

    assert_eq!(cli["local_usage"]["enabled"], true, "{cli:#}");
    assert_eq!(cli["local_usage"]["state"], "error", "{cli:#}");
    assert_eq!(
        cli["local_usage"]["error"]["code"], "usage_store_unavailable",
        "{cli:#}"
    );
    assert!(cli["local_usage"].get("definitions").is_none(), "{cli:#}");
    let encoded = serde_json::to_string(&result).unwrap();
    assert!(!encoded.contains(marker), "{encoded}");
    assert!(!encoded.contains("bearer-secret"), "{encoded}");
    assert_status_facts_stay_machine_only(&result);
    assert_eq!(
        tree_snapshot(&root),
        before,
        "malformed usage reporting must remain content-free and filesystem read-only"
    );
}

#[test]
fn mcp_startup_health_checks_enabled_daemon_before_status_and_tools_list() {
    let temp = daemon_test_root();
    let data_root = data_root(&temp);
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
        assert!(show_tool["inputSchema"]["properties"]
            .get("content")
            .is_none());
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
    let initialized = status["initialized"].as_bool().expect("initialized flag");
    assert!(
        status["indexed_sessions"].is_null() || status["indexed_sessions"] == 0,
        "{status:#}"
    );
    assert!(
        status["indexed_events"].is_null() || status["indexed_events"] == 0,
        "{status:#}"
    );
    let core_status = status["history_epoch"]["status"]
        .as_str()
        .expect("history epoch status");
    assert!(
        matches!(core_status, "ready" | "pending" | "unavailable"),
        "startup health may observe a queued initial refresh or its terminal empty-source result: {status:#}"
    );
    assert_eq!(initialized, core_status == "ready", "{status:#}");
    assert_eq!(status["lexical"]["status"], core_status, "{status:#}");
    assert_eq!(
        status["lexical"]["path"],
        json!(data_root.join("search/lexical"))
    );
    assert!(
        matches!(
            status["refresh"]["status"].as_str(),
            Some("ready" | "pending" | "unavailable")
        ),
        "{status:#}"
    );
    assert!(
        matches!(
            status["relational"]["status"].as_str(),
            Some("ready" | "pending" | "unavailable")
        ),
        "{status:#}"
    );
    assert!(status.get("prior_epoch").is_none());
    assert_eq!(status["read_only"], true, "{status:#}");
    assert_eq!(status["semantic"]["status"], "disabled");
    assert_eq!(
        status["semantic"]["flat_f32"]["path"],
        json!(data_root.join("search/semantic"))
    );
    assert_eq!(status["daemon"]["enabled"], true);
    assert_eq!(status["daemon"]["running"], true, "{status:#}");
    assert_eq!(status["daemon"]["core_refresh_endpoint"]["available"], true);
    assert_eq!(status["daemon"]["start_mode"], "auto");
    assert_eq!(status["daemon"]["supervisor"]["status"], "fallback");
    let initialized_text = format!("initialized: {initialized}");
    assert_useful_mcp_text(
        &responses[2]["result"],
        &[
            "ctx status",
            &initialized_text,
            "history_epoch: status=",
            "lexical: status=",
            "source_refresh: status=",
            "relational: status=",
            "read_only: true",
            "local_only: true",
            "semantic: status=disabled",
            "flat_f32: status=",
            "semantic_path:",
            "daemon: enabled=true",
            "mode=full",
            "daemon_lock:",
            "daemon_endpoint:",
        ],
    );
    assert!(data_root.join("daemon/daemon.lock").is_file());
    assert!(data_root
        .join("daemon/source-refresh-endpoint.json")
        .is_file());
    assert!(
        !data_root.join("work.sqlite").exists(),
        "MCP startup must not initialize the previous history epoch"
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
    let generation_id = initialize_generation_only_sql_projection(&data_root(&temp));
    assert!(
        !data_root(&temp).join("work.sqlite").exists(),
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
        !data_root(&temp).join("work.sqlite").exists(),
        "MCP SQL must remain in the fresh relational projection"
    );
}

#[test]
fn mcp_sql_fresh_root_reports_missing_projection_without_initializing_storage() {
    let temp = tempdir();
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
        &[("CTX_DAEMON_AUTOSTART_OFF", "1")],
    );

    let error = &responses[1]["result"];
    assert_eq!(error["isError"], true, "{error:#}");
    assert!(
        error["structuredContent"]["error"]
            .as_str()
            .is_some_and(|message| message.contains("Core SQL projection is missing")),
        "{error:#}"
    );
    assert!(
        !data_root(&temp).exists()
            || std::fs::read_dir(data_root(&temp))
                .unwrap()
                .next()
                .is_none(),
        "existing-only MCP SQL may establish the data-root directory but must not create storage"
    );
}

#[test]
fn mcp_search_returns_structured_json_without_refresh() {
    let temp = daemon_test_root();
    let fixture = provider_history_fixture("codex-sessions");
    copy_dir_all(
        std::path::Path::new(&fixture),
        &temp.path().join(".codex/sessions"),
    );
    let mut daemon = start_mcp_source_refresh_daemon(&temp);
    let initial = json_output(ctx(&temp).args([
        "search",
        "onboarding",
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_eq!(initial["schema_version"], 1, "{initial:#}");
    assert_eq!(initial["results"].as_array().map(Vec::len), Some(1));
    let killed_pid = daemon.kill_and_wait();

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
            json!({
                "jsonrpc": "2.0",
                "id": "semantic-only",
                "method": "tools/call",
                "params": {
                    "name": "search",
                    "arguments": {
                        "query": "onboarding",
                        "provider": "codex",
                        "limit": 5,
                        "backend": "semantic"
                    }
                }
            }),
        ],
    );
    let recovered = json_output(ctx(&temp).args(["daemon", "status", "--format=json"]));
    let search = &search_responses[1]["result"]["structuredContent"];
    assert_eq!(search["schema_version"], 1, "{search:#}\n{recovered:#}");
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
            "next: ctx --data-root",
        ],
    );
    let first_result = &search["results"][0];
    assert_eq!(first_result["rank"], 1);
    assert!(first_result["retrieval_score"].is_number());
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
    assert_mcp_typed_error(
        &search_responses[3]["result"],
        "semantic_disabled",
        false,
        "semantic search is disabled",
    );
    assert_eq!(recovered["daemon"]["running"], true, "{recovered:#}");
    assert_eq!(
        recovered["daemon"]["core_refresh_endpoint"]["available"], true,
        "{recovered:#}"
    );
    assert_ne!(
        recovered["daemon"]["pid"].as_u64(),
        Some(u64::from(killed_pid)),
        "{recovered:#}"
    );
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
        assert_eq!(search["retrieval"]["index"], "core", "{search:#}");
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
        assert_eq!(search["retrieval"]["index"], "core", "{search:#}");
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
