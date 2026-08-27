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
    assert!(cli.get("relational").is_none(), "{cli:#}");
    assert!(cli["upgrade"].is_object(), "{cli:#}");
    assert_eq!(cli["upgrade"]["auto"], "off", "{cli:#}");
    assert_eq!(cli["upgrade"]["install"]["marker"], "absent", "{cli:#}");
    assert!(cli.get("pro").is_none(), "{cli:#}");
    assert_eq!(
        cli["local_usage"],
        json!({
            "schema_version": 3,
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
    initialize_authoritative_empty_core(&root);

    let (cli, result) =
        assert_cli_mcp_status_parity(&temp, &[("CTX_LOCAL_USAGE_ENABLED", "false")]);

    assert_eq!(cli["initialized"], true, "{cli:#}");
    assert_eq!(cli["history_epoch"]["status"], "ready", "{cli:#}");
    assert_eq!(cli["lexical"]["status"], "ready", "{cli:#}");
    assert!(cli.get("relational").is_none(), "{cli:#}");
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
    for expected in ["status", "sources", "search", "show_session", "show_event"] {
        assert!(
            tools.iter().any(|tool| tool["name"] == expected),
            "missing MCP tool {expected} in {tools:#?}"
        );
    }
    assert!(
        tools
            .iter()
            .all(|tool| !matches!(tool["name"].as_str(), Some("research" | "sql"))),
        "removed MCP tools should not be exposed in {tools:#?}"
    );
    let search_tool = tools.iter().find(|tool| tool["name"] == "search").unwrap();
    for name in ["show_session", "show_event"] {
        let show_tool = tools.iter().find(|tool| tool["name"] == name).unwrap();
        assert!(show_tool["inputSchema"]["properties"]
            .get("content")
            .is_none());
    }
    let show_session_tool = tools
        .iter()
        .find(|tool| tool["name"] == "show_session")
        .unwrap();
    let show_session_schema = &show_session_tool["inputSchema"];
    assert_eq!(show_session_schema["required"], json!(["ctx_session_id"]));
    assert_eq!(show_session_schema["additionalProperties"], false);
    assert_eq!(
        show_session_schema["properties"]["mode"]["enum"],
        json!(["full", "lite", "log"])
    );
    assert_eq!(show_session_schema["properties"]["mode"]["default"], "lite");
    assert_eq!(
        show_session_schema["properties"]["limit"],
        json!({
            "type": "integer",
            "minimum": 1,
            "maximum": 4096,
            "default": 200,
            "description": "Maximum selected transcript events to return after applying mode."
        })
    );
    assert_eq!(
        show_session_schema["properties"]["cursor"],
        json!({
            "type": "string",
            "minLength": 1,
            "maxLength": 4096,
            "description": "Opaque next_cursor from the preceding page of this exact session and Core generation."
        })
    );
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
    assert!(search_tool["inputSchema"]["properties"]
        .get("include_subagents")
        .is_none());
    assert_eq!(
        search_tool["inputSchema"]["properties"]["primary_only"],
        json!({
            "type": "boolean",
            "default": false,
            "description": "Search only primary agent sessions."
        })
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
    assert!(status.get("relational").is_none(), "{status:#}");
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
    assert!(!data_root.join("relational.sqlite").exists());
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
fn mcp_search_matches_cli_results_without_refresh() {
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
    assert_eq!(initial["schema_version"], 2, "{initial:#}");
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
    assert_eq!(search["schema_version"], 2, "{search:#}\n{recovered:#}");
    assert_eq!(
        search["results"], initial["results"],
        "{search:#}\n{initial:#}"
    );
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
fn mcp_sources_matches_cli_catalog_and_discovery_issues() {
    let temp = tempdir();
    let cli = json_output(
        ctx(&temp)
            .env("CLAUDE_CONFIG_DIR", "relative-account")
            .args(["sources", "--all", "--format=json"]),
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
    assert_eq!(mcp["automatic_discovery"], cli["automatic_discovery"]);
    assert_eq!(mcp["sources"], cli["sources"]);
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
fn mcp_search_does_not_infer_active_session_from_cli_environment() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");
    let (_daemon, _) = import_codex_fixture_through_daemon(&temp, &fixture);

    let default_search = mcp_roundtrip_with_env(
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
    let default_search = &default_search[1]["result"]["structuredContent"];
    let default_results = default_search["results"].as_array().unwrap();
    assert_eq!(default_results.len(), 1, "{default_search:#}");
    assert_eq!(
        default_results[0]["provider_session_id"], "codex-session-root",
        "{default_search:#}"
    );
    assert!(default_search["filters"]["include_current_session"].is_null());

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
