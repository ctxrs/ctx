mod support;

use support::*;

#[test]
fn qwen_kimi_mistral_mux_and_qoder_default_sources_import_search_and_reimport() {
    let temp = tempdir();
    copy_dir_all(
        Path::new(&provider_history_fixture("qwen-code/.qwen")),
        &temp.path().join(".qwen"),
    );
    copy_dir_all(
        Path::new(&provider_history_fixture("kimi-code-cli/.kimi-code")),
        &temp.path().join(".kimi-code"),
    );
    copy_dir_all(
        Path::new(&provider_history_fixture("mistral-vibe/v1/logs/session")),
        &temp.path().join(".vibe").join("logs").join("session"),
    );
    copy_dir_all(
        Path::new(&provider_history_fixture("mux/v0.27.0/sessions")),
        &temp.path().join(".mux").join("sessions"),
    );
    copy_dir_all(
        Path::new(&provider_history_fixture("qoder/projects")),
        &temp.path().join(".qoder").join("projects"),
    );

    let sources = json_output(ctx(&temp).args(["sources", "--json"]));
    for (provider, source_format) in [
        ("qwen_code", "qwen_code_chat_jsonl_tree"),
        ("kimi_code_cli", "kimi_code_cli_wire_jsonl_tree"),
        ("mistral_vibe", "mistral_vibe_session_jsonl_tree"),
        ("mux", "mux_session_jsonl_tree"),
        ("qoder", "qoder_transcript_jsonl_tree"),
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
        assert_eq!(source["import_support"], "native");
        assert_eq!(source["native_import"], true);
        assert_eq!(source["importable"], true);
    }

    for (cli_provider, stored_provider, query, minimum_events) in [
        ("qwen-code", "qwen_code", "qwen jsonl oracle prompt", 3),
        (
            "kimi-code-cli",
            "kimi_code_cli",
            "kimi jsonl oracle prompt",
            7,
        ),
        (
            "mistral-vibe",
            "mistral_vibe",
            "mistral vibe oracle prompt",
            4,
        ),
        ("mux", "mux", "mux jsonl oracle prompt", 6),
        ("qoder", "qoder", "qoder jsonl oracle prompt", 7),
    ] {
        let first = json_output(ctx(&temp).args([
            "import",
            "--provider",
            cli_provider,
            "--json",
            "--progress",
            "none",
        ]));
        assert_eq!(first["totals"]["failed"], 0);
        assert_eq!(first["totals"]["imported_sources"], 1);
        assert!(
            first["totals"]["imported_events"].as_u64().unwrap() >= minimum_events,
            "{first:#}"
        );

        let search = json_output(ctx(&temp).args([
            "search",
            query,
            "--provider",
            cli_provider,
            "--refresh",
            "off",
            "--json",
        ]));
        assert_search_provider_oracle(&search, stored_provider, query, 1, "message");

        let second = json_output(ctx(&temp).args([
            "import",
            "--provider",
            cli_provider,
            "--json",
            "--progress",
            "none",
        ]));
        assert_eq!(second["totals"]["failed"], 0);
        assert_eq!(second["totals"]["imported_events"], 0);
    }
}

