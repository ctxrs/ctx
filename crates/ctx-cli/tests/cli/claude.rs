#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn search_refresh_auto_imports_discovered_top_provider_sources() {
    for (cli_provider, stored_provider, install_fixture) in [
        (
            "claude",
            "claude",
            install_default_claude_fixture as fn(&TempDir, &str),
        ),
        ("pi", "pi", install_default_pi_fixture),
        ("cursor", "cursor", install_default_cursor_fixture),
        ("openclaw", "openclaw", install_default_openclaw_fixture),
        ("hermes", "hermes", install_default_hermes_fixture),
        ("kilo", "kilo", install_default_kilo_fixture),
        ("astrbot", "astrbot", install_default_astrbot_fixture),
        ("shelley", "shelley", install_default_shelley_fixture),
        ("continue", "continue", install_default_continue_fixture),
        ("openhands", "openhands", install_default_openhands_fixture),
        ("rovodev", "rovodev", install_default_rovodev_fixture),
        ("lingma", "lingma", install_default_lingma_fixture),
        ("qoder", "qoder", install_default_qoder_fixture),
        ("junie", "junie", install_default_junie_fixture),
    ] {
        let temp = tempdir();
        let query = format!("{stored_provider}-default-refresh-oracle");
        install_fixture(&temp, &query);

        let search =
            json_output(ctx(&temp).args(["search", &query, "--provider", cli_provider, "--json"]));
        assert_eq!(search["freshness"]["mode"], "auto");
        assert_eq!(search["freshness"]["status"], "completed");
        assert_eq!(search["freshness"]["source_count"], 1);
        assert!(
            search["freshness"]["totals"]["imported_sessions"]
                .as_u64()
                .unwrap()
                >= 1
        );
        assert_search_provider_oracle(&search, stored_provider, &query, 1, "message");

        let started = Instant::now();
        let refreshed =
            json_output(ctx(&temp).args(["search", &query, "--provider", cli_provider, "--json"]));
        assert_eq!(refreshed["freshness"]["mode"], "auto");
        assert_eq!(refreshed["freshness"]["status"], "completed");
        assert_eq!(refreshed["freshness"]["totals"]["imported_events"], 0);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "second refresh should stay incremental for {cli_provider}"
        );
    }
}

#[test]
pub(crate) fn native_provider_cli_flow_imports_supported_provider_paths() {
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

pub(crate) fn install_default_claude_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_claude_fixture(temp, query));
    copy_dir_all(&source, &temp.path().join(".claude").join("projects"));
}

pub(crate) fn write_native_claude_fixture(temp: &TempDir, query: &str) -> String {
    let root = temp.path().join("native-claude/projects/-workspace");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("claude-cli-native.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "sessionId": "claude-cli-native",
                "timestamp": "2026-06-24T12:00:00Z",
                "cwd": "/workspace",
                "version": "test",
                "type": "user",
                "message": {"role": "user", "content": [{"type": "text", "text": query}]},
                "uuid": "claude-cli-native-user"
            }),
            json!({
                "sessionId": "claude-cli-native",
                "timestamp": "2026-06-24T12:00:01Z",
                "cwd": "/workspace",
                "version": "test",
                "type": "assistant",
                "message": {"role": "assistant", "content": [{"type": "text", "text": "native import ok"}]},
                "uuid": "claude-cli-native-assistant"
            })
        ),
    )
    .unwrap();
    temp.path()
        .join("native-claude/projects")
        .to_str()
        .unwrap()
        .to_owned()
}

#[test]
pub(crate) fn native_provider_cli_requires_existing_history_or_explicit_path() {
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
pub(crate) fn skill_install_auto_targets_universal_and_detected_claude_code() {
    let temp = tempdir();
    fs::create_dir_all(temp.path().join(".claude")).unwrap();

    let install = json_output(
        ctx(&temp)
            .env("CODEX_HOME", temp.path().join("missing-codex"))
            .args(["skill", "install", "--json"]),
    );
    assert_eq!(install["results"].as_array().unwrap().len(), 2);
    assert_eq!(install["results"][0]["agent"], "universal");
    assert_eq!(install["results"][1]["agent"], "claude-code");
    assert_eq!(install["results"][0]["status"], "current");
    assert_eq!(install["results"][1]["status"], "current");

    assert!(temp
        .path()
        .join(".agents")
        .join("skills")
        .join("ctx-agent-history-search")
        .join("SKILL.md")
        .exists());
    assert!(temp
        .path()
        .join(".claude")
        .join("skills")
        .join("ctx-agent-history-search")
        .join("SKILL.md")
        .exists());
}

#[test]
pub(crate) fn skill_install_agent_paths_respect_env_xdg_and_project_scope() {
    let temp = tempdir();
    let home = temp.path();
    let xdg = temp.path().join("xdg-config");
    let codex_home = temp.path().join("custom-codex");
    let claude_home = temp.path().join("custom-claude");

    let global = json_output(
        ctx(&temp)
            .env("XDG_CONFIG_HOME", &xdg)
            .env("CODEX_HOME", &codex_home)
            .env("CLAUDE_CONFIG_DIR", &claude_home)
            .args([
                "skill",
                "install",
                "--agent",
                "codex",
                "--agent",
                "claude-code",
                "--agent",
                "opencode",
                "--json",
            ]),
    );
    assert_eq!(global["results"].as_array().unwrap().len(), 3);
    assert!(codex_home
        .join("skills")
        .join("ctx-agent-history-search")
        .join("SKILL.md")
        .exists());
    assert!(claude_home
        .join("skills")
        .join("ctx-agent-history-search")
        .join("SKILL.md")
        .exists());
    assert!(xdg
        .join("opencode")
        .join("skills")
        .join("ctx-agent-history-search")
        .join("SKILL.md")
        .exists());

    let project = temp.path().join("project");
    fs::create_dir_all(&project).unwrap();
    let mut command = ctx(&temp);
    command.current_dir(&project).args([
        "skill",
        "install",
        "--project",
        "--agent",
        "codex",
        "--agent",
        "claude-code",
        "--json",
    ]);
    let project_output = json_output(&mut command);
    assert_eq!(project_output["scope"], "project");
    assert!(project
        .join(".agents")
        .join("skills")
        .join("ctx-agent-history-search")
        .join("SKILL.md")
        .exists());
    assert!(project
        .join(".claude")
        .join("skills")
        .join("ctx-agent-history-search")
        .join("SKILL.md")
        .exists());
    assert!(!home
        .join(".codex")
        .join("skills")
        .join("ctx-agent-history-search")
        .exists());
}
