#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn bare_history_source_plugin_selector_fails_before_execution() {
    let temp = tempdir();
    let plugin_root = temp.path().join("history-plugins");
    let dorkos = write_history_source_plugin_at(&plugin_root, "dorkos", false, None);
    let hermes = write_history_source_plugin_at(&plugin_root, "hermes", false, None);

    let stderr = failure_stderr(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin_root)
            .args(["import", "--history-source", "dorkos", "--progress", "none"]),
    );

    assert!(
        stderr.contains("no history source plugin matched"),
        "{stderr}"
    );
    assert!(!dorkos.run_marker.exists());
    assert!(!hermes.run_marker.exists());
}

#[test]
pub(crate) fn import_all_runs_enabled_history_source_plugins_for_external_shapes() {
    let temp = tempdir();
    let plugin_root = temp.path().join("history-plugins");
    let providers = ["dorkos", "openclaw", "hermes", "nanoclaw"];
    for provider in providers {
        write_history_source_plugin_at(&plugin_root, provider, true, None);
    }
    write_history_source_plugin_at(&plugin_root, "disabled-dorkos", false, None);

    let imported = json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin_root)
            .args(["import", "--all", "--json", "--progress", "none"]),
    );
    assert_eq!(imported["totals"]["imported_sources"], 4);
    assert_eq!(imported["totals"]["imported_sessions"], 4);
    assert_eq!(imported["totals"]["imported_events"], 4);
    let sources = imported["sources"].as_array().unwrap();
    for provider in providers {
        assert!(
            sources
                .iter()
                .any(|source| source["history_source"] == format!("{provider}/default")),
            "missing import source for {provider}: {sources:#?}"
        );
        let search = json_output(ctx(&temp).args([
            "search",
            &format!("{provider} plugin initial marker"),
            "--provider",
            "custom",
            "--refresh",
            "off",
            "--json",
        ]));
        assert!(
            !search["results"].as_array().unwrap().is_empty(),
            "{provider} plugin result was not searchable: {search:#}"
        );
    }
    assert!(!sources
        .iter()
        .any(|source| source["history_source"] == "disabled-dorkos/default"));
}

#[test]
pub(crate) fn sources_lists_supported_personal_agent_provider_defaults() {
    let temp = tempdir();
    install_default_openclaw_fixture(&temp, "openclaw-sources-oracle");
    install_default_hermes_fixture(&temp, "hermes-sources-oracle");
    install_default_kilo_fixture(&temp, "kilo-sources-oracle");
    install_default_kiro_fixture(&temp, "kiro-sources-oracle");
    install_default_astrbot_fixture(&temp, "astrbot-sources-oracle");
    install_default_shelley_fixture(&temp, "shelley-sources-oracle");
    install_default_continue_fixture(&temp, "continue-sources-oracle");
    install_default_forgecode_fixture(&temp, "forgecode-sources-oracle");
    install_default_mistral_vibe_fixture(&temp, "mistral-vibe-sources-oracle");
    install_default_mux_fixture(&temp, "mux-sources-oracle");
    install_default_lingma_fixture(&temp, "lingma-sources-oracle");
    install_default_qoder_fixture(&temp, "qoder-sources-oracle");
    install_default_auggie_fixture(&temp, "auggie-sources-oracle");
    install_default_junie_fixture(&temp, "junie-sources-oracle");
    install_default_warp_fixture(&temp);
    install_default_trae_fixture(&temp, "trae-sources-oracle");

    let sources = json_output(ctx(&temp).args(["sources", "--json"]));
    for (provider, source_format, import_support, native_import) in [
        ("openclaw", "openclaw_session_jsonl_tree", "native", true),
        ("hermes", "hermes_state_sqlite", "native", true),
        ("kilo", "kilo_sqlite", "native", true),
        ("kiro_cli", "kiro_cli_sqlite", "native", true),
        ("astrbot", "astrbot_data_v4_sqlite", "native", true),
        ("shelley", "shelley_sqlite", "native", true),
        ("continue", "continue_cli_sessions_json", "native", true),
        ("forgecode", "forgecode_sqlite", "native", true),
        (
            "mistral_vibe",
            "mistral_vibe_session_jsonl_tree",
            "native",
            true,
        ),
        ("mux", "mux_session_jsonl_tree", "native", true),
        ("lingma", "lingma_sqlite", "native", true),
        ("qoder", "qoder_transcript_jsonl_tree", "native", true),
        ("auggie", "auggie_session_json", "native", true),
        ("junie", "junie_session_events_jsonl_tree", "native", true),
        ("warp", "warp_sqlite", "native", true),
        ("trae", "trae_state_vscdb", "native", true),
    ] {
        let source = sources["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|source| {
                source["provider"] == provider && source["source_format"] == source_format
            })
            .unwrap_or_else(|| panic!("missing {provider} source in {sources:#}"));
        assert_eq!(source["status"], "available");
        assert_eq!(source["import_support"], import_support);
        assert_eq!(source["native_import"], native_import);
        assert_eq!(source["importable"], true);
        assert!(source["unsupported_reason"].is_null());
    }
}

