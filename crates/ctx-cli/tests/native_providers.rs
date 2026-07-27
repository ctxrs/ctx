mod support;

use support::*;

#[path = "support/native_providers/sqlite_sources.rs"]
mod sqlite_sources;
#[path = "support/native_providers/workspace_sources.rs"]
mod workspace_sources;

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

    let sources = json_output(ctx(&temp).args(["sources", "--format=json"]));
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
        ("qwen-code", "qwen_code", "qwen jsonl oracle prompt", 2),
        (
            "kimi-code-cli",
            "kimi_code_cli",
            "kimi jsonl oracle prompt",
            6,
        ),
        (
            "mistral-vibe",
            "mistral_vibe",
            "mistral vibe oracle prompt",
            3,
        ),
        ("mux", "mux", "mux jsonl oracle prompt", 4),
        ("qoder", "qoder", "qoder jsonl oracle prompt", 6),
    ] {
        let first = json_output(ctx(&temp).args([
            "import",
            "--provider",
            cli_provider,
            "--format=json",
            "--progress",
            "none",
        ]));
        assert_eq!(first["totals"]["rejected_records"], 0, "{first:#}");
        assert_eq!(first["totals"]["imported_sources"], 1);
        assert!(
            first["totals"]["imported_events"].as_u64().unwrap() >= minimum_events,
            "{first:#}"
        );
        if stored_provider == "mux" {
            let conn = Connection::open(temp.path().join("work.sqlite")).unwrap();
            assert_eq!(
                sqlite_count(
                    &conn,
                    "SELECT COUNT(*) FROM ctx_sessions WHERE provider = 'mux'"
                ),
                2,
                "manifested Mux files must reuse one canonical source-scoped parent session"
            );
        }

        let search = json_output(ctx(&temp).args([
            "search",
            query,
            "--provider",
            cli_provider,
            "--refresh",
            "off",
            "--format=json",
        ]));
        assert_search_provider_oracle(&search, stored_provider, query, 1, "message");

        let second = json_output(ctx(&temp).args([
            "import",
            "--provider",
            cli_provider,
            "--format=json",
            "--progress",
            "none",
        ]));
        assert_eq!(second["totals"]["rejected_records"], 0);
        assert_eq!(second["totals"]["imported_events"], 0);
    }
}