#[test]
fn mimocode_default_env_and_channel_sources_import_search_and_reimport() {
    let temp = tempdir();
    let default_query = "mimocode-default-discovery-oracle";
    let default_db = temp
        .path()
        .join(".local")
        .join("share")
        .join("mimocode")
        .join("mimocode.db");
    install_default_mimocode_fixture(&temp, default_query);

    let sources = json_output(ctx(&temp).args(["sources", "--json", "--all"]));
    let source = source_by_path(&sources, "mimocode", &default_db);
    assert_eq!(source["status"], "available");
    assert_eq!(source["source_format"], "mimocode_sqlite");
    assert_eq!(source["import_support"], "native");
    assert_eq!(source["native_import"], true);
    assert_eq!(source["importable"], true);

    let search = json_output(ctx(&temp).args([
        "search",
        default_query,
        "--provider",
        "mimo-code",
        "--json",
    ]));
    let freshness_mode = search["freshness"]["mode"].as_str().unwrap();
    assert!(
        matches!(freshness_mode, "auto" | "background"),
        "unexpected freshness mode in {search:#}"
    );
    assert_eq!(search["freshness"]["status"], "completed");
    assert_eq!(search["freshness"]["totals"]["failed"], 0);
    assert!(
        search["freshness"]["totals"]["imported_sessions"]
            .as_u64()
            .unwrap()
            >= 1
    );
    assert!(
        search["freshness"]["totals"]["imported_events"]
            .as_u64()
            .unwrap()
            >= 1
    );
    assert_search_provider_oracle(&search, "mimocode", default_query, 1, "message");

    let second = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "mimo_code",
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(second["totals"]["failed"], 0);
    assert_eq!(second["totals"]["imported_events"], 0);

    let home_query = "mimocode-home-env-oracle";
    let mimocode_home = temp.path().join("mimocode-home");
    let home_db = mimocode_home.join("data").join("mimocode.db");
    write_mimocode_sqlite_fixture(&home_db, home_query, "mimocode-home");
    let home_sources = json_output(
        ctx(&temp)
            .env("MIMOCODE_HOME", &mimocode_home)
            .args(["sources", "--json", "--all"]),
    );
    assert_eq!(
        source_by_path(&home_sources, "mimocode", &home_db)["status"],
        "available"
    );
    assert!(
        !has_provider_source_path(&home_sources, "mimocode", &default_db),
        "MIMOCODE_HOME should replace the default MiMo data root: {home_sources:#}"
    );
    let home_import = json_output(ctx(&temp).env("MIMOCODE_HOME", &mimocode_home).args([
        "import",
        "--provider",
        "mimocode",
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(home_import["totals"]["failed"], 0);
    assert!(home_import["totals"]["imported_events"].as_u64().unwrap() >= 1);
    let home_search = json_output(ctx(&temp).args([
        "search",
        home_query,
        "--provider",
        "mimocode",
        "--refresh",
        "off",
        "--json",
    ]));
    assert_search_provider_oracle(&home_search, "mimocode", home_query, 1, "message");

    let custom_query = "mimocode-db-env-oracle";
    let custom_db = temp.path().join("custom-mimocode.db");
    write_mimocode_sqlite_fixture(&custom_db, custom_query, "mimocode-custom");
    let custom_import = json_output(ctx(&temp).env("MIMOCODE_DB", &custom_db).args([
        "import",
        "--provider",
        "mimocode",
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(
        custom_import["sources"][0]["path"],
        custom_db.display().to_string()
    );
    assert_eq!(custom_import["totals"]["failed"], 0);
    assert!(custom_import["totals"]["imported_events"].as_u64().unwrap() >= 1);

    let xdg_data = temp.path().join("xdg-data");
    let channel_db = xdg_data.join("mimocode").join("mimocode-nightly.db");
    write_mimocode_sqlite_fixture(&channel_db, "mimocode-channel-oracle", "mimocode-channel");
    let channel_sources = json_output(
        ctx(&temp)
            .env("XDG_DATA_HOME", &xdg_data)
            .args(["sources", "--json", "--all"]),
    );
    assert_eq!(
        source_by_path(&channel_sources, "mimocode", &channel_db)["status"],
        "available"
    );
    let disabled_channel_sources = json_output(
        ctx(&temp)
            .env("XDG_DATA_HOME", &xdg_data)
            .env("MIMOCODE_DISABLE_CHANNEL_DB", "1")
            .args(["sources", "--json", "--all"]),
    );
    assert!(
        !has_provider_source_path(&disabled_channel_sources, "mimocode", &channel_db),
        "MIMOCODE_DISABLE_CHANNEL_DB should suppress channel DB discovery"
    );

    let relative_db = xdg_data.join("mimocode").join("relative.db");
    write_mimocode_sqlite_fixture(
        &relative_db,
        "mimocode-relative-db-oracle",
        "mimocode-relative",
    );
    let relative_sources = json_output(
        ctx(&temp)
            .env("XDG_DATA_HOME", &xdg_data)
            .env("MIMOCODE_DB", "relative.db")
            .args(["sources", "--json", "--all"]),
    );
    assert_eq!(
        source_by_path(&relative_sources, "mimocode", &relative_db)["status"],
        "available"
    );
    assert!(
        !has_provider_source_path(&relative_sources, "mimocode", &channel_db),
        "MIMOCODE_DB should select one explicit MiMo database"
    );
}

#[test]
fn windsurf_default_discovery_is_native_and_search_refresh_imports() {
    let temp = tempdir();
    let query = "windsurf-native-default-discovery-oracle";
    install_default_windsurf_fixture(&temp, query);

    let sources = json_output(ctx(&temp).args(["sources", "--json"]));
    let windsurf = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["provider"] == "windsurf")
        .unwrap();
    assert_eq!(windsurf["status"], "available");
    assert_eq!(
        windsurf["source_format"],
        "windsurf_cascade_hook_transcript_jsonl_tree"
    );
    assert_eq!(windsurf["import_support"], "native");
    assert_eq!(windsurf["native_import"], true);
    assert_eq!(windsurf["importable"], true);
    assert!(windsurf["path"]
        .as_str()
        .unwrap()
        .ends_with(".windsurf/transcripts"));

    let search = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "windsurf",
        "--refresh",
        "wait",
        "--json",
    ]));
    assert_eq!(search["freshness"]["mode"], "wait");
    assert_eq!(search["freshness"]["status"], "completed");
    assert_eq!(search["freshness"]["source_count"], 1);
    assert_eq!(search["freshness"]["totals"]["failed"], 0);
    assert_eq!(search["freshness"]["totals"]["imported_sessions"], 1);
    assert_eq!(search["freshness"]["totals"]["imported_events"], 3);
    assert_search_provider_oracle(&search, "windsurf", query, 1, "message");

    let second = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "windsurf",
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(second["totals"]["failed"], 0);
    assert_eq!(second["totals"]["imported_events"], 0);
}

#[test]
fn unknown_native_providers_are_rejected_by_public_cli() {
    let temp = tempdir();

    for provider in ["not-a-real-provider", "unsupported-provider-placeholder"] {
        let stderr = failure_stderr(ctx(&temp).args(["import", "--provider", provider, "--json"]));
        assert!(stderr.contains("unknown provider"), "{provider}: {stderr}");
    }
}

#[test]
fn native_provider_cli_flow_imports_supported_provider_paths() {
    for (cli_provider, stored_provider, expected_format, fixture) in [
        (
            "claude",
            "claude",
            "claude_projects_jsonl_tree",
            write_native_claude_fixture as fn(&TempDir, &str) -> String,
        ),
        (
            "opencode",
            "opencode",
            "opencode_sqlite",
            write_native_opencode_fixture,
        ),
        (
            "mimocode",
            "mimocode",
            "mimocode_sqlite",
            write_native_mimocode_fixture,
        ),
        ("kilo", "kilo", "kilo_sqlite", write_native_kilo_fixture),
        (
            "kiro-cli",
            "kiro_cli",
            "kiro_cli_sqlite",
            write_native_kiro_fixture,
        ),
        (
            "gemini",
            "gemini",
            "gemini_cli_chat_recording_jsonl",
            write_native_gemini_fixture,
        ),
        (
            "cursor",
            "cursor",
            "cursor_agent_transcript_jsonl_tree",
            write_native_cursor_fixture,
        ),
        (
            "windsurf",
            "windsurf",
            "windsurf_cascade_hook_transcript_jsonl_tree",
            write_native_windsurf_fixture,
        ),
        (
            "copilot-cli",
            "copilot_cli",
            "copilot_cli_session_events_jsonl",
            write_native_copilot_fixture,
        ),
        (
            "factory-ai-droid",
            "factory_ai_droid",
            "factory_ai_droid_sessions_jsonl",
            write_native_factory_droid_fixture,
        ),
        (
            "qwen-code",
            "qwen_code",
            "qwen_code_chat_jsonl_tree",
            write_native_qwen_fixture,
        ),
        (
            "kimi-code-cli",
            "kimi_code_cli",
            "kimi_code_cli_wire_jsonl_tree",
            write_native_kimi_fixture,
        ),
        (
            "forgecode",
            "forgecode",
            "forgecode_sqlite",
            write_native_forgecode_fixture,
        ),
        (
            "mistral-vibe",
            "mistral_vibe",
            "mistral_vibe_session_jsonl_tree",
            write_native_mistral_vibe_fixture,
        ),
        (
            "mux",
            "mux",
            "mux_session_jsonl_tree",
            write_native_mux_fixture,
        ),
        (
            "rovodev",
            "rovodev",
            "rovodev_session_json_tree",
            write_native_rovodev_fixture,
        ),
        (
            "lingma",
            "lingma",
            "lingma_sqlite",
            write_native_lingma_fixture,
        ),
        (
            "codebuddy",
            "codebuddy",
            "codebuddy_history_json",
            write_native_codebuddy_fixture,
        ),
        (
            "auggie",
            "auggie",
            "auggie_session_json",
            write_native_auggie_fixture,
        ),
        (
            "junie",
            "junie",
            "junie_session_events_jsonl_tree",
            write_native_junie_fixture,
        ),
        (
            "firebender",
            "firebender",
            "firebender_chat_history_sqlite",
            write_native_firebender_fixture,
        ),
        (
            "openclaw",
            "openclaw",
            "openclaw_session_jsonl_tree",
            write_native_openclaw_fixture,
        ),
        (
            "hermes",
            "hermes",
            "hermes_state_sqlite",
            write_native_hermes_fixture,
        ),
        (
            "nanoclaw",
            "nanoclaw",
            "nanoclaw_project",
            write_native_nanoclaw_fixture,
        ),
        (
            "astrbot",
            "astrbot",
            "astrbot_data_v4_sqlite",
            write_native_astrbot_fixture,
        ),
        (
            "shelley",
            "shelley",
            "shelley_sqlite",
            write_native_shelley_fixture,
        ),
        (
            "continue",
            "continue",
            "continue_cli_sessions_json",
            write_native_continue_fixture,
        ),
        (
            "openhands",
            "openhands",
            "openhands_file_events",
            write_native_openhands_fixture,
        ),
        (
            "qoder",
            "qoder",
            "qoder_transcript_jsonl_tree",
            write_native_qoder_fixture,
        ),
    ] {
        let temp = tempdir();
        let query = format!("{stored_provider}-cli-flow-oracle");
        let path = fixture(&temp, &query);

        let first = json_output(ctx(&temp).args([
            "import",
            "--provider",
            cli_provider,
            "--path",
            &path,
            "--json",
        ]));
        assert_eq!(first["schema_version"], 1);
        assert_eq!(first["sources"][0]["provider"], stored_provider);
        assert_eq!(first["sources"][0]["source_format"], expected_format);
        assert_eq!(first["totals"]["failed"], 0);
        assert!(first["totals"]["imported_sessions"].as_u64().unwrap() >= 1);
        assert!(first["totals"]["imported_events"].as_u64().unwrap() >= 1);

        let search = json_output(ctx(&temp).args([
            "search",
            &query,
            "--provider",
            cli_provider,
            "--refresh",
            "off",
            "--json",
        ]));
        assert_search_provider_oracle(&search, stored_provider, &query, 1, "message");
    }
}

fn source_by_path<'a>(packet: &'a Value, provider: &str, path: &Path) -> &'a Value {
    let expected_path = path.display().to_string();
    packet["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| {
            source["provider"] == provider
                && source["path"]
                    .as_str()
                    .is_some_and(|path| path == expected_path)
        })
        .unwrap_or_else(|| panic!("missing {provider} source {expected_path} in {packet:#}"))
}

