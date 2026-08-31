mod support;

use support::*;

#[test]
fn bootstrap_colors_clap_help_and_parse_errors_before_dispatch() {
    let temp = tempdir();
    let always_help = ctx(&temp)
        .args(["--color", "always", "--help"])
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .output()
        .unwrap();
    assert!(always_help.status.success());
    assert!(always_help.stdout.contains(&0x1b));

    let never_help = ctx(&temp)
        .args(["--color=never", "--help"])
        .output()
        .unwrap();
    assert!(never_help.status.success());
    assert!(!never_help.stdout.contains(&0x1b));

    let always_error = ctx(&temp)
        .args(["--color=always", "--not-a-real-option"])
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .output()
        .unwrap();
    assert!(!always_error.status.success());
    assert!(always_error.stderr.contains(&0x1b));

    let auto_pipe = ctx(&temp)
        .args(["--color=auto", "--help"])
        .env_remove("CLICOLOR")
        .env_remove("CLICOLOR_FORCE")
        .env_remove("NO_COLOR")
        .output()
        .unwrap();
    assert!(auto_pipe.status.success());
    assert!(!auto_pipe.stdout.contains(&0x1b));

    for args in [
        &[
            "--color=always",
            "show",
            "event",
            "bad",
            "--format=jsonl",
            "--not-a-real-option",
        ][..],
        &[
            "--color=always",
            "show",
            "session",
            "bad",
            "--format=json",
            "--not-a-real-option",
        ][..],
        &[
            "--color=always",
            "setup",
            "--progress=json",
            "--not-a-real-option",
        ][..],
        &["--color=always", "mcp", "serve", "--not-a-real-option"][..],
    ] {
        let machine_error = ctx(&temp).args(args).output().unwrap();
        assert!(!machine_error.status.success(), "{args:?}");
        assert!(!machine_error.stderr.contains(&0x1b), "{args:?}");
    }
}

#[test]
fn sources_mutations_accept_global_json_format_after_the_subcommand() {
    let temp = tempdir();
    let provider_home = temp.path().join("claude-personal");
    fs::create_dir_all(&provider_home).unwrap();

    let output = ctx(&temp)
        .args([
            "sources",
            "add",
            "personal",
            "--provider",
            "claude",
            "--root",
            provider_home.to_str().unwrap(),
            "--format",
            "json",
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "{:?}", output.stderr);
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["operation"], "add");
    assert_eq!(value["root"]["name"], "personal");
}