#[test]
fn mimocode_default_and_env_sources_import_search_and_reimport() {
    let temp = tempdir();
    let default_query = "mimocode-default-discovery-oracle";
    let default_db = temp
        .path()
        .join(".local")
        .join("share")
        .join("mimocode")
        .join("mimocode.db");
    install_default_mimocode_fixture(&temp, default_query);

    let sources = json_output(ctx(&temp).args(["sources", "--format=json", "--all"]));
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
        "--refresh",
        "wait",
        "--format=json",
    ]));
    let freshness_mode = search["freshness"]["mode"].as_str().unwrap();
    assert_eq!(
        freshness_mode, "wait",
        "unexpected freshness mode in {search:#}"
    );
    assert_eq!(search["freshness"]["status"], "completed");
    assert_eq!(search["freshness"]["totals"]["rejected_records"], 0);
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
            >= 1,
        "expected MiMo refresh events in {search:#}"
    );
    assert_search_provider_oracle(&search, "mimocode", default_query, 1, "message");

    let second = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "mimo_code",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(second["totals"]["rejected_records"], 0);
    assert_eq!(second["totals"]["imported_events"], 0);

    let home_query = "mimocode-home-env-oracle";
    let mimocode_home = temp.path().join("mimocode-home");
    let home_db = mimocode_home.join("data").join("mimocode.db");
    write_mimocode_sqlite_fixture(&home_db, home_query, "mimocode-home");
    let home_sources = json_output(ctx(&temp).env("MIMOCODE_HOME", &mimocode_home).args([
        "sources",
        "--format=json",
        "--all",
    ]));
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
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(home_import["totals"]["rejected_records"], 0);
    assert!(home_import["totals"]["imported_events"].as_u64().unwrap() >= 1);
    let home_search = json_output(ctx(&temp).args([
        "search",
        home_query,
        "--provider",
        "mimocode",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(&home_search, "mimocode", home_query, 1, "message");

    let custom_query = "mimocode-db-env-oracle";
    let custom_db = temp.path().join("custom-mimocode.db");
    write_mimocode_sqlite_fixture(&custom_db, custom_query, "mimocode-custom");
    let custom_import = json_output(ctx(&temp).env("MIMOCODE_DB", &custom_db).args([
        "import",
        "--provider",
        "mimocode",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(
        custom_import["sources"][0]["path"],
        custom_db.display().to_string()
    );
    assert_eq!(custom_import["totals"]["rejected_records"], 0);
    assert!(custom_import["totals"]["imported_events"].as_u64().unwrap() >= 1);

    let xdg_data = temp.path().join("xdg-data");
    let channel_db = xdg_data.join("mimocode").join("mimocode-nightly.db");
    write_mimocode_sqlite_fixture(&channel_db, "mimocode-channel-oracle", "mimocode-channel");
    let channel_sources = json_output(ctx(&temp).env("XDG_DATA_HOME", &xdg_data).args([
        "sources",
        "--format=json",
        "--all",
    ]));
    assert!(
        !has_provider_source_path(&channel_sources, "mimocode", &channel_db),
        "unregistered channel databases must not be discovered: {channel_sources:#}"
    );
    let selected_xdg_db = xdg_data.join("mimocode").join("mimocode.db");
    assert_eq!(
        source_by_path(&channel_sources, "mimocode", &selected_xdg_db)["status"],
        "missing"
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
            .args(["sources", "--format=json", "--all"]),
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

    let sources = json_output(ctx(&temp).args(["sources", "--format=json"]));
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
        "--format=json",
    ]));
    assert_eq!(search["freshness"]["mode"], "wait");
    assert_eq!(search["freshness"]["status"], "completed");
    assert_eq!(search["freshness"]["source_count"], 1);
    assert_eq!(search["freshness"]["totals"]["rejected_records"], 0);
    assert_eq!(search["freshness"]["totals"]["imported_sessions"], 1);
    assert_eq!(search["freshness"]["totals"]["imported_events"], 3);
    assert_search_provider_oracle(&search, "windsurf", query, 1, "message");

    let second = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "windsurf",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(second["totals"]["rejected_records"], 0);
    assert_eq!(second["totals"]["imported_events"], 0);
}

#[test]
fn unknown_native_providers_are_rejected_by_public_cli() {
    let temp = tempdir();

    for provider in ["not-a-real-provider", "unsupported-provider-placeholder"] {
        let stderr =
            failure_stderr(ctx(&temp).args(["import", "--provider", provider, "--format=json"]));
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
            "--format=json",
        ]));
        assert_eq!(first["schema_version"], 2);
        assert_eq!(first["sources"][0]["provider"], stored_provider);
        assert_eq!(first["sources"][0]["source_format"], expected_format);
        assert_eq!(first["totals"]["rejected_records"], 0, "{first:#}");
        assert!(first["totals"]["imported_sessions"].as_u64().unwrap() >= 1);
        assert!(first["totals"]["imported_events"].as_u64().unwrap() >= 1);

        let search = json_output(ctx(&temp).args([
            "search",
            &query,
            "--provider",
            cli_provider,
            "--refresh",
            "off",
            "--format=json",
        ]));
        assert_search_provider_oracle(&search, stored_provider, &query, 1, "message");
    }
}

#[test]
fn authorized_missing_explicit_paths_retire_routes_and_restart_as_noops() {
    for (cli_provider, stored_provider, source_format, fixture) in [
        (
            "factory-ai-droid",
            "factory_ai_droid",
            "factory_ai_droid_sessions_jsonl",
            write_native_factory_droid_fixture as fn(&TempDir, &str) -> String,
        ),
        (
            "lingma",
            "lingma",
            "lingma_sqlite",
            write_native_lingma_fixture,
        ),
        (
            "shelley",
            "shelley",
            "shelley_sqlite",
            write_native_shelley_fixture,
        ),
        (
            "forgecode",
            "forgecode",
            "forgecode_sqlite",
            write_native_forgecode_fixture,
        ),
        (
            "astrbot",
            "astrbot",
            "astrbot_data_v4_sqlite",
            write_native_astrbot_fixture,
        ),
    ] {
        let temp = tempdir();
        let path = PathBuf::from(fixture(
            &temp,
            &format!("{stored_provider}-missing-route-oracle"),
        ));
        let path_text = path.to_str().unwrap();
        let first = json_output(ctx(&temp).args([
            "import",
            "--provider",
            cli_provider,
            "--path",
            path_text,
            "--format=json",
            "--progress",
            "none",
        ]));
        assert_eq!(first["outcome"], "success", "{first:#}");
        assert!(
            provider_route_count(&temp, stored_provider, source_format) > 0,
            "{stored_provider} did not publish route authority"
        );

        if path.is_dir() {
            fs::remove_dir_all(&path).unwrap();
        } else {
            fs::remove_file(&path).unwrap();
        }
        let retired = json_output(ctx(&temp).args([
            "import",
            "--provider",
            cli_provider,
            "--path",
            path_text,
            "--format=json",
            "--progress",
            "none",
        ]));
        assert_eq!(retired["outcome"], "success", "{retired:#}");
        assert_eq!(retired["totals"]["source_files"], 0, "{retired:#}");
        assert_eq!(retired["totals"]["source_bytes"], 0, "{retired:#}");
        assert_eq!(retired["sources"][0]["source_files"], 0, "{retired:#}");
        assert_eq!(
            provider_route_count(&temp, stored_provider, source_format),
            0,
            "{stored_provider} did not retire its route"
        );
        assert_eq!(
            current_provider_locator_count(&temp, stored_provider, source_format),
            0,
            "{stored_provider} retained a current missing locator"
        );

        let restarted = json_output(ctx(&temp).args([
            "import",
            "--provider",
            cli_provider,
            "--path",
            path_text,
            "--format=json",
            "--progress",
            "none",
        ]));
        assert_eq!(restarted["outcome"], "success", "{restarted:#}");
        assert_eq!(restarted["totals"]["change"], "no_op", "{restarted:#}");
        assert_eq!(restarted["totals"]["source_files"], 0, "{restarted:#}");
        assert_eq!(
            provider_route_count(&temp, stored_provider, source_format),
            0,
            "{stored_provider} restart restored a retired route"
        );
    }
}