fn has_provider_source_path(packet: &Value, provider: &str, path: &Path) -> bool {
    let expected_path = path.display().to_string();
    packet["sources"].as_array().unwrap().iter().any(|source| {
        source["provider"] == provider
            && source["path"]
                .as_str()
                .is_some_and(|path| path == expected_path)
    })
}

#[test]
fn native_provider_cli_policy_excludes_success_tool_outputs_from_search_and_payloads() {
    let temp = tempdir();
    let qoder_query = "qoder-policy-real-message-oracle";
    let qoder_path = write_native_qoder_fixture(&temp, qoder_query);
    let openhands_query = "openhands-policy-real-message-oracle";
    let openhands_path = write_native_openhands_fixture(&temp, openhands_query);
    let continue_query = "continue-policy-real-message-oracle";
    let continue_path = write_native_continue_fixture(&temp, continue_query);

    for (provider, path, query) in [
        ("qoder", qoder_path.as_str(), qoder_query),
        ("openhands", openhands_path.as_str(), openhands_query),
        ("continue", continue_path.as_str(), continue_query),
    ] {
        let imported = json_output(ctx(&temp).args([
            "import",
            "--provider",
            provider,
            "--path",
            path,
            "--json",
            "--progress",
            "none",
        ]));
        assert_eq!(imported["totals"]["failed"], 0, "{imported:#}");

        let search = json_output(ctx(&temp).args([
            "search",
            query,
            "--provider",
            provider,
            "--refresh",
            "off",
            "--json",
        ]));
        assert_search_provider_oracle(&search, provider, query, 1, "message");
    }

    for (provider, sentinel) in [
        ("qoder", "qoder-success-tool-output-sentinel"),
        ("openhands", "openhands-success-tool-output-sentinel"),
        ("continue", "continue-success-tool-output-sentinel"),
    ] {
        let search = json_output(ctx(&temp).args([
            "search",
            sentinel,
            "--provider",
            provider,
            "--refresh",
            "off",
            "--json",
        ]));
        assert!(
            search["results"].as_array().unwrap().is_empty(),
            "{provider} success tool output leaked into search: {search:#}"
        );
    }

    let conn = Connection::open(temp.path().join("work.sqlite")).unwrap();
    assert_eq!(
        sqlite_count(
            &conn,
            "SELECT COUNT(*) FROM events WHERE payload_json LIKE '%qoder-success-tool-output-sentinel%'",
        ),
        0
    );
    assert_eq!(
        sqlite_count(
            &conn,
            "SELECT COUNT(*) FROM events WHERE payload_json LIKE '%openhands-success-tool-output-sentinel%'",
        ),
        0
    );
    assert_eq!(
        sqlite_count(
            &conn,
            "SELECT COUNT(*) FROM events WHERE payload_json LIKE '%continue-success-tool-output-sentinel%'",
        ),
        0
    );
    assert!(
        sqlite_count(
            &conn,
            "SELECT COUNT(*) FROM files_touched WHERE path = 'openhands-cli-native-oracle.txt'",
        ) > 0
    );
}

#[test]
fn trae_cli_imports_explicit_workspace_storage_with_default_discovery() {
    let temp = tempdir();
    let empty_sources = json_output(ctx(&temp).args(["sources", "--json", "--all"]));
    let trae_source = empty_sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["provider"] == "trae")
        .unwrap_or_else(|| panic!("missing Trae default source: {empty_sources:#}"));
    assert_eq!(trae_source["status"], "missing");
    assert_eq!(trae_source["source_format"], "trae_state_vscdb");
    assert_eq!(trae_source["import_support"], "native");
    assert_eq!(trae_source["native_import"], true);
    assert_eq!(trae_source["importable"], false);

    let fixture = provider_history_fixture("trae/User/workspaceStorage");
    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "trae-cn",
        "--path",
        &fixture,
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(imported["schema_version"], 1);
    assert_eq!(imported["sources"][0]["provider"], "trae");
    assert_eq!(imported["sources"][0]["source_format"], "trae_state_vscdb");
    assert_eq!(imported["totals"]["failed"], 0);
    assert_eq!(imported["totals"]["imported_sessions"], 1);
    assert_eq!(imported["totals"]["imported_events"], 2);

    let search = json_output(ctx(&temp).args([
        "search",
        "trae oracle answer",
        "--provider",
        "trae-cn",
        "--refresh",
        "off",
        "--json",
    ]));
    assert_search_provider_oracle_with_scope(
        &search,
        "trae",
        "trae oracle answer",
        1,
        "message",
        "session_result",
        "session",
    );

    let second = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "trae",
        "--path",
        &fixture,
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(second["totals"]["failed"], 0);
    assert_eq!(second["totals"]["imported_sessions"], 0);
    assert_eq!(second["totals"]["imported_events"], 0);
}