#[test]
fn release_cli_does_not_expose_or_invoke_the_index_dashboard_fixture() {
    const FIXTURE_COMMAND: &str = "_index-dashboard-renderer-fixture";

    let temp = tempdir();
    ctx(&temp)
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains(FIXTURE_COMMAND).not());

    ctx(&temp)
        .args([
            FIXTURE_COMMAND,
            "--case",
            "ready",
            "--columns",
            "80",
            "--rows",
            "24",
            "--clock",
            "2026-06-23T12:00:00Z",
            "--random-seed",
            "ctx-cli-ux-core-v1",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unrecognized subcommand"))
        .stderr(predicate::str::contains("requires stdout to be a terminal").not());
}

#[test]
fn help_exposes_session_retrieval_commands() {
    let temp = tempdir();
    let output = ctx(&temp)
        .arg("--help")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let help = String::from_utf8(output).unwrap();
    let commands = help
        .split("Commands:")
        .nth(1)
        .and_then(|tail| tail.split("Options:").next())
        .unwrap_or(&help);

    for expected in [
        "pro",
        "blame",
        "referral",
        "setup",
        "semantic",
        "status",
        "stats",
        "index",
        "sources",
        "import",
        "show",
        "list",
        "locate",
        "search",
        "docs",
        "mcp",
        "integrations",
        "daemon",
        "upgrade",
        "doctor",
    ] {
        assert!(
            commands.contains(expected),
            "missing command {expected} in\n{help}"
        );
    }
    for forbidden in [
        "dashboard",
        "shim",
        "evidence",
        "publish",
        "link-pr",
        "record",
        "research",
        "validate",
        "report",
        "schema",
        "workspace",
        "work",
        "service",
        "capture",
        "vcs",
        "pr",
        "repair",
        "watch",
        "context",
        "update",
        "uninstall",
        "skill",
        "sql",
    ] {
        assert!(
            !commands.lines().any(|line| {
                line.strip_prefix("  ")
                    .and_then(|line| line.split_whitespace().next())
                    == Some(forbidden)
            }),
            "forbidden command {forbidden} appeared in\n{help}"
        );
    }
}

#[test]
fn status_and_stats_help_keep_health_controls_separate_from_read_only_reporting() {
    let temp = tempdir();
    let status_output = ctx(&temp)
        .args(["status", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status_help = String::from_utf8(status_output).unwrap();
    assert!(status_help.contains("--usage <USAGE>"), "{status_help}");
    for value in ["enable", "disable", "reset"] {
        assert!(
            status_help.contains(value),
            "missing {value} in\n{status_help}"
        );
    }
    for removed in ["summary", "detail", "methodology"] {
        assert!(!status_help.contains(removed), "{status_help}");
    }

    let stats_output = ctx(&temp)
        .args(["stats", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stats_help = String::from_utf8(stats_output).unwrap();
    assert!(stats_help.contains("Usage: ctx stats"), "{stats_help}");
    assert!(stats_help.contains("--detail"), "{stats_help}");
    assert!(stats_help.contains("--format <FORMAT>"), "{stats_help}");
    for forbidden in ["--usage", "--control", "--methodology", "dashboard"] {
        assert!(!stats_help.contains(forbidden), "{stats_help}");
    }
}

#[test]
fn show_help_has_no_noop_content_selector() {
    let temp = tempdir();
    for target in ["session", "event"] {
        let output = ctx(&temp)
            .args(["show", target, "--help"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let help = String::from_utf8(output).unwrap();
        assert!(!help.contains("--content"), "{help}");
        assert!(!help.contains("indexed, complete"), "{help}");
    }

    ctx(&temp)
        .args([
            "show",
            "event",
            "00000000-0000-0000-0000-000000000000",
            "--content",
            "complete",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unexpected argument '--content'"));
}

#[test]
fn show_event_help_states_the_event_window_bounds() {
    let temp = tempdir();
    let output = ctx(&temp)
        .args(["show", "event", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let help = String::from_utf8(output).unwrap();

    for expected in [
        "Number of preceding events to include (0..50)",
        "Number of following events to include (0..50)",
        "Use this many events on both sides of the selected event (0..50)",
    ] {
        assert!(
            help.contains(expected),
            "show event help omitted {expected:?}:\n{help}"
        );
    }
}

#[test]
fn provider_help_and_errors_do_not_dump_full_provider_list() {
    let temp = tempdir();
    let help = ctx(&temp)
        .args(["import", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let help = String::from_utf8(help).unwrap();
    assert!(help.contains("for example codex, claude, cursor, pi"));
    assert!(!help.contains("factory-ai-droid"));

    let stderr = failure_stderr(ctx(&temp).args(["import", "--provider", "nope"]));
    assert!(stderr.contains("invalid value 'nope'"));
    assert!(stderr.contains("examples: codex, claude, cursor, pi"));
    assert!(!stderr.contains("[possible values:"));
    assert!(!stderr.contains("factory-ai-droid"));
}

#[test]
fn root_version_reports_package_version() {
    let temp = tempdir();
    ctx(&temp)
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn removed_commands_are_rejected() {
    let temp = tempdir();
    for command in [
        "dashboard",
        "shim",
        "evidence",
        "publish",
        "link-pr",
        "record",
        "report",
        "schema",
        "workspace",
        "work",
        "service",
        "capture",
        "vcs",
        "pr",
        "repair",
        "watch",
        "context",
        "update",
        "uninstall",
        "sql",
    ] {
        ctx(&temp)
            .arg(command)
            .assert()
            .failure()
            .stderr(predicate::str::contains("unrecognized subcommand"));
    }
}

#[test]
fn provider_help_stays_compact_for_large_supported_provider_set() {
    let temp = tempdir();
    let output = ctx(&temp)
        .args(["import", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let help = String::from_utf8(output).unwrap();

    assert!(help.contains("--provider <PROVIDER>"));
    assert!(help.contains("for example codex, claude, cursor, pi, copilot-cli, or opencode"));
    assert!(
        !help.contains("--provider <PROVIDER>\n          [possible values:"),
        "{help}"
    );
}

#[test]
fn provider_json_names_are_accepted_as_cli_filter_aliases() {
    let temp = tempdir();
    initialize_empty_store(&temp);

    for (provider, expected) in [
        ("copilot_cli", "copilot_cli"),
        ("github-copilot", "copilot_cli"),
        ("factory_ai_droid", "factory_ai_droid"),
        ("droid", "factory_ai_droid"),
        ("kilo_code", "kilo"),
        ("qwen_code", "qwen_code"),
        ("kimi_code_cli", "kimi_code_cli"),
        ("code_buddy", "codebuddy"),
        ("auggie", "auggie"),
        ("augment", "auggie"),
        ("augment-code", "auggie"),
        ("forge", "forgecode"),
        ("forge_code", "forgecode"),
        ("mistral_vibe", "mistral_vibe"),
        ("mux", "mux"),
        ("qoder-cn", "lingma"),
        ("qoder_cn", "lingma"),
        ("qoder", "qoder"),
        ("open_claw", "openclaw"),
        ("nano_claw", "nanoclaw"),
        ("astr_bot", "astrbot"),
        ("open_hands", "openhands"),
    ] {
        let search = json_output(ctx(&temp).args([
            "search",
            "anything",
            "--provider",
            provider,
            "--refresh",
            "off",
            "--format=json",
        ]));
        assert_eq!(search["filters"]["provider"], expected);
    }
}

#[test]
fn public_subcommand_help_is_golden_enough_for_session_retrieval() {
    let temp = tempdir();
    for (command, required) in [
        (
            "setup",
            vec![
                "Usage: ctx setup",
                "--catalog-only",
                "Deprecated and ignored; setup follows its normal refresh lifecycle",
                "--format <FORMAT>",
            ],
        ),
        (
            "semantic",
            vec![
                "Usage: ctx semantic [OPTIONS] <COMMAND>",
                "enable",
                "status",
                "disable",
                "Manage local semantic search",
            ],
        ),
        ("status", vec!["Usage: ctx status", "--format <FORMAT>"]),
        (
            "stats",
            vec![
                "Usage: ctx stats",
                "--detail",
                "Show CLI/MCP operation and latency breakdowns",
                "--format <FORMAT>",
                "Show local history retrieval and value statistics",
            ],
        ),
        (
            "index",
            vec![
                "Usage: ctx index",
                "mode",
                "watch",
                "wait",
                "Show or configure local indexing and follow progress",
            ],
        ),
        ("sources", vec!["Usage: ctx sources", "--format <FORMAT>"]),
        (
            "import",
            vec![
                "Usage: ctx import",
                "--provider <PROVIDER>",
                "--path <PATH>",
                "--input-format <INPUT_FORMAT>",
                "--resume",
                "--format <FORMAT>",
            ],
        ),
        ("show", vec!["Usage: ctx show", "session", "event"]),
        (
            "list",
            vec![
                "Usage: ctx list",
                "events",
                "List filtered events from one immutable Core generation",
            ],
        ),
        (
            "docs",
            vec![
                "Usage: ctx docs",
                "list",
                "search",
                "show",
                "man",
                "Read embedded ctx documentation",
            ],
        ),
        ("mcp", vec!["Usage: ctx mcp", "serve"]),
        (
            "integrations",
            vec![
                "Usage: ctx integrations",
                "install",
                "status",
                "Install or inspect ctx integrations",
            ],
        ),
        (
            "daemon",
            vec![
                "Usage: ctx daemon",
                "run",
                "Run local ctx background maintenance",
            ],
        ),
        (
            "upgrade",
            vec![
                "Usage: ctx upgrade",
                "check",
                "status",
                "enable",
                "disable",
                "Check or apply signed ctx CLI upgrades",
            ],
        ),
        (
            "search",
            vec![
                "Usage: ctx search",
                "[QUERY]",
                "Natural-language query to search local agent history",
                "--term <TERM>",
                "Add another search query or keyword",
                "--provider <PROVIDER>",
                "--workspace <WORKSPACE>",
                "Filter by stored workspace",
                "--since <SINCE>",
                "Filter to recent history, as RFC3339 or a day window like 30d",
                "--primary-only",
                "Search only primary agent sessions",
                "--content-scope <CONTENT_SCOPE>",
                "Search content scope: all, transcript, calls, or outputs",
                "--event-type <EVENT_TYPE>",
                "Filter by event type:",
                "--file <FILE>",
                "indexed touched-file path metadata",
                "--session <SESSION>",
                "--exclude-session <SESSION>",
                "Exclude one exact ctx session id or unambiguous id prefix",
                "--events",
                "--limit <LIMIT>",
                "Maximum results to return, from 1 to 200",
                "--refresh <REFRESH>",
                "Index freshness behavior. background serves the existing index",
                "--include-current-session",
                "Include the automatically detected active session tree",
                "--format <FORMAT>",
                "--verbose",
                "Print expanded text details",
            ],
        ),
        ("doctor", vec!["Usage: ctx doctor", "--format <FORMAT>"]),
    ] {
        let output = ctx(&temp)
            .args([command, "--help"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let help = String::from_utf8(output).unwrap();
        for needle in required {
            assert!(
                help.contains(needle),
                "{command} help missing {needle} in\n{help}"
            );
        }
        for forbidden in ["dashboard", "shim", "link-pr"] {
            assert!(
                !help.contains(forbidden),
                "{command} help leaked {forbidden} in\n{help}"
            );
        }
        if command == "setup" {
            assert!(!help.contains("--semantic"), "{help}");
        }
    }
}

#[test]
fn machine_readable_output_uses_format_without_a_json_alias() {
    let temp = tempdir();
    for args in [
        &["setup", "--help"][..],
        &["semantic", "enable", "--help"],
        &["semantic", "status", "--help"],
        &["semantic", "disable", "--help"],
        &["status", "--help"],
        &["stats", "--help"],
        &["index", "--help"],
        &["index", "mode", "--help"],
        &["index", "watch", "--help"],
        &["index", "wait", "--help"],
        &["sources", "--help"],
        &["import", "--help"],
        &["show", "session", "--help"],
        &["show", "event", "--help"],
        &["list", "events", "--help"],
        &["search", "--help"],
        &["docs", "list", "--help"],
        &["docs", "search", "--help"],
        &["docs", "show", "--help"],
        &["integrations", "install", "mcp", "--help"],
        &["integrations", "install", "skills", "--help"],
        &["integrations", "install", "slash-commands", "--help"],
        &["integrations", "status", "mcp", "--help"],
        &["integrations", "status", "skills", "--help"],
        &["daemon", "run", "--help"],
        &["upgrade", "--help"],
        &["upgrade", "check", "--help"],
        &["upgrade", "status", "--help"],
        &["doctor", "--help"],
    ] {
        let help = ctx(&temp)
            .args(args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let help = String::from_utf8(help).unwrap();
        assert!(help.contains("--format"), "{args:?} help:\n{help}");
        assert!(!help.contains("--json"), "{args:?} help:\n{help}");
    }

    ctx(&temp)
        .args(["status", "--json"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unexpected argument '--json'"));

    ctx(&temp)
        .args(["doctor", "--progress", "none"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "unexpected argument '--progress' found",
        ));
}

#[test]
fn builtin_throttling_remains_config_only_without_new_semantic_flags() {
    let temp = tempdir();
    for args in [
        &["semantic", "enable", "--help"][..],
        &["semantic", "status", "--help"],
        &["semantic", "disable", "--help"],
    ] {
        let output = ctx(&temp).args(args).output().unwrap();
        assert!(output.status.success(), "{args:?}: {:?}", output.stderr);
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(!help.contains("builtin-throttling"), "{help}");
        assert!(!help.contains("builtin_throttling"), "{help}");
    }
}

#[test]
fn indexing_mode_and_daemon_run_help_expose_the_public_controls() {
    let temp = tempdir();
    for (args, required) in [
        (
            vec!["index", "mode", "--help"],
            vec![
                "Usage: ctx index mode",
                "[MODE]",
                "auto",
                "manual",
                "--format <FORMAT>",
                "Show or change automatic indexing mode",
            ],
        ),
        (
            vec!["daemon", "run", "--help"],
            vec![
                "Usage: ctx daemon run",
                "--loop-interval-seconds <LOOP_INTERVAL_SECONDS>",
                "Wait this many seconds between maintenance passes",
                "--max-chunks <MAX_CHUNKS>",
                "Process at most this many semantic chunks per pass",
                "--force",
                "--format <FORMAT>",
                "foreground until stopped",
                "does not change the configured indexing mode",
            ],
        ),
    ] {
        let output = ctx(&temp)
            .args(args.clone())
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let help = String::from_utf8(output).unwrap();
        for needle in required {
            assert!(
                help.contains(needle),
                "{args:?} help missing {needle} in\n{help}"
            );
        }
        assert!(
            !help.contains("--max-seconds"),
            "{args:?} help must not expose a daemon runtime cap in\n{help}"
        );
        assert!(!help.contains("--once"), "{args:?} help:\n{help}");
        assert!(
            !help.contains("--idle-exit-seconds"),
            "{args:?} help:\n{help}"
        );
    }

    let daemon_help = String::from_utf8(
        ctx(&temp)
            .args(["daemon", "--help"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap();
    let daemon_commands = daemon_help
        .split("Commands:")
        .nth(1)
        .and_then(|tail| tail.split("Options:").next())
        .unwrap_or(&daemon_help);
    assert!(daemon_commands.contains("run"), "{daemon_help}");
    for hidden in ["status", "enable", "disable"] {
        assert!(!daemon_commands.contains(hidden), "{daemon_help}");
    }

    let stderr = ctx(&temp)
        .args(["daemon", "run", "--once"])
        .assert()
        .code(2)
        .get_output()
        .stderr
        .clone();
    let stderr = String::from_utf8(stderr).unwrap();
    assert!(
        stderr.contains("The --once option has been retired"),
        "{stderr}"
    );
    assert!(stderr.contains("ctx import --help"), "{stderr}");
    assert!(!stderr.contains("--idle-exit-seconds"), "{stderr}");
    assert!(!stderr.contains("--force"), "{stderr}");

    let stderr = ctx(&temp)
        .args(["daemon", "run", "--idle-exit-seconds", "60"])
        .assert()
        .code(2)
        .get_output()
        .stderr
        .clone();
    let stderr = String::from_utf8(stderr).unwrap();
    assert!(
        stderr.contains("The --idle-exit-seconds option has been removed"),
        "{stderr}"
    );
    assert!(stderr.contains("persistent foreground worker"), "{stderr}");
    assert!(stderr.contains("has no idle timeout"), "{stderr}");
    assert!(stderr.contains("ctx daemon run --help"), "{stderr}");
}

#[test]
fn daemon_run_rejects_public_runtime_cap() {
    let temp = tempdir();
    let stderr = failure_stderr(ctx(&temp).args(["daemon", "run", "--max-seconds", "1"]));
    assert!(
        stderr.contains("unexpected argument '--max-seconds'"),
        "daemon run must not accept --max-seconds; stderr:\n{stderr}"
    );
}

#[test]
fn daemon_run_rejects_internal_autostart_metadata_flags() {
    let temp = tempdir();
    for args in [
        ["daemon", "run", "--start-mode", "auto"],
        ["daemon", "run", "--trigger-command", "setup"],
    ] {
        let stderr = failure_stderr(ctx(&temp).args(args));
        assert!(
            stderr.contains("daemon autostart metadata flags are internal"),
            "daemon run must reject internal metadata flags; stderr:\n{stderr}"
        );
    }
}

#[test]
fn docs_commands_expose_embedded_docs_and_man_pages() {
    let temp = tempdir();

    let list = json_output(ctx(&temp).args(["--color=always", "docs", "list", "--format=json"]));
    assert_eq!(list["schema_version"], 1);
    assert!(list["topics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|topic| topic["id"] == "cli-reference"));
    for topic_id in ["docs", "mcp", "upgrade"] {
        assert!(list["topics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|topic| topic["id"] == topic_id));
    }

    let search = json_output(ctx(&temp).args(["docs", "search", "upgrade", "--format=json"]));
    assert_eq!(search["schema_version"], 1);
    assert_eq!(search["query"], "upgrade");
    assert!(!search["results"].as_array().unwrap().is_empty());

    let mcp_search = json_output(ctx(&temp).args(["docs", "search", "mcp", "--format=json"]));
    let mcp_result_ids = mcp_search["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|topic| topic["id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(mcp_result_ids.contains(&"mcp"));
    assert!(mcp_result_ids.contains(&"mcp-integrations"));

    let upgrade_search =
        json_output(ctx(&temp).args(["docs", "search", "upgrade", "--format=json"]));
    assert_eq!(upgrade_search["results"][0]["id"], "upgrade");

    let weak_search = json_output(ctx(&temp).args(["docs", "search", "a", "--format=json"]));
    assert!(weak_search["results"].as_array().unwrap().is_empty());
    assert!(weak_search["suggested_next_commands"]
        .as_array()
        .unwrap()
        .iter()
        .any(|command| command == "ctx docs list"));

    let show = json_output(ctx(&temp).args(["docs", "show", "cli-reference", "--format", "json"]));
    assert_eq!(show["schema_version"], 1);
    assert_eq!(show["id"], "cli-reference");
    assert!(show["body"].as_str().unwrap().contains("ctx search"));

    let mcp = json_output(ctx(&temp).args(["docs", "show", "mcp", "--format", "json"]));
    assert!(mcp["body"].as_str().unwrap().contains("ctx mcp serve"));

    let mcp_integrations =
        json_output(ctx(&temp).args(["docs", "show", "mcp-integrations", "--format", "json"]));
    assert!(mcp_integrations["body"]
        .as_str()
        .unwrap()
        .contains("ctx integrations install mcp"));

    let upgrade = json_output(ctx(&temp).args(["docs", "show", "upgrade", "--format", "json"]));
    let upgrade_body = upgrade["body"].as_str().unwrap();
    assert!(upgrade_body.contains("ctx upgrade status"));
    assert!(upgrade_body.contains("managed default is `upgrade.auto = \"apply\"`"));
    assert!(upgrade_body.contains("CTX_UPGRADE_AUTO=off"));
    assert!(upgrade_body.contains("ctx upgrade disable"));
    assert!(upgrade_body.contains("enabled persistent daemon is the sole"));
    assert!(upgrade_body.contains("Ordinary\nforeground commands and MCP never claim or spawn"));

    let unmanaged =
        json_output(ctx(&temp).args(["docs", "show", "unmanaged-installs", "--format", "json"]));
    let unmanaged_body = unmanaged["body"].as_str().unwrap();
    assert!(unmanaged_body.contains("codesign --verify --strict --verbose=4 \"$(command -v ctx)\""));
    assert!(
        unmanaged_body.contains("spctl --assess --verbose=4 --type install \"$(command -v ctx)\"")
    );
    assert!(unmanaged_body.contains("codesign -d --verbose=4 \"$(command -v ctx)\""));

    let missing_topic = ctx(&temp)
        .args(["--color=always", "docs", "show", "cli"])
        .assert()
        .failure()
        .get_output()
        .clone();
    assert_eq!(missing_topic.status.code(), Some(1));
    assert!(missing_topic.stdout.is_empty());
    let missing_topic = String::from_utf8(missing_topic.stderr).unwrap();
    assert_eq!(
        missing_topic.matches("Unknown ctx docs topic: cli").count(),
        1
    );
    assert!(missing_topic.contains("Unknown ctx docs topic: cli"));
    assert!(missing_topic.contains("Nearest topics"));
    assert!(missing_topic.contains("ctx docs list"));
    assert!(missing_topic.contains("ctx docs search cli"));
    assert!(!missing_topic.contains("\\n"));
    assert!(missing_topic.as_bytes().contains(&0x1b));

    let missing_topic_json = ctx(&temp)
        .args(["--color=always", "docs", "show", "cli", "--format=json"])
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(missing_topic_json.stdout.is_empty());
    assert_eq!(
        String::from_utf8(missing_topic_json.stderr).unwrap(),
        concat!(
            "Error: unknown ctx docs topic: cli\n",
            "nearest topics: cli-reference slash-command-integrations mcp-integrations\n",
            "try: ctx docs list\n",
            "try: ctx docs search cli\n",
        )
    );

    let man = ctx(&temp)
        .args(["docs", "man", "--print", "ctx"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let man = String::from_utf8(man).unwrap();
    assert!(man.contains(".TH ctx"));
    assert!(man.contains("Search local agent history"));

    let stats_man = ctx(&temp)
        .args(["docs", "man", "--print", "ctx-stats"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stats_man = String::from_utf8(stats_man).unwrap();
    assert!(stats_man.contains(".TH ctx-stats"));
    assert!(stats_man.contains(r"\-\-detail"));
}

#[test]
fn status_and_doctor_report_external_install_auto_off() {
    let temp = tempdir();
    for command in ["status", "doctor"] {
        let default = json_output(
            ctx(&temp)
                .args([command, "--format=json"])
                .env_remove("CTX_UPGRADE_AUTO"),
        );
        assert_eq!(default["upgrade"]["auto"], "off");
        assert_eq!(default["upgrade"]["auto_enabled"], false);

        let process_enable = json_output(
            ctx(&temp)
                .args([command, "--format=json"])
                .env("CTX_UPGRADE_AUTO", "apply"),
        );
        assert_eq!(process_enable["upgrade"]["auto"], "off");
        assert_eq!(process_enable["upgrade"]["auto_enabled"], false);

        let process_opt_out = json_output(
            ctx(&temp)
                .args([command, "--format=json"])
                .env("CTX_UPGRADE_AUTO", "off"),
        );
        assert_eq!(process_opt_out["upgrade"]["auto"], "off");
        assert_eq!(process_opt_out["upgrade"]["auto_enabled"], false);
    }

    fs::create_dir_all(data_root(&temp)).unwrap();
    fs::write(
        data_root(&temp).join("config.toml"),
        "[upgrade]\nauto = \"off\"\n",
    )
    .unwrap();
    let persistent_opt_out = json_output(
        ctx(&temp)
            .args(["status", "--format=json"])
            .env("CTX_UPGRADE_AUTO", "apply"),
    );
    assert_eq!(persistent_opt_out["upgrade"]["auto"], "off");
    assert_eq!(persistent_opt_out["upgrade"]["auto_enabled"], false);
}

fn assert_safe_platform_install_action(action: &str) {
    assert!(
        action.contains("ctx daemon disable --prepare-uninstall --format=json"),
        "{action}"
    );
    assert!(action.contains("after a successful receipt"), "{action}");
    assert!(
        action.contains("ctx docs show unmanaged-installs"),
        "{action}"
    );
    assert!(!action.contains("reinstall ctx from https://ctx.rs/install"));
    #[cfg(windows)]
    {
        assert!(
            action.contains("irm https://ctx.rs/install.ps1 | iex"),
            "{action}"
        );
        assert!(!action.contains("curl -fsSL"), "{action}");
    }
    #[cfg(not(windows))]
    {
        assert!(
            action.contains("curl -fsSL https://ctx.rs/install | sh"),
            "{action}"
        );
        assert!(!action.contains("install.ps1"), "{action}");
    }
}

#[test]
fn status_and_doctor_require_safe_handoff_for_absent_and_invalid_markers() {
    let temp = tempdir();
    let binary = bind_test_ctx_binary(&temp);

    for command in ["status", "doctor"] {
        let absent = json_output(ctx(&temp).args([command, "--format=json"]));
        assert_eq!(absent["upgrade"]["install"]["marker"], "absent");
        assert_safe_platform_install_action(
            absent["upgrade"]["install"]["action"].as_str().unwrap(),
        );
    }

    fs::write(hosted_install_marker_path(&binary), b"{not-json").unwrap();
    for command in ["status", "doctor"] {
        let invalid = json_output(ctx(&temp).args([command, "--format=json"]));
        assert_eq!(invalid["upgrade"]["install"]["marker"], "corrupt");
        assert_eq!(invalid["upgrade"]["auto"], "off");
        assert_eq!(invalid["upgrade"]["auto_enabled"], false);
        let error = invalid["upgrade"]["install"]["error"].as_str().unwrap();
        assert!(error.contains("parse ctx install marker"), "{error}");
        assert_safe_platform_install_action(error);
        assert_safe_platform_install_action(
            invalid["upgrade"]["install"]["action"].as_str().unwrap(),
        );

        if command == "doctor" {
            let finding = invalid["findings"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(serde_json::Value::as_str)
                .find(|finding| finding.contains("managed ctx install marker is corrupt"))
                .expect("doctor invalid-marker finding");
            assert!(finding.contains("parse ctx install marker"), "{finding}");
            assert_safe_platform_install_action(finding);
        }
    }

    let stderr = failure_stderr(ctx(&temp).args(["upgrade", "enable"]));
    assert!(stderr.contains("parse ctx install marker"), "{stderr}");
    assert_safe_platform_install_action(&stderr);
    assert!(!data_root(&temp).join("config.toml").exists());
}

#[test]
fn external_install_upgrade_enable_fails_without_writing_config() {
    let temp = tempdir();
    let failed = ctx(&temp)
        .args(["upgrade", "enable"])
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(failed.stdout.is_empty(), "{:?}", failed.stdout);
    let stderr = String::from_utf8(failed.stderr).unwrap();
    assert!(
        stderr.contains("ctx is not installed by the hosted installer"),
        "{stderr}"
    );
    assert_safe_platform_install_action(&stderr);
    assert!(!data_root(&temp).join("config.toml").exists());

    let disabled = json_output(ctx(&temp).args(["upgrade", "--format=json", "disable"]));
    assert_eq!(disabled["schema_version"], 1);
    assert_eq!(disabled["command"], "upgrade_disable");
    assert_eq!(disabled["status"], "disabled");
    assert_eq!(disabled["auto"], "off");
    assert_eq!(disabled["enabled"], false);
}

#[test]
fn docs_show_out_creates_parent_directories() {
    let temp = tempdir();
    let out = temp.path().join("nested").join("doc.txt");

    ctx(&temp)
        .args([
            "--color=always",
            "docs",
            "show",
            "cli-reference",
            "--out",
            out.to_str().unwrap(),
        ])
        .assert()
        .success();

    assert!(
        out.exists(),
        "docs show --out should write the requested file"
    );
    let body = fs::read_to_string(&out).unwrap();
    assert!(body.contains("CLI Reference"), "{body}");
    assert!(!body.as_bytes().contains(&0x1b), "{body}");
}

#[cfg(unix)]
#[test]
fn provider_session_lookup_requires_explicit_provider_flags_in_help() {
    let temp = tempdir();
    for args in [vec!["show", "session", "--help"]] {
        let output = ctx(&temp)
            .args(args.clone())
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let help = String::from_utf8(output).unwrap();
        for needle in [
            "--provider <PROVIDER>",
            "--provider-session <PROVIDER_SESSION>",
        ] {
            assert!(
                help.contains(needle),
                "{args:?} help missing {needle} in\n{help}"
            );
        }
        if args.as_slice() == ["show", "session", "--help"] {
            for needle in [
                "--mode <MODE>",
                "--out <OUT>",
                "[default: lite]",
                "[possible values: full, lite, log]",
            ] {
                assert!(
                    help.contains(needle),
                    "{args:?} help missing {needle} in\n{help}"
                );
            }
        }
    }
}

#[test]
fn provider_session_rejects_whitespace_only_value() {
    let temp = tempdir();
    ctx_with_enabled_daemon(&temp)
        .arg("setup")
        .assert()
        .success();

    let args = vec![
        "show",
        "session",
        "--provider",
        "codex",
        "--provider-session",
        " ",
    ];
    let stderr = failure_stderr(ctx(&temp).args(&args));
    assert_eq!(
        stderr, "✗ provider session ID must not be empty\n",
        "unexpected typed diagnostic for {args:?}"
    );
}

#[test]
fn removed_public_commands_are_rejected() {
    let temp = tempdir();
    let root_output = ctx(&temp)
        .arg("--help")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let root_help = String::from_utf8(root_output).unwrap();
    let commands = root_help
        .split("Commands:")
        .nth(1)
        .and_then(|tail| tail.split("Options:").next())
        .unwrap_or(&root_help);
    for removed in [
        "context",
        "export",
        "validate",
        "materialize",
        "related",
        "timeline",
        "facts",
    ] {
        assert!(
            !commands.contains(removed),
            "removed {removed} command appeared in root help\n{root_help}"
        );
    }

    for args in [
        vec!["context", "onboarding", "--format=json"],
        vec!["export", "session", "00000000-0000-0000-0000-000000000000"],
        vec!["validate", "--format=json"],
        vec!["materialize", "--format=json"],
        vec!["related", "commit", "abc"],
        vec!["timeline", "commit", "abc"],
        vec!["facts", "commit", "abc"],
    ] {
        ctx(&temp).args(args.clone()).assert().failure().stderr(
            predicate::str::contains("unrecognized subcommand")
                .and(predicate::str::contains(args[0])),
        );
    }
}
