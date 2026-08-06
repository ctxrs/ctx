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
        "setup",
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
fn pro_help_advertises_the_fixed_monthly_price() {
    let temp = tempdir();
    let output = ctx(&temp)
        .args(["pro", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let help = String::from_utf8(output).unwrap();
    assert!(help.contains("Price: $20/month"), "{help}");
}

#[test]
fn pro_uninstall_help_uses_local_pro_data_terminology() {
    let temp = tempdir();
    let output = ctx(&temp)
        .args(["pro", "uninstall", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let help = String::from_utf8(output).unwrap();
    assert!(help.contains("Delete local Pro data"), "{help}");
    assert!(
        help.contains("Preserve local Pro data for later setup"),
        "{help}"
    );
    assert!(!help.contains("encrypted graph"), "{help}");
    assert!(!help.contains("credentials"), "{help}");
}

#[test]
fn blame_help_explains_launch_targets_and_bounds() {
    let temp = tempdir();
    let output = ctx(&temp)
        .args(["blame", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let help = String::from_utf8(output).unwrap();
    assert!(help.contains("ctx blame [OPTIONS] <TARGET>"), "{help}");
    assert!(help.contains("ctx blame <COMMAND>"), "{help}");
    assert!(help.contains("--type <TYPE>"), "{help}");
    assert!(!help.contains("--evidence-preview"), "{help}");
    assert!(help.contains("possible values: file, commit, pr"), "{help}");
    assert!(help.contains("overrides auto-detection"), "{help}");

    for args in [
        vec!["blame", "file", "--help"],
        vec!["blame", "commit", "--help"],
        vec!["blame", "pr", "--help"],
    ] {
        let output = ctx(&temp)
            .args(&args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let help = String::from_utf8(output).unwrap();
        assert!(
            help.to_ascii_lowercase()
                .contains("logical repository identity"),
            "{args:?} help omitted repository semantics:\n{help}"
        );
        assert!(
            help.contains("forge:github.com/ctxrs/ctx"),
            "{args:?} help omitted a concrete logical identity:\n{help}"
        );
        assert!(
            help.contains("Maximum complete matches to return, from 1 to 100"),
            "{args:?} help omitted the limit contract:\n{help}"
        );
        assert!(help.contains("--cursor <CURSOR>"));
        assert!(!help.contains("--evidence-preview"), "{args:?}:\n{help}");
        for secret in ["generation", "project", "heuristic"] {
            assert!(!help.contains(secret), "{args:?} leaked {secret}:\n{help}");
        }
        assert!(!help.contains("Codex"), "{args:?} leaked Codex:\n{help}");
        if args.as_slice() == ["blame", "file", "--help"] {
            assert!(
                help.contains("--lines <START[:END]>"),
                "{args:?} help omitted line-range semantics:\n{help}"
            );
        } else {
            assert!(
                !help.contains("--lines"),
                "non-file blame help advertised --lines:\n{help}"
            );
        }
    }
}

#[test]
fn cli_rejects_invalid_blame_selectors_before_local_pro_access() {
    let temp = tempdir();
    let stderr = failure_stderr(ctx(&temp).args(["blame", "file", "src/lib.rs", "--lines", "0"]));
    assert!(stderr.contains("line number must be positive"));
    let stderr =
        failure_stderr(ctx(&temp).args(["blame", "file", "src/lib.rs", "--lines", "60:42"]));
    assert!(stderr.contains("END >= START"));
    let stderr = failure_stderr(ctx(&temp).args(["blame", "pr", "0", "--repository", "ctxrs/ctx"]));
    assert!(stderr.contains("positive decimal number"));
    let stderr = failure_stderr(ctx(&temp).args(["blame", "42"]));
    assert!(
        stderr.contains("pull request number requires a repository selector"),
        "{stderr}"
    );
    assert!(!stderr.contains("pro_not_installed"), "{stderr}");
    for repository in ["", "   "] {
        let stderr = failure_stderr(ctx(&temp).args([
            "blame",
            "commit",
            "abc123",
            "--repository",
            repository,
            "--format=json",
        ]));
        let diagnostic: serde_json::Value = serde_json::from_str(&stderr).unwrap();
        assert_eq!(diagnostic["error"], "invalid_request");
        assert_eq!(diagnostic["error_code"], "invalid_request");
        assert_eq!(diagnostic["reason"], "request_invalid");
        assert_eq!(diagnostic["retryable"], false);
        assert!(!stderr.contains("repository selector"), "{stderr}");
    }

    for args in [
        &["blame", "main"][..],
        &["blame", "main", "--format=json"][..],
    ] {
        let stderr = failure_stderr(ctx(&temp).args(args));
        assert!(stderr.contains("invalid_request"), "{stderr}");
        if args.contains(&"--format=json") {
            let diagnostic: serde_json::Value = serde_json::from_str(&stderr).unwrap();
            assert_eq!(diagnostic["error"], "invalid_request");
            assert_eq!(diagnostic["reason"], "request_invalid");
            assert_eq!(diagnostic["retryable"], false);
            assert!(!stderr.contains("target type is ambiguous"), "{stderr}");
            continue;
        }
        assert!(stderr.contains("target type is ambiguous"), "{stderr}");
        assert!(stderr.contains("--type file"), "{stderr}");
        assert!(stderr.contains("--type commit"), "{stderr}");
        assert!(stderr.contains("--type pr"), "{stderr}");
        assert!(!stderr.contains("pro_not_installed"), "{stderr}");
    }

    let stderr = failure_stderr(ctx(&temp).args([
        "blame",
        "src/lib.rs",
        "--type",
        "unknown",
        "--format=json",
    ]));
    let diagnostic: serde_json::Value = serde_json::from_str(&stderr).unwrap();
    assert_eq!(diagnostic["error"], "invalid_request");
    assert_eq!(diagnostic["reason"], "request_invalid");
    assert!(!stderr.contains("unknown"), "{stderr}");
    assert!(!stderr.contains("pro_not_installed"), "{stderr}");
}

#[test]
fn blame_json_sanitizes_malformed_config_before_pro_access() {
    let temp = tempdir();
    fs::create_dir_all(data_root(&temp)).unwrap();
    fs::write(
        data_root(&temp).join("config.toml"),
        "private malformed config at /home/alice/repository",
    )
    .unwrap();

    let stderr = failure_stderr(ctx(&temp).args(["blame", "commit", "abc1234", "--format=json"]));
    let diagnostic: serde_json::Value = serde_json::from_str(&stderr).unwrap();
    assert_eq!(diagnostic["error"], "invalid_response");
    assert_eq!(diagnostic["reason"], "helper_response_invalid");
    assert_eq!(diagnostic["retryable"], false);
    assert!(!stderr.contains("alice"), "{stderr}");
    assert!(!stderr.contains("repository"), "{stderr}");
    assert!(!stderr.contains('\u{1b}'), "{stderr}");
}

#[test]
fn removed_evidence_preview_flag_is_unknown_before_pro_or_core_access() {
    let temp = tempdir();
    let root = data_root(&temp);
    for args in [
        &["blame", "src/lib.rs", "--evidence-preview"][..],
        &["blame", "abc1234", "--evidence-preview"],
        &[
            "blame",
            "42",
            "--repository",
            "forge:github.com/ctxrs/ctx",
            "--evidence-preview",
        ],
        &["blame", "file", "src/lib.rs", "--evidence-preview"],
        &["blame", "commit", "abc1234", "--evidence-preview"],
        &[
            "blame",
            "pr",
            "42",
            "--repository",
            "forge:github.com/ctxrs/ctx",
            "--evidence-preview",
        ],
    ] {
        let output = ctx(&temp).args(args).output().unwrap();
        assert!(!output.status.success(), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert!(!output.stderr.contains(&0x1b), "{args:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stderr.contains("unexpected argument '--evidence-preview'"),
            "{args:?}: {stderr}"
        );
        assert!(
            !root.exists(),
            "rejected {args:?} created {}",
            root.display()
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
        ("trae", "trae"),
        ("trae-cn", "trae"),
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
        ("windsurf_cascade", "windsurf"),
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
                "--semantic",
                "Enable local semantic search in config",
                "--format <FORMAT>",
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
                "status",
                "watch",
                "wait",
                "Show, watch, or wait for local indexing progress",
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
                "status",
                "enable",
                "disable",
                "Run or inspect local ctx background maintenance",
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
                "--include-subagents",
                "Include subagent sessions",
                "--content-scope <CONTENT_SCOPE>",
                "Search content scope: all, transcript, calls, or outputs",
                "--event-type <EVENT_TYPE>",
                "Filter by event type:",
                "--file <FILE>",
                "indexed touched-file path metadata",
                "--session <SESSION>",
                "--events",
                "--limit <LIMIT>",
                "Maximum results to return, from 1 to 200",
                "--refresh <REFRESH>",
                "Index freshness behavior. background serves the existing index",
                "--include-current-session",
                "Include the active Codex session tree when CODEX_THREAD_ID is set",
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
    }
}

#[test]
fn machine_readable_output_uses_format_without_a_json_alias() {
    let temp = tempdir();
    for args in [
        &["setup", "--help"][..],
        &["status", "--help"],
        &["stats", "--help"],
        &["index", "watch", "--help"],
        &["index", "wait", "--help"],
        &["sources", "--help"],
        &["import", "--help"],
        &["show", "session", "--help"],
        &["show", "event", "--help"],
        &["list", "events", "--help"],
        &["search", "--help"],
        &["pro", "--help"],
        &["pro", "setup", "--help"],
        &["pro", "manage", "--help"],
        &["pro", "uninstall", "--help"],
        &["referral", "create", "--help"],
        &["referral", "status", "--help"],
        &["referral", "payout", "--help"],
        &["blame", "file", "--help"],
        &["blame", "commit", "--help"],
        &["blame", "pr", "--help"],
        &["docs", "list", "--help"],
        &["docs", "search", "--help"],
        &["docs", "show", "--help"],
        &["integrations", "install", "mcp", "--help"],
        &["integrations", "install", "skills", "--help"],
        &["integrations", "install", "slash-commands", "--help"],
        &["integrations", "status", "mcp", "--help"],
        &["integrations", "status", "skills", "--help"],
        &["daemon", "run", "--help"],
        &["daemon", "status", "--help"],
        &["daemon", "enable", "--help"],
        &["daemon", "disable", "--help"],
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
fn daemon_help_exposes_readable_status_and_run_controls() {
    let temp = tempdir();
    for (args, required) in [
        (
            vec!["daemon", "status", "--help"],
            vec![
                "Usage: ctx daemon status",
                "--format <FORMAT>",
                "Show ctx daemon status",
            ],
        ),
        (
            vec!["daemon", "run", "--help"],
            vec![
                "Usage: ctx daemon run",
                "--idle-exit-seconds <IDLE_EXIT_SECONDS>",
                "Exit after this many seconds without maintenance work",
                "--loop-interval-seconds <LOOP_INTERVAL_SECONDS>",
                "Wait this many seconds between maintenance passes",
                "--max-chunks <MAX_CHUNKS>",
                "Process at most this many semantic chunks per pass",
                "--force",
                "--format <FORMAT>",
            ],
        ),
        (
            vec!["daemon", "enable", "--help"],
            vec![
                "Usage: ctx daemon enable",
                "--format <FORMAT>",
                "Enable ctx daemon maintenance",
            ],
        ),
        (
            vec!["daemon", "disable", "--help"],
            vec![
                "Usage: ctx daemon disable",
                "--format <FORMAT>",
                "Disable ctx daemon maintenance",
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
    }

    let stderr = failure_stderr(ctx(&temp).args(["daemon", "run", "--once"]));
    assert!(
        stderr.contains("The --once option has been retired"),
        "{stderr}"
    );
    assert!(
        stderr.contains("ctx daemon run --idle-exit-seconds <SECONDS>"),
        "{stderr}"
    );
    assert!(!stderr.contains("--force"), "{stderr}");
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
    assert!(upgrade_body.contains("on by default (`upgrade.auto = \"apply\"`)"));
    assert!(upgrade_body.contains("CTX_UPGRADE_AUTO=off"));
    assert!(upgrade_body.contains("ctx upgrade disable"));
    assert!(upgrade_body.contains("Foreground commands and MCP"));
    assert!(upgrade_body.contains("never schedule an upgrade"));

    let unmanaged =
        json_output(ctx(&temp).args(["docs", "show", "unmanaged-installs", "--format", "json"]));
    let unmanaged_body = unmanaged["body"].as_str().unwrap();
    assert!(unmanaged_body.contains("codesign --verify --strict --verbose=4 \"$(command -v ctx)\""));
    assert!(
        unmanaged_body.contains("spctl --assess --verbose=4 --type install \"$(command -v ctx)\"")
    );
    assert!(unmanaged_body.contains("codesign -d --verbose=4 \"$(command -v ctx)\""));

    let missing_topic = failure_stderr(ctx(&temp).args(["--color=always", "docs", "show", "cli"]));
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
fn status_and_doctor_report_effective_upgrade_auto_mode() {
    let temp = tempdir();
    for command in ["status", "doctor"] {
        let default = json_output(
            ctx(&temp)
                .args([command, "--format=json"])
                .env_remove("CTX_UPGRADE_AUTO"),
        );
        assert_eq!(default["upgrade"]["auto"], "apply");
        assert_eq!(default["upgrade"]["auto_enabled"], true);

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

#[test]
fn upgrade_auto_mode_has_one_human_or_machine_receipt() {
    let temp = tempdir();
    let human = ctx(&temp)
        .args(["upgrade", "enable"])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8(human.stdout).unwrap();
    assert_eq!(stdout.matches("Automatic upgrades enabled").count(), 1);
    assert!(!stdout.contains("ctx automatic upgrade"), "{stdout}");
    assert!(human.stderr.is_empty(), "{:?}", human.stderr);

    let enabled = json_output(ctx(&temp).args(["upgrade", "--format=json", "enable"]));
    assert_eq!(enabled["schema_version"], 1);
    assert_eq!(enabled["command"], "upgrade_enable");
    assert_eq!(enabled["status"], "enabled");
    assert_eq!(enabled["auto"], "apply");
    assert_eq!(enabled["enabled"], true);

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

    for obsolete in ["status", "install", "update"] {
        ctx(&temp)
            .args(["pro", obsolete])
            .assert()
            .failure()
            .stderr(predicate::str::contains("unrecognized subcommand"));
    }
}