#[test]
fn trae_cn_native_default_discovery_search_refresh_imports_input_history() {
    let temp = tempdir();
    let query = "trae-cn-default-discovery-oracle";
    install_default_trae_cn_fixture(&temp, query);

    let sources = json_output(ctx(&temp).args(["sources", "--json"]));
    let source = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| {
            source["provider"] == "trae"
                && source["status"] == "available"
                && source["path"]
                    .as_str()
                    .is_some_and(|path| path.ends_with("Trae CN/User/workspaceStorage"))
        })
        .unwrap_or_else(|| panic!("missing Trae CN source in {sources:#}"));
    assert_eq!(source["status"], "available");
    assert_eq!(source["source_format"], "trae_state_vscdb");
    assert_eq!(source["import_support"], "native");
    assert_eq!(source["native_import"], true);
    assert!(source["path"]
        .as_str()
        .unwrap()
        .ends_with("Trae CN/User/workspaceStorage"));

    let search = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "trae-cn",
        "--refresh",
        "wait",
        "--json",
    ]));
    assert_eq!(search["freshness"]["mode"], "wait");
    assert_eq!(search["freshness"]["status"], "completed");
    assert_eq!(search["freshness"]["source_count"], 1);
    assert_eq!(search["freshness"]["totals"]["failed"], 0);
    assert_eq!(search["freshness"]["totals"]["imported_sessions"], 1);
    assert_eq!(search["freshness"]["totals"]["imported_events"], 2);
    assert_search_provider_oracle_with_scope(
        &search,
        "trae",
        query,
        1,
        "message",
        "session_result",
        "session",
    );
}

#[test]
fn trae_native_default_discovery_search_refresh_imports_standard_workspace_storage() {
    let temp = tempdir();
    let query = "trae-standard-default-discovery-oracle";
    install_default_trae_fixture(&temp, query);

    let sources = json_output(ctx(&temp).args(["sources", "--json"]));
    let source = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| {
            source["provider"] == "trae"
                && source["status"] == "available"
                && source["path"]
                    .as_str()
                    .is_some_and(|path| path.ends_with("Trae/User/workspaceStorage"))
        })
        .unwrap_or_else(|| panic!("missing standard Trae source in {sources:#}"));
    assert_eq!(source["source_format"], "trae_state_vscdb");
    assert_eq!(source["import_support"], "native");
    assert_eq!(source["native_import"], true);

    let search = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "trae",
        "--refresh",
        "wait",
        "--json",
    ]));
    assert_eq!(search["freshness"]["mode"], "wait");
    assert_eq!(search["freshness"]["status"], "completed");
    assert_eq!(search["freshness"]["source_count"], 1);
    assert_eq!(search["freshness"]["totals"]["failed"], 0);
    assert_eq!(search["freshness"]["totals"]["imported_sessions"], 1);
    assert_eq!(search["freshness"]["totals"]["imported_events"], 2);
    assert_search_provider_oracle_with_scope(
        &search,
        "trae",
        query,
        1,
        "message",
        "session_result",
        "session",
    );
}

#[test]
fn trae_cn_native_default_discovery_is_included_in_import_all() {
    let temp = tempdir();
    let query = "trae-cn-import-all-oracle";
    install_default_trae_cn_fixture(&temp, query);

    let imported =
        json_output(ctx(&temp).args(["import", "--all", "--json", "--progress", "none"]));
    assert!(imported["sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|source| {
            source["provider"] == "trae"
                && source["source_format"] == "trae_state_vscdb"
                && source["import_support"] == "native"
        }));
    assert_eq!(imported["totals"]["failed"], 0);
    assert_eq!(imported["totals"]["imported_sessions"], 1);
    assert_eq!(imported["totals"]["imported_events"], 2);

    let search = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "trae-cn",
        "--refresh",
        "off",
        "--json",
    ]));
    assert_search_provider_oracle_with_scope(
        &search,
        "trae",
        query,
        1,
        "message",
        "session_result",
        "session",
    );
}

#[test]
fn astrbot_native_default_discovery_is_included_in_import_all() {
    let temp = tempdir();
    let query = "astrbot-import-all-oracle";
    install_default_astrbot_fixture(&temp, query);

    let imported =
        json_output(ctx(&temp).args(["import", "--all", "--json", "--progress", "none"]));
    assert!(imported["sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|source| {
            source["provider"] == "astrbot"
                && source["source_format"] == "astrbot_data_v4_sqlite"
                && source["import_support"] == "native"
        }));
    assert_eq!(imported["totals"]["failed"], 0);
    assert_eq!(imported["totals"]["imported_sessions"], 1);
    assert_eq!(imported["totals"]["imported_events"], 3);

    let search = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "astrbot",
        "--refresh",
        "off",
        "--json",
    ]));
    assert_search_provider_oracle(&search, "astrbot", query, 1, "message");
}

#[test]
fn warp_cli_imports_explicit_sqlite() {
    let temp = tempdir();
    let fixture = provider_history_fixture("warp/v1/warp.sqlite");
    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "warp",
        "--path",
        &fixture,
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(imported["schema_version"], 1);
    assert_eq!(imported["sources"][0]["provider"], "warp");
    assert_eq!(imported["sources"][0]["source_format"], "warp_sqlite");
    assert_eq!(imported["totals"]["failed"], 0);
    assert_eq!(imported["totals"]["imported_sessions"], 1);
    assert_eq!(imported["totals"]["imported_events"], 4);

    let search = json_output(ctx(&temp).args([
        "search",
        "Warp sqlite oracle answer",
        "--provider",
        "warp",
        "--refresh",
        "off",
        "--json",
    ]));
    assert_search_provider_oracle(&search, "warp", "Warp sqlite oracle answer", 1, "message");

    let second = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "warp",
        "--path",
        &fixture,
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(second["totals"]["failed"], 0);
    assert_eq!(second["totals"]["imported_sessions"], 0);
    assert_eq!(second["totals"]["imported_events"], 0);
}