#[test]
fn missing_explicit_path_authority_is_provider_scoped_and_cold_paths_fail_closed() {
    for (provider, missing_name) in [
        ("factory-ai-droid", "cold-factory-sessions"),
        ("lingma", "cold-lingma.sqlite"),
        ("shelley", "cold-shelley.db"),
        ("forgecode", "COLD-FORGECODE.DB"),
        ("astrbot", "cold-astrbot.sqlite"),
    ] {
        let temp = tempdir();
        let missing = temp.path().join(missing_name);
        let stderr = failure_stderr(ctx(&temp).args([
            "import",
            "--provider",
            provider,
            "--path",
            missing.to_str().unwrap(),
            "--format=json",
            "--progress",
            "none",
        ]));
        assert!(
            stderr.contains("invalid provider transcript path"),
            "{stderr}"
        );
        assert!(
            stderr.contains("no matching prior provider route authority"),
            "{stderr}"
        );
    }

    let temp = tempdir();
    let lingma = PathBuf::from(write_native_lingma_fixture(
        &temp,
        "cross-provider-route-oracle",
    ));
    let lingma_text = lingma.to_str().unwrap();
    json_output(ctx(&temp).args([
        "import",
        "--provider",
        "lingma",
        "--path",
        lingma_text,
        "--format=json",
        "--progress",
        "none",
    ]));
    fs::remove_file(&lingma).unwrap();

    let stderr = failure_stderr(ctx(&temp).args([
        "import",
        "--provider",
        "shelley",
        "--path",
        lingma_text,
        "--format=json",
        "--progress",
        "none",
    ]));
    assert!(
        stderr.contains("invalid provider transcript path"),
        "{stderr}"
    );
    assert!(
        stderr.contains("no matching prior provider route authority"),
        "{stderr}"
    );

    let retired = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "lingma",
        "--path",
        lingma_text,
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(retired["outcome"], "success", "{retired:#}");
}

#[test]
fn forgecode_custom_filenames_receive_zero_stat_missing_path_plans() {
    for custom_name in ["history.sqlite", "history", "HISTORY.DB"] {
        let temp = tempdir();
        let generated = PathBuf::from(write_native_forgecode_fixture(
            &temp,
            "forgecode-custom-missing-route-oracle",
        ));
        let custom = temp.path().join(custom_name);
        fs::rename(generated, &custom).unwrap();
        let custom_text = custom.to_str().unwrap();

        let first = json_output(ctx(&temp).args([
            "import",
            "--provider",
            "forgecode",
            "--path",
            custom_text,
            "--format=json",
            "--progress",
            "none",
        ]));
        assert_eq!(first["outcome"], "success", "{first:#}");
        fs::remove_file(&custom).unwrap();

        let output = ctx(&temp)
            .args([
                "import",
                "--provider",
                "forgecode",
                "--path",
                custom_text,
                "--format=json",
                "--progress",
                "none",
            ])
            .output()
            .unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            !stderr.contains("no matching prior provider route authority"),
            "{custom_name} was rejected before ForgeCode dispatch: {stderr}"
        );
        let packet: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
            panic!("{custom_name} did not produce an import report after dispatch: {stderr}")
        });
        assert_eq!(packet["totals"]["source_files"], 0, "{packet:#}");
        assert_eq!(packet["totals"]["source_bytes"], 0, "{packet:#}");
        assert_eq!(packet["sources"][0]["source_files"], 0, "{packet:#}");
    }
}

