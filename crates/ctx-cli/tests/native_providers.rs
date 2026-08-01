mod support;

use support::{daemon_test_root as tempdir, *};

#[path = "support/native_providers/daemon.rs"]
mod provider_daemon;
#[path = "support/native_providers/sqlite_sources.rs"]
mod sqlite_sources;
#[path = "support/native_providers/workspace_sources.rs"]
mod workspace_sources;

use provider_daemon::*;

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
    let _daemon = start_isolated_provider_daemon(&temp);

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
            5,
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
            "--no-daemon",
            "--format=json",
            "--progress",
            "none",
        ]));
        assert_authoritative_provider_publication(&first);
        wait_for_imported_projections(&temp, &first);
        assert_eq!(first["totals"]["current_rejected_records"], 1, "{first:#}");
        assert!(
            source_backed_count(
                &temp,
                &format!("SELECT COUNT(*) FROM ctx_events WHERE provider = '{stored_provider}'")
            ) >= minimum_events,
            "expected at least {minimum_events} indexed {stored_provider} events"
        );
        if stored_provider == "mux" {
            assert_eq!(
                source_backed_count(
                    &temp,
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
        assert_source_backed_search(&search, stored_provider, query);

        let second = json_output(ctx(&temp).args([
            "import",
            "--provider",
            cli_provider,
            "--no-daemon",
            "--format=json",
            "--progress",
            "none",
        ]));
        assert_authoritative_provider_publication(&second);
        assert_eq!(
            second["totals"]["current_rejected_records"], 1,
            "{second:#}"
        );
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
    assert!(
        search["retrieval"]["indexed_documents"]
            .as_u64()
            .is_some_and(|count| count >= 1),
        "expected MiMo documents in {search:#}"
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
    assert_authoritative_provider_publication(&second);
    assert_eq!(
        second["totals"]["current_rejected_records"], 0,
        "{second:#}"
    );

    drop(temp);
    let temp = tempdir();
    let default_db = temp.path().join(".local/share/mimocode/mimocode.db");
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
    assert_authoritative_provider_publication(&home_import);
    assert_eq!(home_import["totals"]["current_rejected_records"], 0);
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

    drop(temp);
    let temp = tempdir();
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
    assert_authoritative_provider_publication(&custom_import);
    assert_eq!(custom_import["totals"]["current_rejected_records"], 0);
    let custom_search = json_output(ctx(&temp).args([
        "search",
        custom_query,
        "--provider",
        "mimocode",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(&custom_search, "mimocode", custom_query, 1, "message");

    drop(temp);
    let temp = tempdir();
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
    assert!(
        search["retrieval"]["indexed_documents"]
            .as_u64()
            .is_some_and(|count| count >= 1),
        "{search:#}"
    );
    assert_search_provider_oracle(&search, "windsurf", query, 1, "message");

    let second = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "windsurf",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_authoritative_provider_publication(&second);
    assert_eq!(
        second["totals"]["current_rejected_records"], 0,
        "{second:#}"
    );
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
        let _daemon = start_isolated_provider_daemon(&temp);

        let first = json_output(ctx(&temp).args([
            "import",
            "--provider",
            cli_provider,
            "--path",
            &path,
            "--no-daemon",
            "--format=json",
        ]));
        assert_explicit_source_publication(&first, stored_provider, expected_format);
        wait_for_imported_projections(&temp, &first);
        let expected_rejected_records = u64::from(stored_provider == "qwen_code");
        assert_eq!(
            first["totals"]["current_rejected_records"], expected_rejected_records,
            "{first:#}"
        );
        assert!(
            source_backed_count(
                &temp,
                &format!("SELECT COUNT(*) FROM ctx_sessions WHERE provider = '{stored_provider}'")
            ) >= 1
        );
        assert!(
            source_backed_count(
                &temp,
                &format!("SELECT COUNT(*) FROM ctx_events WHERE provider = '{stored_provider}'")
            ) >= 1
        );

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
fn discovery_only_sqlite_explicit_paths_are_rejected_without_fallback() {
    for (provider, fixture, reason) in [
        (
            "astrbot",
            write_native_astrbot_fixture as fn(&TempDir, &str) -> String,
            "requires provider discovery authority",
        ),
        (
            "shelley",
            write_native_shelley_fixture,
            "has no explicit source-backed adapter",
        ),
    ] {
        let temp = tempdir();
        let path = fixture(&temp, &format!("{provider}-explicit-unsupported-oracle"));
        let stderr = failure_stderr(ctx(&temp).args([
            "import",
            "--provider",
            provider,
            "--path",
            &path,
            "--no-daemon",
            "--format=json",
        ]));
        assert!(
            stderr.contains(reason) && stderr.contains("no legacy import fallback was used"),
            "{provider}: {stderr}"
        );
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
fn native_provider_cli_preserves_complete_tool_outputs_without_legacy_payloads() {
    for (provider, source_format, fixture, query, sentinel, output_is_searchable) in [
        (
            "qoder",
            "qoder_transcript_jsonl_tree",
            write_native_qoder_fixture as fn(&TempDir, &str) -> String,
            "qoder-policy-real-message-oracle",
            "qoderleakproofxylophonium",
            true,
        ),
        (
            "openhands",
            "openhands_file_events",
            write_native_openhands_fixture,
            "openhands-policy-real-message-oracle",
            "openhandssuccesstooloutputsentinel",
            true,
        ),
        (
            "continue",
            "continue_cli_sessions_json",
            write_native_continue_fixture,
            "continue-policy-real-message-oracle",
            "continuesuccesstooloutputsentinel",
            false,
        ),
    ] {
        let temp = tempdir();
        let path = fixture(&temp, query);
        let imported = json_output(ctx(&temp).args([
            "import",
            "--provider",
            provider,
            "--path",
            &path,
            "--format=json",
            "--progress",
            "none",
        ]));
        assert_explicit_source_publication(&imported, provider, source_format);
        wait_for_imported_projections(&temp, &imported);
        assert_eq!(
            imported["totals"]["current_rejected_records"], 0,
            "{imported:#}"
        );

        let search = json_output(ctx(&temp).args([
            "search",
            query,
            "--provider",
            provider,
            "--refresh",
            "off",
            "--format=json",
        ]));
        assert_source_backed_search(&search, provider, query);

        let search = json_output(ctx(&temp).args([
            "search",
            sentinel,
            "--provider",
            provider,
            "--refresh",
            "off",
            "--format=json",
        ]));
        if output_is_searchable {
            assert_source_backed_search(&search, provider, sentinel);
        } else {
            assert!(
                search["results"].as_array().unwrap().is_empty(),
                "{provider} must not invent a Core event for nested result context: {search:#}"
            );
        }

        assert_eq!(
            source_backed_count(
                &temp,
                "SELECT COUNT(*) FROM pragma_table_info('ctx_events') WHERE name = 'payload_json'",
            ),
            0,
            "the metadata-only relational projection must expose no payload column"
        );
        assert_eq!(
            source_backed_count(&temp, "SELECT COUNT(*) FROM ctx_files_touched"),
            0,
            "unscoped file paths must not cross the Core repository boundary"
        );
    }
}

#[test]
fn personal_agent_provider_imports_are_idempotent_and_incremental() {
    for (cli_provider, stored_provider, source_format, fixture, append_event) in [
        (
            "openclaw",
            "openclaw",
            "openclaw_session_jsonl_tree",
            write_native_openclaw_fixture as fn(&TempDir, &str) -> String,
            append_native_openclaw_event as fn(&str, &str),
        ),
        (
            "hermes",
            "hermes",
            "hermes_state_sqlite",
            write_native_hermes_fixture,
            append_native_hermes_event,
        ),
        (
            "nanoclaw",
            "nanoclaw",
            "nanoclaw_project",
            write_native_nanoclaw_fixture,
            append_native_nanoclaw_event,
        ),
        (
            "astrbot",
            "astrbot",
            "astrbot_data_v4_sqlite",
            write_native_astrbot_fixture,
            append_native_astrbot_event,
        ),
        (
            "shelley",
            "shelley",
            "shelley_sqlite",
            write_native_shelley_fixture,
            append_native_shelley_event,
        ),
    ] {
        let temp = tempdir();
        let initial_query = format!("{stored_provider}-incremental-initial-oracle");
        let incremental_query = format!("{stored_provider}-incremental-next-oracle");
        let fixture_path = fixture(&temp, &initial_query);
        let (path, automatic, exact_cwd) = match stored_provider {
            "hermes" => {
                let path = temp.path().join(".hermes/state.db");
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::copy(&fixture_path, &path).unwrap();
                (path.display().to_string(), true, false)
            }
            "astrbot" => {
                fs::write(temp.path().join(".astrbot"), b"cli marker").unwrap();
                let path = temp.path().join("data/data_v4.db");
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::copy(&fixture_path, &path).unwrap();
                (path.display().to_string(), true, true)
            }
            "shelley" => {
                let path = temp.path().join("shelley.db");
                fs::copy(&fixture_path, &path).unwrap();
                (path.display().to_string(), true, true)
            }
            _ => (fixture_path, false, false),
        };
        let _daemon = start_isolated_provider_daemon(&temp);

        let mut first_command = ctx(&temp);
        first_command.args(["import", "--provider", cli_provider]);
        if !automatic {
            first_command.args(["--path", &path]);
        }
        if exact_cwd {
            first_command.current_dir(temp.path());
        }
        let first = json_output(first_command.args(["--no-daemon", "--format=json"]));
        if automatic {
            assert_authoritative_provider_publication(&first);
            assert_eq!(first["totals"]["current_rejected_records"], 0);
        } else {
            assert_explicit_source_publication(&first, stored_provider, source_format);
            assert_eq!(first["totals"]["current_rejected_records"], 0);
        }
        wait_for_imported_projections(&temp, &first);
        let initial_event_count = source_backed_count(
            &temp,
            &format!("SELECT COUNT(*) FROM ctx_events WHERE provider = '{stored_provider}'"),
        );
        assert!(initial_event_count >= 1, "{stored_provider}: {first:#}");

        let mut second_command = ctx(&temp);
        second_command.args(["import", "--provider", cli_provider]);
        if !automatic {
            second_command.args(["--path", &path]);
        }
        if exact_cwd {
            second_command.current_dir(temp.path());
        }
        let second = json_output(second_command.args(["--no-daemon", "--format=json"]));
        if automatic {
            assert_authoritative_provider_publication(&second);
            assert_eq!(second["totals"]["current_rejected_records"], 0);
        } else {
            assert_explicit_source_publication(&second, stored_provider, source_format);
            assert_eq!(second["totals"]["current_rejected_records"], 0);
        }
        assert_noop_publication(&second);

        append_event(&path, &incremental_query);
        let mut third_command = ctx(&temp);
        third_command.args(["import", "--provider", cli_provider]);
        if !automatic {
            third_command.args(["--path", &path]);
        }
        if exact_cwd {
            third_command.current_dir(temp.path());
        }
        let third = json_output(third_command.args(["--no-daemon", "--format=json"]));
        if automatic {
            assert_authoritative_provider_publication(&third);
            assert_eq!(third["totals"]["current_rejected_records"], 0);
        } else {
            assert_explicit_source_publication(&third, stored_provider, source_format);
            assert_eq!(third["totals"]["current_rejected_records"], 0);
        }
        wait_for_imported_projections(&temp, &third);
        assert!(
            source_backed_count(
                &temp,
                &format!("SELECT COUNT(*) FROM ctx_events WHERE provider = '{stored_provider}'")
            ) > initial_event_count,
            "{third:#}"
        );

        let mut search_command = ctx(&temp);
        if exact_cwd {
            search_command.current_dir(temp.path());
        }
        let search = json_output(search_command.args([
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
    assert_explicit_source_publication(&imported, "openclaw", "openclaw_session_jsonl_tree");
    assert_eq!(imported["totals"]["current_rejected_records"], 0);

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
    assert_explicit_source_publication(&imported, "nanoclaw", "nanoclaw_project");
    assert_eq!(imported["totals"]["current_rejected_records"], 0);

    let search =
        json_output(ctx(&temp).args(["search", query, "--provider", "nanoclaw", "--format=json"]));
    assert_search_provider_oracle(&search, "nanoclaw", query, 1, "message");
}

#[test]
fn personal_agent_sqlite_imports_report_corrupt_databases() {
    for (provider, path) in [
        ("hermes", "corrupt-hermes-state.db"),
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
    let db_path = temp.path().join(".astrbot/data/data_v4.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    fs::write(&db_path, b"not sqlite").unwrap();
    let stderr =
        failure_stderr(ctx(&temp).args(["import", "--provider", "astrbot", "--format=json"]));
    assert!(stderr.contains("not a database"), "{stderr}");

    let temp = tempdir();
    fs::write(temp.path().join("shelley.db"), b"not sqlite").unwrap();
    let stderr = failure_stderr(ctx(&temp).current_dir(temp.path()).args([
        "import",
        "--provider",
        "shelley",
        "--format=json",
    ]));
    assert!(stderr.contains("not a database"), "{stderr}");

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
    let temp = tempdir();
    let _daemon = start_isolated_provider_daemon(&temp);
    for cli_provider in [
        "claude",
        "opencode",
        "kilo",
        "antigravity",
        "gemini",
        "cursor",
        "zed",
        "copilot-cli",
        "factory-ai-droid",
        "openclaw",
        "hermes",
        "nanoclaw",
        "astrbot",
        "shelley",
        "lingma",
        "codebuddy",
        "auggie",
        "deepagents",
        "mistral-vibe",
        "mux",
        "cline",
        "roo",
    ] {
        let stderr = failure_stderr(ctx(&temp).current_dir(temp.path()).args([
            "import",
            "--provider",
            cli_provider,
            "--no-daemon",
            "--format=json",
        ]));

        assert!(
            stderr.contains("--path <path>")
                && (stderr.contains("no importable ")
                    || stderr.contains("no official automatic history location established")),
            "{cli_provider}: {stderr}"
        );
    }
}

#[test]
fn task_json_cli_imports_cline_and_roo_and_searches() {
    let temp = tempdir();
    let cline_fixture = PathBuf::from(provider_history_fixture("cline/data"));
    let cline = temp.path().join(".cline/data");
    copy_dir_all(&cline_fixture, &cline);
    let _daemon = start_isolated_provider_daemon(&temp);
    let cline = cline.display().to_string();

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "cline",
        "--path",
        &cline,
        "--no-daemon",
        "--format=json",
    ]));
    assert_explicit_source_publication(&imported, "cline", "cline_task_directory_json");
    wait_for_imported_projections(&temp, &imported);
    assert_eq!(imported["totals"]["current_rejected_records"], 0);
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_sessions WHERE provider = 'cline'"
        ),
        1
    );
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_events WHERE provider = 'cline'"
        ),
        5
    );

    let second = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "cline",
        "--path",
        &cline,
        "--no-daemon",
        "--format=json",
    ]));
    assert_explicit_source_publication(&second, "cline", "cline_task_directory_json");
    assert_eq!(second["sources"][0]["catalog_changed"], false, "{second:#}");

    let search = json_output(ctx(&temp).args([
        "search",
        "parser note",
        "--provider",
        "cline",
        "--refresh",
        "off",
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
        "--no-daemon",
        "--format=json",
    ]));
    assert_explicit_source_publication(&imported, "roo_code", "roo_task_directory_json");
    wait_for_imported_projections(&temp, &imported);
    assert_eq!(imported["totals"]["current_rejected_records"], 0);
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_sessions WHERE provider = 'roo_code'"
        ),
        2
    );
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_events WHERE provider = 'roo_code'"
        ),
        7
    );

    let search = json_output(ctx(&temp).args([
        "search",
        "fallback claude_messages",
        "--provider",
        "roo",
        "--refresh",
        "off",
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
    let source_fixture = PathBuf::from(provider_history_fixture("antigravity/v1/brain"));
    let fixture = temp.path().join("antigravity-brain");
    for session in ["agy-future", "agy-resume", "agy-success"] {
        copy_dir_all(&source_fixture.join(session), &fixture.join(session));
    }
    let _daemon = start_isolated_provider_daemon(&temp);

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "antigravity",
        "--path",
        fixture.to_str().unwrap(),
        "--no-daemon",
        "--format=json",
    ]));
    assert_explicit_source_publication(
        &imported,
        "antigravity",
        "antigravity_cli_transcript_jsonl_tree",
    );
    wait_for_imported_projections(&temp, &imported);
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_sessions WHERE provider = 'antigravity'"
        ),
        3
    );
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_events WHERE provider = 'antigravity'"
        ),
        8
    );

    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_sessions \
             WHERE provider = 'antigravity' AND provider_session_id = 'agy-future'",
        ),
        1,
        "the future-shape transcript must retain its notice-only session"
    );
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_events \
             WHERE provider = 'antigravity' AND provider_session_id = 'agy-future' \
             AND event_type = 'notice'",
        ),
        2,
        "both future-shape records must survive as notices"
    );

    let search = json_output(ctx(&temp).args([
        "search",
        "agycardinalityxylophonium",
        "--provider",
        "antigravity",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_source_backed_search(&search, "antigravity", "agycardinalityxylophonium");
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
    assert_explicit_source_publication(
        &imported,
        "antigravity",
        "antigravity_cli_transcript_jsonl_tree",
    );
    wait_for_imported_projections(&temp, &imported);
    assert_eq!(
        imported["sources"][0]["source_files"],
        2,
        "outer inventory reports the authoritative root; the provider owns sibling preference: {imported:#}"
    );
    assert_eq!(
        imported["totals"]["current_rejected_records"], 0,
        "{imported:#}"
    );
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_sessions WHERE provider = 'antigravity'"
        ),
        1,
        "{imported:#}"
    );
}