#[test]
fn warp_native_default_discovery_auto_imports_for_search() {
    let temp = tempdir();
    install_default_warp_fixture(&temp);

    let sources = json_output(ctx(&temp).args(["sources", "--json"]));
    let source = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["provider"] == "warp")
        .unwrap_or_else(|| panic!("missing Warp source in {sources:#}"));
    assert_eq!(source["status"], "available");
    assert_eq!(source["source_format"], "warp_sqlite");
    assert_eq!(source["import_support"], "native");
    assert_eq!(source["native_import"], true);
    assert_eq!(source["importable"], true);

    let search = json_output(ctx(&temp).args([
        "search",
        "Warp sqlite oracle answer",
        "--provider",
        "warp",
        "--refresh",
        "wait",
        "--json",
    ]));
    assert_eq!(search["freshness"]["mode"], "wait");
    assert_eq!(search["freshness"]["status"], "completed");
    assert_eq!(search["freshness"]["source_count"], 1);
    assert_eq!(search["freshness"]["totals"]["failed"], 0);
    assert_eq!(search["freshness"]["totals"]["imported_sessions"], 1);
    assert_eq!(search["freshness"]["totals"]["imported_events"], 4);
    assert_search_provider_oracle(&search, "warp", "Warp sqlite oracle answer", 1, "message");
}

#[test]
fn warp_native_default_discovery_is_included_in_import_all() {
    let temp = tempdir();
    install_default_warp_fixture(&temp);

    let imported =
        json_output(ctx(&temp).args(["import", "--all", "--json", "--progress", "none"]));
    assert!(imported["sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|source| {
            source["provider"] == "warp" && source["source_format"] == "warp_sqlite"
        }));
    assert_eq!(imported["totals"]["failed"], 0);
    assert_eq!(imported["totals"]["imported_sessions"], 1);
    assert_eq!(imported["totals"]["imported_events"], 4);

    let search = json_output(ctx(&temp).args([
        "search",
        "Warp sqlite oracle answer",
        "--provider",
        "warp",
        "--refresh",
        "off",
        "--json",
    ]));
    assert_search_provider_oracle(&search, "warp", "Warp sqlite oracle answer", 1, "message");
}

#[test]
fn lingma_cli_default_source_imports_home_local_db() {
    let temp = tempdir();
    let query = "lingma-default-import-oracle";
    install_default_lingma_fixture(&temp, query);

    let sources = json_output(ctx(&temp).args(["sources", "--json"]));
    let source = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["provider"] == "lingma")
        .unwrap_or_else(|| panic!("missing Lingma source in {sources:#}"));
    assert_eq!(source["source_format"], "lingma_sqlite");
    assert_eq!(source["status"], "available");
    assert_eq!(source["importable"], true);

    let imported = json_output(ctx(&temp).args(["import", "--provider", "lingma", "--json"]));
    assert_eq!(imported["sources"][0]["provider"], "lingma");
    assert_eq!(imported["sources"][0]["source_format"], "lingma_sqlite");
    assert_eq!(imported["totals"]["failed"], 0);
    assert_eq!(imported["totals"]["imported_sessions"], 1);
    assert_eq!(imported["totals"]["imported_events"], 2);

    let search = json_output(ctx(&temp).args(["search", query, "--provider", "lingma", "--json"]));
    assert_search_provider_oracle(&search, "lingma", query, 1, "message");

    let alias_search =
        json_output(ctx(&temp).args(["search", query, "--provider", "qoder-cn", "--json"]));
    assert_search_provider_oracle(&alias_search, "lingma", query, 1, "message");

    let second = json_output(ctx(&temp).args(["import", "--provider", "lingma", "--json"]));
    assert_eq!(second["totals"]["failed"], 0);
    assert_eq!(second["totals"]["imported_events"], 0);
}

#[test]
fn tabnine_cli_imports_explicit_agent_home_searches_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("tabnine-cli/.tabnine/agent");

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "tabnine",
        "--path",
        &fixture,
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(imported["schema_version"], 1);
    assert_eq!(imported["sources"][0]["provider"], "tabnine");
    assert_eq!(
        imported["sources"][0]["source_format"],
        "tabnine_cli_chat_recording_jsonl"
    );
    assert_eq!(imported["totals"]["failed"], 0);
    assert_eq!(imported["totals"]["imported_sessions"], 2);
    assert_eq!(imported["totals"]["imported_events"], 7);

    let search = json_output(ctx(&temp).args([
        "search",
        "tabnine jsonl oracle answer",
        "--provider",
        "tabnine",
        "--refresh",
        "off",
        "--json",
    ]));
    assert_search_provider_oracle(
        &search,
        "tabnine",
        "tabnine jsonl oracle answer",
        1,
        "message",
    );

    let second = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "tabnine",
        "--path",
        &fixture,
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(second["totals"]["failed"], 0);
    assert_eq!(second["totals"]["imported_sessions"], 0);
    assert_eq!(second["totals"]["imported_events"], 0);
}

#[test]
fn deepagents_cli_sources_import_search_and_reimport_with_aliases() {
    let temp = tempdir();
    let default_db = temp.path().join(".deepagents/.state/sessions.db");
    fs::create_dir_all(default_db.parent().unwrap()).unwrap();
    fs::copy(
        provider_history_fixture("deepagents/v1/sessions.db"),
        &default_db,
    )
    .unwrap();

    let sources = json_output(ctx(&temp).args(["sources", "--json"]));
    let source = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["provider"] == "deepagents")
        .unwrap_or_else(|| panic!("missing Deep Agents source in {sources:#}"));
    assert_eq!(source["status"], "available");
    assert_eq!(source["source_format"], "deepagents_sessions_sqlite");
    assert_eq!(source["import_support"], "native");
    assert_eq!(source["importable"], true);

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "deep-agents",
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(imported["sources"][0]["provider"], "deepagents");
    assert_eq!(
        imported["sources"][0]["source_format"],
        "deepagents_sessions_sqlite"
    );
    assert_eq!(imported["totals"]["failed"], 0);
    assert_eq!(imported["totals"]["imported_sessions"], 1);
    assert_eq!(imported["totals"]["imported_events"], 3);

    let search = json_output(ctx(&temp).args([
        "search",
        "deepagents fixture oracle",
        "--provider",
        "dcode",
        "--refresh",
        "off",
        "--json",
    ]));
    assert_search_provider_oracle(
        &search,
        "deepagents",
        "deepagents fixture oracle",
        1,
        "message",
    );

    let second = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "deepagents",
        "--path",
        default_db.to_str().unwrap(),
        "--json",
    ]));
    assert_eq!(second["totals"]["failed"], 0);
    assert_eq!(second["totals"]["imported_events"], 0);
    let conn = Connection::open(temp.path().join("work.sqlite")).unwrap();
    assert_eq!(
        sqlite_count(
            &conn,
            "SELECT COUNT(*) FROM events e JOIN sessions s ON e.session_id = s.id WHERE s.provider = 'deepagents'"
        ),
        3
    );
}

