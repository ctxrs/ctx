mod support;

use support::*;

#[test]
fn mcp_status_and_tools_list_are_read_only_without_initialized_store() {
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
    let status = &responses[2]["result"]["structuredContent"];
    assert_eq!(status["schema_version"], 1);
    assert_eq!(status["initialized"], false);
    assert_eq!(status["read_only"], true);
    assert_useful_mcp_text(
        &responses[2]["result"],
        &[
            "ctx status",
            "initialized: false",
            "database_path:",
            "indexed_items: 0",
            "read_only: true",
            "local_only: true",
        ],
    );
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "MCP status should not initialize the ctx store"
    );
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
    assert_eq!(
        responses[0]["error"]["data"]["error"],
        "MCP message is not valid UTF-8"
    );
    assert_eq!(responses[1]["result"]["serverInfo"]["name"], "ctx");
}

#[test]
fn mcp_sql_tool_returns_structured_json_and_rejects_writes() {
    let temp = tempdir();
    ctx(&temp)
        .args(["setup", "--catalog-only", "--progress", "none"])
        .assert()
        .success();

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
                        "sql": "SELECT COUNT(*) AS sessions FROM ctx_sessions",
                        "max_rows": 5
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
    assert_eq!(sql["item_type"], "sql_result");
    assert_eq!(sql["read_only"], true);
    assert_eq!(sql["share_safe"], false);
    assert_eq!(sql["columns"], json!(["sessions"]));
    assert_eq!(sql["rows"], json!([[0]]));
    assert_useful_mcp_text(
        &responses[1]["result"],
        &[
            "ctx sql",
            "returned_rows: 1",
            "truncated: rows=false, values=false",
            "| sessions |",
            "| 0 |",
        ],
    );

    let write = &responses[2]["result"];
    assert_eq!(write["isError"], true);
    assert!(write["structuredContent"]["error"]
        .as_str()
        .unwrap()
        .contains("SQL query must be read-only"));
    assert!(mcp_content_text(write).contains("SQL query must be read-only"));

    let budget = &responses[3]["result"];
    assert_eq!(budget["isError"], true);
    assert!(budget["structuredContent"]["error"]
        .as_str()
        .unwrap()
        .contains("SQL result preview budget"));
    assert!(mcp_content_text(budget).contains("SQL result preview budget"));
}

#[test]
fn mcp_show_session_caps_transcript_events() {
    let temp = tempdir();
    ctx(&temp)
        .args(["setup", "--catalog-only", "--progress", "none"])
        .assert()
        .success();

    let session_id = "018f45d0-0000-7000-8000-000000010001";
    let conn = Connection::open(temp.path().join("work.sqlite")).unwrap();
    conn.execute(
        r#"
        INSERT INTO sessions
        (
            id, provider, external_session_id, agent_type, is_primary, status, fidelity,
            started_at_ms, created_at_ms, updated_at_ms
        )
        VALUES (?1, 'codex', 'mcp-large-session', 'primary', 1, 'imported', 'imported', 1, 1, 1)
        "#,
        [session_id],
    )
    .unwrap();
    for index in 0..201 {
        let event_id = format!("018f45d0-0000-7000-8000-{index:012x}");
        conn.execute(
            r#"
            INSERT INTO events
            (id, seq, session_id, event_type, role, occurred_at_ms, payload_json)
            VALUES (?1, ?2, ?3, 'message', 'assistant', ?4, ?5)
            "#,
            params![
                event_id,
                index,
                session_id,
                index + 1,
                format!(r#"{{"text":"mcp transcript event {index}"}}"#)
            ],
        )
        .unwrap();
    }
    drop(conn);

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
                "id": "show",
                "method": "tools/call",
                "params": {
                    "name": "show_session",
                    "arguments": {
                        "ctx_session_id": session_id,
                        "mode": "log"
                    }
                }
            }),
        ],
    );

    let transcript = &responses[1]["result"]["structuredContent"];
    assert_eq!(transcript["truncated"]["events"], true);
    assert_eq!(transcript["truncated"]["max_events"], 200);
    assert_eq!(transcript["events"].as_array().unwrap().len(), 200);
    let text = assert_useful_mcp_text(
        &responses[1]["result"],
        &[
            "ctx show session",
            session_id,
            "events: 200",
            "event list capped at 200 events",
            "mcp transcript event 0",
            "... 192 more events omitted from text",
        ],
    );
    assert!(
        !text.contains("mcp transcript event 199"),
        "session text should summarize rather than dump all events:\n{text}"
    );
}