#[test]
fn codex_cli_catalogs_valid_content_from_mixed_fixture() {
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
    assert_explicit_source_publication(&imported, "codex", "codex_session_jsonl");
    wait_for_imported_projections(&temp, &imported);
    assert_eq!(
        imported["totals"]["current_rejected_records"], 1,
        "{imported:#}"
    );
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_sessions WHERE provider = 'codex'"
        ),
        1
    );
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_events WHERE provider = 'codex'"
        ),
        2
    );

    let search = json_output(ctx(&temp).args(["search", "after malformed", "--format=json"]));
    assert!(!search["results"].as_array().unwrap().is_empty());
}

#[test]
fn pi_cli_catalogs_valid_content_from_mixed_fixture() {
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
    assert_explicit_source_publication(&imported, "pi", "pi_session_jsonl");
    wait_for_imported_projections(&temp, &imported);
    assert_eq!(
        imported["totals"]["current_rejected_records"], 2,
        "{imported:#}"
    );
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_sessions WHERE provider = 'pi'"
        ),
        1
    );
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_events WHERE provider = 'pi'"
        ),
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
    assert_authoritative_provider_publication(&imported);
    assert_eq!(imported["totals"]["current_rejected_records"], 1);
    assert_eq!(imported["totals"]["current_sources_with_rejections"], 1);
    assert_eq!(imported["totals"]["current_source_count"], 2);

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