#[test]
fn sqlite_cli_imports_crush_goose_zed_kiro_and_forgecode_and_searches() {
    for (cli_provider, stored_provider, source_format, fixture, query, sessions, events) in [
        (
            "zed",
            "zed",
            "zed_threads_sqlite",
            "zed/v1/threads.db",
            "zed sqlite oracle",
            2,
            6,
        ),
        (
            "crush",
            "crush",
            "crush_sqlite",
            "crush/v1/crush.db",
            "crush oracle",
            1,
            3,
        ),
        (
            "goose",
            "goose",
            "goose_sessions_sqlite",
            "goose/v14/sessions.db",
            "goose oracle",
            1,
            3,
        ),
        (
            "kiro-cli",
            "kiro_cli",
            "kiro_cli_sqlite",
            "kiro-cli/v2/data.sqlite3",
            "kiro oracle",
            1,
            3,
        ),
        (
            "forgecode",
            "forgecode",
            "forgecode_sqlite",
            "forgecode/v1/forge.db",
            "forgecode oracle",
            1,
            3,
        ),
    ] {
        let temp = tempdir();
        let fixture = provider_history_fixture(fixture);

        let imported = json_output(ctx(&temp).args([
            "import",
            "--provider",
            cli_provider,
            "--path",
            &fixture,
            "--json",
            "--progress",
            "none",
        ]));
        assert_eq!(imported["schema_version"], 1);
        assert_eq!(imported["sources"][0]["provider"], stored_provider);
        assert_eq!(imported["sources"][0]["source_format"], source_format);
        assert_eq!(imported["totals"]["failed"], 0);
        assert_eq!(imported["totals"]["imported_sessions"], sessions);
        assert_eq!(imported["totals"]["imported_events"], events);

        let search = json_output(ctx(&temp).args([
            "search",
            query,
            "--provider",
            cli_provider,
            "--refresh",
            "off",
            "--json",
        ]));
        assert_search_provider_oracle(&search, stored_provider, query, 1, "message");

        let result = &search["results"].as_array().unwrap()[0];
        let ctx_event_id = result["ctx_event_id"].as_str().unwrap();
        let located = json_output(ctx(&temp).args(["locate", "event", ctx_event_id, "--json"]));
        assert_eq!(located["provider"], stored_provider);
        assert_eq!(located["source"]["source_format"], source_format);
        assert!(located["source"]["path"]
            .as_str()
            .is_some_and(|path| path.ends_with(".db") || path.ends_with(".sqlite3")));

        let second = json_output(ctx(&temp).args([
            "import",
            "--provider",
            cli_provider,
            "--path",
            &fixture,
            "--json",
            "--progress",
            "none",
        ]));
        assert_eq!(second["totals"]["failed"], 0);
        assert_eq!(second["totals"]["imported_sessions"], 0);
        assert_eq!(second["totals"]["imported_events"], 0);
    }
}

#[test]
fn personal_agent_provider_imports_are_idempotent_and_incremental() {
    for (cli_provider, stored_provider, fixture, append_event) in [
        (
            "openclaw",
            "openclaw",
            write_native_openclaw_fixture as fn(&TempDir, &str) -> String,
            append_native_openclaw_event as fn(&str, &str),
        ),
        (
            "hermes",
            "hermes",
            write_native_hermes_fixture,
            append_native_hermes_event,
        ),
        (
            "nanoclaw",
            "nanoclaw",
            write_native_nanoclaw_fixture,
            append_native_nanoclaw_event,
        ),
        (
            "astrbot",
            "astrbot",
            write_native_astrbot_fixture,
            append_native_astrbot_event,
        ),
        (
            "shelley",
            "shelley",
            write_native_shelley_fixture,
            append_native_shelley_event,
        ),
    ] {
        let temp = tempdir();
        let initial_query = format!("{stored_provider}-incremental-initial-oracle");
        let incremental_query = format!("{stored_provider}-incremental-next-oracle");
        let path = fixture(&temp, &initial_query);

        let first = json_output(ctx(&temp).args([
            "import",
            "--provider",
            cli_provider,
            "--path",
            &path,
            "--json",
        ]));
        assert_eq!(first["totals"]["failed"], 0);
        assert!(first["totals"]["imported_events"].as_u64().unwrap() >= 1);

        let second = json_output(ctx(&temp).args([
            "import",
            "--provider",
            cli_provider,
            "--path",
            &path,
            "--json",
        ]));
        assert_eq!(second["totals"]["failed"], 0);
        assert_eq!(second["totals"]["imported_events"], 0);

        append_event(&path, &incremental_query);
        let third = json_output(ctx(&temp).args([
            "import",
            "--provider",
            cli_provider,
            "--path",
            &path,
            "--json",
        ]));
        assert_eq!(third["totals"]["failed"], 0);
        assert!(third["totals"]["imported_events"].as_u64().unwrap() >= 1);

        let search = json_output(ctx(&temp).args([
            "search",
            &incremental_query,
            "--provider",
            cli_provider,
            "--json",
        ]));
        assert_search_provider_oracle(&search, stored_provider, &incremental_query, 1, "message");
    }
}

#[test]
fn openclaw_import_accepts_explicit_session_jsonl_file() {
    let temp = tempdir();
    let query = "openclaw-explicit-file-oracle";
    let path = temp.path().join("openclaw-single-session.jsonl");
    fs::write(
        &path,
        format!(
            "{}\n{}\n",
            json!({
                "type": "session",
                "id": "openclaw-single-session",
                "timestamp": "2026-06-24T12:00:00Z"
            }),
            json!({
                "type": "message",
                "id": "openclaw-single-user",
                "timestamp": "2026-06-24T12:00:01Z",
                "message": {"role": "user", "content": query}
            })
        ),
    )
    .unwrap();

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "openclaw",
        "--path",
        path.to_str().unwrap(),
        "--json",
    ]));
    assert_eq!(imported["totals"]["failed"], 0);
    assert_eq!(imported["totals"]["imported_sources"], 1);

    let search =
        json_output(ctx(&temp).args(["search", query, "--provider", "openclaw", "--json"]));
    assert_search_provider_oracle(&search, "openclaw", query, 1, "message");
}

#[test]
fn nanoclaw_import_tolerates_partial_auxiliary_tables() {
    let temp = tempdir();
    let query = "nanoclaw-partial-auxiliary-schema-oracle";
    let path = write_native_nanoclaw_fixture(&temp, query);
    let conn = Connection::open(Path::new(&path).join("data/v2.db")).unwrap();
    conn.execute_batch(
        "drop table agent_groups;
         create table agent_groups (id text primary key);
         insert into agent_groups values ('ag-1');
         drop table messaging_groups;
         create table messaging_groups (id text primary key);
         insert into messaging_groups values ('mg-1');",
    )
    .unwrap();

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "nanoclaw",
        "--path",
        &path,
        "--json",
    ]));
    assert_eq!(imported["totals"]["failed"], 0);
    assert_eq!(imported["totals"]["imported_sources"], 1);

    let search =
        json_output(ctx(&temp).args(["search", query, "--provider", "nanoclaw", "--json"]));
    assert_search_provider_oracle(&search, "nanoclaw", query, 1, "message");
}