#[test]
pub(crate) fn mcp_sources_and_search_support_history_source_plugins() {
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

    let search = &responses[2]["result"]["structuredContent"];
    assert_eq!(search["filters"]["provider"], "custom");
    assert_eq!(search["filters"]["history_source"], "hermes/default");
    assert_eq!(search["results"][0]["history_source"], "hermes/default");
}

#[test]
pub(crate) fn search_refresh_off_does_not_execute_history_source_plugins() {
    let temp = tempdir();
    json_output(ctx(&temp).args(["setup", "--json"]));
    let plugin =
        write_history_source_plugin_with_refresh(&temp, "hermes", true, Some("auto"), None);

    let search = json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args([
                "search",
                "hermes plugin initial marker",
                "--provider",
                "custom",
                "--refresh",
                "off",
                "--json",
            ]),
    );

    assert_eq!(search["freshness"]["mode"], "off");
    assert_eq!(search["freshness"]["status"], "skipped");
    assert!(search["results"].as_array().unwrap().is_empty());
    assert!(!plugin.run_marker.exists());
}

#[test]
pub(crate) fn search_refresh_auto_skips_disabled_or_manual_history_source_plugins() {
    let temp = tempdir();
    let plugin_root = temp.path().join("history-plugins");
    let manual = write_history_source_plugin_at_with_refresh(
        &plugin_root,
        "hermes",
        true,
        Some("manual"),
        None,
    );
    let disabled = write_history_source_plugin_at_with_refresh(
        &plugin_root,
        "dorkos",
        false,
        Some("auto"),
        None,
    );

    let search = json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin_root)
            .args([
                "search",
                "plugin initial marker",
                "--provider",
                "custom",
                "--json",
            ]),
    );

    assert_eq!(search["freshness"]["mode"], "auto");
    assert_eq!(search["freshness"]["status"], "no_sources");
    assert_eq!(search["freshness"]["source_count"], 0);
    assert!(search["results"].as_array().unwrap().is_empty());
    assert!(!manual.run_marker.exists());
    assert!(!disabled.run_marker.exists());
}

pub(crate) fn install_default_hermes_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_hermes_fixture(temp, query));
    let target = temp.path().join(".hermes");
    fs::create_dir_all(&target).unwrap();
    fs::copy(source, target.join("state.db")).unwrap();
}

pub(crate) fn write_native_hermes_fixture(temp: &TempDir, query: &str) -> String {
    let path = temp.path().join("native-hermes-state.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table sessions (
            id text primary key,
            source text not null,
            model text,
            model_config text,
            parent_session_id text,
            started_at real not null,
            ended_at real,
            message_count integer default 0,
            tool_call_count integer default 0,
            input_tokens integer default 0,
            output_tokens integer default 0,
            cwd text,
            title text,
            archived integer default 0
        );
        create table messages (
            id integer primary key autoincrement,
            session_id text not null,
            role text not null,
            content text,
            tool_calls text,
            tool_call_id text,
            tool_name text,
            timestamp real not null,
            active integer not null default 1,
            compacted integer not null default 0
        );",
    )
    .unwrap();
    conn.execute(
        "insert into sessions (
            id, source, model, model_config, started_at, message_count, cwd, title
        ) values (?1, 'acp', 'gpt-5-mini', ?2, 1782259200.0, 2, '/workspace', 'native hermes')",
        [
            "hermes-cli-native",
            r#"{"cwd":"/workspace","provider":"openai"}"#,
        ],
    )
    .unwrap();
    conn.execute(
        "insert into messages (session_id, role, content, timestamp) values (?1, 'user', ?2, 1782259201.0)",
        ["hermes-cli-native", query],
    )
    .unwrap();
    conn.execute(
        "insert into messages (session_id, role, content, timestamp) values (?1, 'assistant', 'native import ok', 1782259202.0)",
        ["hermes-cli-native"],
    )
    .unwrap();
    path.to_str().unwrap().to_owned()
}

pub(crate) fn append_native_hermes_event(path: &str, query: &str) {
    let conn = Connection::open(path).unwrap();
    conn.execute(
        "insert into messages (session_id, role, content, timestamp) values (?1, 'user', ?2, 1782259203.0)",
        ["hermes-cli-native", query],
    )
    .unwrap();
}