fn provider_route_count(temp: &TempDir, provider: &str, source_format: &str) -> i64 {
    let conn = Connection::open(temp.path().join("work.sqlite")).unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM capture_source_provider_routes
         WHERE provider = ?1 AND source_format = ?2",
        params![provider, source_format],
        |row| row.get(0),
    )
    .unwrap()
}

fn current_provider_locator_count(temp: &TempDir, provider: &str, source_format: &str) -> i64 {
    let conn = Connection::open(temp.path().join("work.sqlite")).unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM provider_source_locators
         WHERE provider = ?1 AND source_format = ?2 AND is_current = 1",
        params![provider, source_format],
        |row| row.get(0),
    )
    .unwrap()
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
            "--format=json",
            "--progress",
            "none",
        ]));
        assert_eq!(imported["totals"]["rejected_records"], 0, "{imported:#}");

        let search = json_output(ctx(&temp).args([
            "search",
            query,
            "--provider",
            provider,
            "--refresh",
            "off",
            "--format=json",
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
            "--format=json",
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
            "--format=json",
        ]));
        assert_eq!(first["totals"]["rejected_records"], 0);
        assert!(first["totals"]["imported_events"].as_u64().unwrap() >= 1);

        let second = json_output(ctx(&temp).args([
            "import",
            "--provider",
            cli_provider,
            "--path",
            &path,
            "--format=json",
        ]));
        assert_eq!(second["totals"]["rejected_records"], 0);
        assert_eq!(second["totals"]["imported_events"], 0);

        append_event(&path, &incremental_query);
        let third = json_output(ctx(&temp).args([
            "import",
            "--provider",
            cli_provider,
            "--path",
            &path,
            "--format=json",
        ]));
        assert_eq!(third["totals"]["rejected_records"], 0);
        assert!(third["totals"]["imported_events"].as_u64().unwrap() >= 1);

        let search = json_output(ctx(&temp).args([
            "search",
            &incremental_query,
            "--provider",
            cli_provider,
            "--format=json",
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
        "--format=json",
    ]));
    assert_eq!(imported["totals"]["rejected_records"], 0);
    assert_eq!(imported["totals"]["imported_sources"], 1);

    let search =
        json_output(ctx(&temp).args(["search", query, "--provider", "openclaw", "--format=json"]));
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
        "--format=json",
    ]));
    assert_eq!(imported["totals"]["rejected_records"], 0);
    assert_eq!(imported["totals"]["imported_sources"], 1);

    let search =
        json_output(ctx(&temp).args(["search", query, "--provider", "nanoclaw", "--format=json"]));
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
                "--format=json",
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
            "--format=json",
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
        let stderr = failure_stderr(ctx(&temp).current_dir(temp.path()).args([
            "import",
            "--provider",
            cli_provider,
            "--format=json",
        ]));

        assert!(stderr.contains(expected_blocker), "{stderr}");
        assert!(stderr.contains("use `ctx sources`"), "{stderr}");
        if matches!(cli_provider, "nanoclaw" | "openclaw" | "lingma") {
            assert!(
                stderr.contains("no default paths are registered for this provider"),
                "{stderr}"
            );
        } else if cli_provider == "factory-ai-droid" {
            assert!(
                stderr.contains("no official automatic history location is established"),
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

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "cline",
        "--path",
        &cline,
        "--format=json",
    ]));
    assert_eq!(imported["schema_version"], 2);
    assert_eq!(imported["sources"][0]["provider"], "cline");
    assert_eq!(
        imported["sources"][0]["source_format"],
        "cline_task_directory_json"
    );
    assert_eq!(imported["totals"]["imported_sessions"], 1);
    assert_eq!(imported["totals"]["imported_events"], 4);
    assert_eq!(imported["totals"]["rejected_records"], 0);

    let second = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "cline",
        "--path",
        &cline,
        "--format=json",
    ]));
    assert_eq!(second["totals"]["imported_sessions"], 0);
    assert_eq!(second["totals"]["imported_events"], 0);
    assert_eq!(second["totals"]["skipped_events"], 0);

    let search = json_output(ctx(&temp).args([
        "search",
        "parser note",
        "--provider",
        "cline",
        "--format=json",
    ]));
    assert_search_provider_oracle(&search, "cline", "parser note", 1, "message");

    let roo = provider_history_fixture("roo/storage");
    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "roo-code",
        "--path",
        &roo,
        "--format=json",
    ]));
    assert_eq!(imported["schema_version"], 2);
    assert_eq!(imported["sources"][0]["provider"], "roo_code");
    assert_eq!(
        imported["sources"][0]["source_format"],
        "roo_task_directory_json"
    );
    assert_eq!(imported["totals"]["imported_sessions"], 2);
    assert_eq!(imported["totals"]["imported_events"], 6);
    assert_eq!(imported["totals"]["rejected_records"], 0);

    let search = json_output(ctx(&temp).args([
        "search",
        "fallback claude_messages",
        "--provider",
        "roo",
        "--format=json",
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
        "--format=json",
    ]));
    assert_eq!(imported["schema_version"], 2);
    assert_eq!(imported["sources"][0]["provider"], "antigravity");
    assert_eq!(
        imported["sources"][0]["source_format"],
        "antigravity_cli_transcript_jsonl_tree"
    );
    assert_eq!(imported["totals"]["imported_sessions"], 4);
    assert_eq!(imported["totals"]["imported_events"], 11);
    assert_eq!(imported["totals"]["rejected_records"], 1);

    let conn = Connection::open(temp.path().join("work.sqlite")).unwrap();
    assert_eq!(
        sqlite_count(
            &conn,
            "SELECT COUNT(*) FROM ctx_sessions \
             WHERE provider = 'antigravity' AND provider_session_id = 'agy-future'",
        ),
        1,
        "the future-shape transcript must retain its notice-only session"
    );
    assert_eq!(
        sqlite_count(
            &conn,
            "SELECT COUNT(*) FROM ctx_events \
             WHERE provider = 'antigravity' AND provider_session_id = 'agy-future' \
             AND event_type = 'notice'",
        ),
        2,
        "both future-shape records must survive as notices"
    );

    let search = json_output(ctx(&temp).args([
        "search",
        "write_to_file",
        "--provider",
        "antigravity",
        "--format=json",
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
        "--format=json",
    ]));
    assert_eq!(
        imported["totals"]["source_files"],
        2,
        "outer inventory reports the authoritative root; the provider owns sibling preference: {imported:#}"
    );
    assert_eq!(imported["totals"]["rejected_records"], 0, "{imported:#}");
    assert_eq!(imported["totals"]["imported_sessions"], 1, "{imported:#}");
}