#[test]
fn personal_agent_sqlite_imports_report_corrupt_databases() {
    for (provider, path) in [
        ("hermes", "corrupt-hermes-state.db"),
        ("astrbot", "corrupt-astrbot-data_v4.db"),
        ("shelley", "corrupt-shelley.db"),
        ("lingma", "corrupt-lingma-local.db"),
    ] {
        let temp = tempdir();
        let db_path = temp.path().join(path);
        fs::write(&db_path, b"not sqlite").unwrap();
        let output = ctx(&temp)
            .args([
                "import",
                "--provider",
                provider,
                "--path",
                db_path.to_str().unwrap(),
                "--json",
            ])
            .assert()
            .failure()
            .get_output()
            .stderr
            .clone();
        let stderr = String::from_utf8(output).unwrap();
        assert!(stderr.contains("not a database"), "{stderr}");
    }

    let temp = tempdir();
    let root = temp.path().join("corrupt-nanoclaw");
    fs::create_dir_all(root.join("data/v2-sessions")).unwrap();
    fs::write(root.join("data/v2.db"), b"not sqlite").unwrap();
    let output = ctx(&temp)
        .args([
            "import",
            "--provider",
            "nanoclaw",
            "--path",
            root.to_str().unwrap(),
            "--json",
        ])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let stderr = String::from_utf8(output).unwrap();
    assert!(stderr.contains("not a database"), "{stderr}");
}

#[test]
fn native_provider_cli_requires_existing_history_or_explicit_path() {
    for (cli_provider, expected_blocker) in [
        ("claude", "no importable claude history found"),
        ("opencode", "no importable opencode history found"),
        ("kilo", "no importable kilo history found"),
        ("antigravity", "no importable antigravity history found"),
        ("gemini", "no importable gemini history found"),
        ("cursor", "no importable cursor history found"),
        ("zed", "no importable zed history found"),
        ("copilot-cli", "no importable copilot_cli history found"),
        (
            "factory-ai-droid",
            "no importable factory_ai_droid history found",
        ),
        ("openclaw", "no importable openclaw history found"),
        ("hermes", "no importable hermes history found"),
        ("nanoclaw", "no importable nanoclaw history found"),
        ("astrbot", "no importable astrbot history found"),
        ("shelley", "no importable shelley history found"),
        ("lingma", "no importable lingma history found"),
        ("codebuddy", "no importable codebuddy history found"),
        ("auggie", "no importable auggie history found"),
        ("deepagents", "no importable deepagents history found"),
        ("mistral-vibe", "no importable mistral_vibe history found"),
        ("mux", "no importable mux history found"),
        ("cline", "no importable cline history found"),
        ("roo", "no importable roo_code history found"),
    ] {
        let temp = tempdir();
        let stderr =
            failure_stderr(ctx(&temp).args(["import", "--provider", cli_provider, "--json"]));

        assert!(stderr.contains(expected_blocker), "{stderr}");
        assert!(stderr.contains("use `ctx sources`"), "{stderr}");
        if cli_provider == "nanoclaw" {
            assert!(
                stderr.contains("no default paths are registered for this provider"),
                "{stderr}"
            );
        } else {
            assert!(stderr.contains("checked paths:"), "{stderr}");
            assert!(stderr.contains(temp.path().to_str().unwrap()), "{stderr}");
        }
    }
}

#[test]
fn task_json_cli_imports_cline_and_roo_and_searches() {
    let temp = tempdir();
    let cline = provider_history_fixture("cline/data");

    let imported =
        json_output(ctx(&temp).args(["import", "--provider", "cline", "--path", &cline, "--json"]));
    assert_eq!(imported["schema_version"], 1);
    assert_eq!(imported["sources"][0]["provider"], "cline");
    assert_eq!(
        imported["sources"][0]["source_format"],
        "cline_task_directory_json"
    );
    assert_eq!(imported["totals"]["imported_sessions"], 1);
    assert_eq!(imported["totals"]["imported_events"], 4);
    assert_eq!(imported["totals"]["failed"], 0);

    let second =
        json_output(ctx(&temp).args(["import", "--provider", "cline", "--path", &cline, "--json"]));
    assert_eq!(second["totals"]["imported_sessions"], 0);
    assert_eq!(second["totals"]["imported_events"], 0);
    assert_eq!(second["totals"]["skipped_events"], 4);

    let search =
        json_output(ctx(&temp).args(["search", "parser note", "--provider", "cline", "--json"]));
    assert_search_provider_oracle(&search, "cline", "parser note", 1, "message");

    let roo = provider_history_fixture("roo/storage");
    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "roo-code",
        "--path",
        &roo,
        "--json",
    ]));
    assert_eq!(imported["schema_version"], 1);
    assert_eq!(imported["sources"][0]["provider"], "roo_code");
    assert_eq!(
        imported["sources"][0]["source_format"],
        "roo_task_directory_json"
    );
    assert_eq!(imported["totals"]["imported_sessions"], 2);
    assert_eq!(imported["totals"]["imported_events"], 6);
    assert_eq!(imported["totals"]["failed"], 0);

    let search = json_output(ctx(&temp).args([
        "search",
        "fallback claude_messages",
        "--provider",
        "roo",
        "--json",
    ]));
    assert_search_provider_oracle(
        &search,
        "roo_code",
        "fallback claude_messages",
        1,
        "message",
    );
}

#[test]
fn antigravity_cli_imports_native_transcript_tree() {
    let temp = tempdir();
    let fixture = provider_history_fixture("antigravity/v1/brain");

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "antigravity",
        "--path",
        &fixture,
        "--partial",
        "--json",
    ]));
    assert_eq!(imported["schema_version"], 1);
    assert_eq!(imported["sources"][0]["provider"], "antigravity");
    assert_eq!(
        imported["sources"][0]["source_format"],
        "antigravity_cli_transcript_jsonl_tree"
    );
    assert_eq!(imported["totals"]["imported_sessions"], 3);
    assert_eq!(imported["totals"]["imported_events"], 9);
    assert_eq!(imported["totals"]["failed"], 2);

    let search = json_output(ctx(&temp).args([
        "search",
        "write_to_file",
        "--provider",
        "antigravity",
        "--json",
    ]));
    assert_search_provider_oracle(&search, "antigravity", "write_to_file", 1, "tool_call");
}