#[test]
fn mcp_search_and_show_tools_return_structured_json_without_refresh() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");
    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--json",
        "--progress",
        "none",
    ]));
    assert!(imported["totals"]["imported_events"].as_u64().unwrap() > 0);

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
                        "limit": 5
                    }
                }
            }),
        ],
    );
    let search = &search_responses[1]["result"]["structuredContent"];
    assert_eq!(search["schema_version"], 1);
    assert_eq!(search["query"], "onboarding");
    assert_eq!(search["freshness"]["mode"], "off");
    assert_eq!(search["freshness"]["status"], "skipped");
    assert_eq!(search["share_safe"], false);
    assert_useful_mcp_text(
        &search_responses[1]["result"],
        &[
            "ctx search",
            "query: onboarding",
            "freshness: off/skipped",
            "filters: provider=codex",
            "results: 1",
            "ctx_session_id:",
            "ctx_event_id:",
            "snippet:",
            "next: ctx show",
        ],
    );
    let first_result = &search["results"][0];
    let ctx_session_id = first_result["ctx_session_id"].as_str().unwrap();
    let ctx_event_id = first_result["ctx_event_id"].as_str().unwrap();

    let show_responses = mcp_roundtrip(
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
                "id": "session",
                "method": "tools/call",
                "params": {
                    "name": "show_session",
                    "arguments": {
                        "ctx_session_id": ctx_session_id,
                        "mode": "lite"
                    }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "event",
                "method": "tools/call",
                "params": {
                    "name": "show_event",
                    "arguments": {
                        "ctx_event_id": ctx_event_id,
                        "window": 1
                    }
                }
            }),
        ],
    );

    let session = &show_responses[1]["result"]["structuredContent"];
    assert_eq!(session["item_type"], "session_transcript");
    assert_eq!(session["ctx_session_id"], ctx_session_id);
    assert_eq!(session["mode"], "lite");
    assert!(session["events"].as_array().unwrap().iter().all(|event| {
        event["ctx_session_id"] == ctx_session_id && event["ctx_event_id"].is_string()
    }));
    assert_useful_mcp_text(
        &show_responses[1]["result"],
        &[
            "ctx show session",
            ctx_session_id,
            "provider: codex",
            "mode: lite",
            "events:",
            "ctx_event_id:",
            "text:",
        ],
    );

    let event = &show_responses[2]["result"]["structuredContent"];
    assert_eq!(event["item_type"], "event_window");
    assert_eq!(event["ctx_event_id"], ctx_event_id);
    assert_eq!(event["ctx_session_id"], ctx_session_id);
    assert!(!event["events"].as_array().unwrap().is_empty());
    assert_useful_mcp_text(
        &show_responses[2]["result"],
        &[
            "ctx show event",
            ctx_event_id,
            ctx_session_id,
            "selected event",
            "window",
            "ctx_event_id:",
            "text:",
        ],
    );
}

#[test]
fn mcp_search_requires_query_term_or_file_without_opening_store() {
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
    assert!(result["structuredContent"]["error"]
        .as_str()
        .unwrap()
        .contains("search needs a query or file"));
    assert!(mcp_content_text(result).contains("search needs a query or file"));
    let hidden_provider = &responses[2]["result"];
    assert_eq!(hidden_provider["isError"], true);
    assert!(hidden_provider["structuredContent"]["error"]
        .as_str()
        .unwrap()
        .contains("provider must be one of"));
    assert!(mcp_content_text(hidden_provider).contains("provider must be one of"));
    let alias_result = &responses[3]["result"];
    assert_eq!(alias_result["isError"], true);
    assert!(alias_result["structuredContent"]["error"]
        .as_str()
        .unwrap()
        .contains("ctx store is not initialized"));
    assert!(mcp_content_text(alias_result).contains("ctx store is not initialized"));
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "invalid MCP search should fail before opening the ctx store"
    );
}

#[test]
fn mcp_sources_and_search_support_history_source_plugins() {
    let temp = tempdir();
    let plugin = write_history_source_plugin(&temp, "hermes", false, None);
    json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args([
                "import",
                "--history-source",
                "hermes/default",
                "--json",
                "--progress",
                "none",
            ]),
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
            json!({
                "jsonrpc": "2.0",
                "id": "search",
                "method": "tools/call",
                "params": {
                    "name": "search",
                    "arguments": {
                        "query": "hermes plugin initial marker",
                        "provider": "custom",
                        "history_source": "hermes/default",
                        "limit": 5
                    }
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
    assert!(sources
        .iter()
        .any(|source| source["history_source"] == "hermes/default"));
    assert_useful_mcp_text(
        &responses[1]["result"],
        &[
            "ctx sources",
            "sources:",
            "available:",
            "importable:",
            "custom",
            "available",
            "hermes/default",
        ],
    );

    let search = &responses[2]["result"]["structuredContent"];
    assert_eq!(search["filters"]["provider"], "custom");
    assert_eq!(search["filters"]["history_source"], "hermes/default");
    assert_eq!(search["results"][0]["history_source"], "hermes/default");
    assert_useful_mcp_text(
        &responses[2]["result"],
        &[
            "ctx search",
            "query: hermes plugin initial marker",
            "filters: provider=custom, history_source=hermes/default",
            "hermes/default",
            "snippet:",
        ],
    );
}

#[test]
fn mcp_search_excludes_active_codex_session_by_default_when_available() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");
    json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--json",
        "--progress",
        "none",
    ]));

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
    assert_eq!(
        excluded_search["filters"]["exclude_provider_session"]["provider"],
        "codex"
    );

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
    assert_eq!(included_search["results"].as_array().unwrap().len(), 1);
    assert!(included_search["filters"]["exclude_provider_session"].is_null());
}

#[test]
fn mcp_rejects_unknown_tool_arguments() {
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
                        "query": "onboarding",
                        "refresh": "strict"
                    }
                }
            }),
        ],
    );

    let error = &responses[1]["error"];
    assert_eq!(error["code"], -32602);
    assert!(error["data"]["error"]
        .as_str()
        .unwrap()
        .contains("unknown argument refresh"));
}