#[test]
fn codex_cli_reports_rejected_records_and_imports_valid_content() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-malformed-session.jsonl");

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--format=json",
    ]));
    assert_eq!(imported["schema_version"], 2);
    assert_eq!(imported["totals"]["imported_sessions"], 1);
    assert_eq!(imported["totals"]["imported_events"], 2);
    assert_eq!(imported["totals"]["rejected_records"], 1);
    assert_eq!(imported["sources"][0]["rejected_records"], 1);

    let search = json_output(ctx(&temp).args(["search", "after malformed", "--format=json"]));
    assert!(!search["results"].as_array().unwrap().is_empty());
}

#[test]
fn pi_cli_reports_malformed_and_schema_rejections() {
    let temp = tempdir();
    let fixture = provider_history_fixture("pi-malformed-mixed.jsonl");

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "pi",
        "--path",
        &fixture,
        "--format=json",
    ]));
    assert_eq!(imported["schema_version"], 2);
    assert_eq!(imported["totals"]["imported_sessions"], 1);
    assert_eq!(imported["totals"]["imported_events"], 2);
    assert_eq!(imported["totals"]["rejected_records"], 2);
    assert_eq!(imported["sources"][0]["rejected_records"], 2);
    assert_eq!(
        imported["sources"][0]["rejections"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let query = "after malformed line";
    let search =
        json_output(ctx(&temp).args(["search", query, "--provider", "pi", "--format=json"]));
    assert_search_provider_oracle(&search, "pi", query, 1, "message");
}

#[test]
fn import_all_isolates_rejected_records_and_imports_other_sources() {
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
        json_output(ctx(&temp).args(["import", "--all", "--format=json", "--progress", "none"]));
    assert_eq!(imported["totals"]["failed_sources"], 0, "{imported:#}");
    assert!(imported["sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|source| {
            source["provider"] == "codex" && source["status"] == "completed_with_rejections"
        }));
    assert!(imported["sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|source| { source["provider"] == "pi" && source["status"] == "success" }));

    let pi_search = json_output(ctx(&temp).args([
        "search",
        pi_query,
        "--provider",
        "pi",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(&pi_search, "pi", pi_query, 1, "message");
    let codex_search = json_output(ctx(&temp).args([
        "search",
        "after malformed",
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert!(!codex_search["results"].as_array().unwrap().is_empty());
}