#[test]
fn antigravity_cli_inventory_prefers_full_transcript_over_live_partial() {
    let temp = tempdir();
    let source_fixture = PathBuf::from(provider_history_fixture("antigravity/v1/brain"));
    let brain = temp.path().join("brain");
    let logs = brain
        .join("agy-success")
        .join(".system_generated")
        .join("logs");
    fs::create_dir_all(&logs).unwrap();
    fs::copy(
        source_fixture
            .join("agy-success")
            .join(".system_generated")
            .join("logs")
            .join("transcript_full.jsonl"),
        logs.join("transcript_full.jsonl"),
    )
    .unwrap();
    fs::write(logs.join("transcript.jsonl"), b"{\"type\":\"partial\"\n").unwrap();

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "antigravity",
        "--path",
        brain.to_str().unwrap(),
        "--partial",
        "--json",
    ]));
    assert_eq!(imported["totals"]["source_files"], 1, "{imported:#}");
    assert_eq!(imported["totals"]["failed"], 0, "{imported:#}");
    assert_eq!(imported["totals"]["imported_sessions"], 1, "{imported:#}");
}

#[test]
fn antigravity_cli_malformed_default_import_is_atomic() {
    let temp = tempdir();
    let fixture = provider_history_fixture("antigravity/v1/brain");

    let stderr = failure_stderr(ctx(&temp).args([
        "import",
        "--provider",
        "antigravity",
        "--path",
        &fixture,
        "--json",
    ]));
    assert!(stderr.contains("failed with 1 failure"), "{stderr}");
    assert_import_store_empty_after_atomic_failure(&temp);

    let search = json_output(ctx(&temp).args([
        "search",
        "write_to_file",
        "--provider",
        "antigravity",
        "--refresh",
        "off",
        "--json",
    ]));
    assert!(search["results"].as_array().unwrap().is_empty());
}

#[test]
fn codex_cli_reports_malformed_partial_import_progress() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-malformed-session.jsonl");

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--partial",
        "--json",
    ]));
    assert_eq!(imported["schema_version"], 1);
    assert_eq!(imported["totals"]["imported_sessions"], 1);
    assert_eq!(imported["totals"]["imported_events"], 2);
    assert_eq!(imported["totals"]["failed"], 1);
    assert_eq!(imported["sources"][0]["failed"], 1);

    let search = json_output(ctx(&temp).args(["search", "after malformed", "--json"]));
    assert!(!search["results"].as_array().unwrap().is_empty());
}

#[test]
fn codex_cli_malformed_default_import_is_atomic() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-malformed-session.jsonl");

    let stderr = failure_stderr(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--json",
    ]));
    assert!(stderr.contains("failed with 1 failure"), "{stderr}");
    assert_import_store_empty_after_atomic_failure(&temp);

    let search =
        json_output(ctx(&temp).args(["search", "after malformed", "--refresh", "off", "--json"]));
    assert!(search["results"].as_array().unwrap().is_empty());
}

#[test]
fn pi_cli_reports_malformed_partial_and_schema_failures() {
    let temp = tempdir();
    let fixture = provider_history_fixture("pi-malformed-partial.jsonl");

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "pi",
        "--path",
        &fixture,
        "--partial",
        "--json",
    ]));
    assert_eq!(imported["schema_version"], 1);
    assert_eq!(imported["totals"]["imported_sessions"], 1);
    assert_eq!(imported["totals"]["imported_events"], 2);
    assert_eq!(imported["totals"]["failed"], 2);
    assert_eq!(imported["sources"][0]["failed"], 2);
    assert_eq!(
        imported["sources"][0]["failures"].as_array().unwrap().len(),
        2
    );

    let query = "after malformed line";
    let search = json_output(ctx(&temp).args(["search", query, "--provider", "pi", "--json"]));
    assert_search_provider_oracle(&search, "pi", query, 1, "message");
}

#[test]
fn pi_cli_malformed_default_import_is_atomic() {
    let temp = tempdir();
    let fixture = provider_history_fixture("pi-malformed-partial.jsonl");

    let stderr = failure_stderr(ctx(&temp).args([
        "import",
        "--provider",
        "pi",
        "--path",
        &fixture,
        "--json",
    ]));
    assert!(stderr.contains("failed with 2 failure"), "{stderr}");
    assert_import_store_empty_after_atomic_failure(&temp);

    let search = json_output(ctx(&temp).args([
        "search",
        "after malformed line",
        "--provider",
        "pi",
        "--refresh",
        "off",
        "--json",
    ]));
    assert!(search["results"].as_array().unwrap().is_empty());
}

#[test]
fn import_all_continues_after_atomic_source_failure_without_bad_rows() {
    let temp = tempdir();
    let codex_dir = temp.path().join(".codex/sessions/2026/07/03");
    fs::create_dir_all(&codex_dir).unwrap();
    fs::copy(
        provider_history_fixture("codex-malformed-session.jsonl"),
        codex_dir.join("bad.jsonl"),
    )
    .unwrap();
    let pi_query = "pi import all survives malformed codex";
    install_default_pi_fixture(&temp, pi_query);

    let imported =
        json_output(ctx(&temp).args(["import", "--all", "--json", "--progress", "none"]));
    assert_eq!(imported["totals"]["failed_sources"], 1, "{imported:#}");
    assert!(imported["sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|source| { source["provider"] == "codex" && source["status"] == "failed" }));
    assert!(imported["sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|source| { source["provider"] == "pi" && source["status"] == "imported" }));

    let pi_search = json_output(ctx(&temp).args([
        "search",
        pi_query,
        "--provider",
        "pi",
        "--refresh",
        "off",
        "--json",
    ]));
    assert_search_provider_oracle(&pi_search, "pi", pi_query, 1, "message");
    let codex_search = json_output(ctx(&temp).args([
        "search",
        "after malformed",
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--json",
    ]));
    assert!(codex_search["results"].as_array().unwrap().is_empty());
    assert_no_history_record_for_provider(&temp, "codex");
}

fn assert_import_store_empty_after_atomic_failure(temp: &TempDir) {
    let conn = Connection::open(temp.path().join("work.sqlite")).unwrap();
    for table in [
        "history_records",
        "sessions",
        "events",
        "ctx_history_search",
        "event_search",
    ] {
        let count: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            count, 0,
            "{table} should be empty after atomic import failure"
        );
    }
}

fn assert_no_history_record_for_provider(temp: &TempDir, provider: &str) {
    let conn = Connection::open(temp.path().join("work.sqlite")).unwrap();
    let title = format!("{provider} agent history");
    let record_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM history_records WHERE title = ?1",
            params![&title],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        record_count, 0,
        "{provider} source record should not persist"
    );
    let search_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ctx_history_search WHERE title = ?1",
            params![&title],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        search_count, 0,
        "{provider} source search row should not persist"
    );
}
