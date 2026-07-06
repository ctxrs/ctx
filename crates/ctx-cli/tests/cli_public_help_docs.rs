mod support;

use support::*;

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
        "setup", "status", "sources", "import", "show", "search", "docs", "locate", "mcp", "sql",
        "upgrade", "doctor",
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
        "list",
        "export",
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
    ] {
        assert!(
            !commands.contains(&format!("  {forbidden}")),
            "forbidden command {forbidden} appeared in\n{help}"
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
            "--json",
        ]));
        assert_eq!(search["filters"]["provider"], expected);
    }
}

#[test]
fn public_subcommand_help_is_golden_enough_for_session_retrieval() {
    let temp = tempdir();
    for (command, required) in [
        ("setup", vec!["Usage: ctx setup", "--json"]),
        ("status", vec!["Usage: ctx status", "--json"]),
        ("sources", vec!["Usage: ctx sources", "--json"]),
        (
            "import",
            vec![
                "Usage: ctx import",
                "--provider <PROVIDER>",
                "--path <PATH>",
                "--format <FORMAT>",
                "--resume",
                "--json",
            ],
        ),
        ("show", vec!["Usage: ctx show", "session", "event"]),
        ("locate", vec!["Usage: ctx locate", "session", "event"]),
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
            "sql",
            vec![
                "Usage: ctx sql",
                "--format <FORMAT>",
                "--file <FILE>",
                "--max-rows <MAX_ROWS>",
                "Run read-only SQL against the local ctx index",
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
                "--event-type <EVENT_TYPE>",
                "Filter by event type:",
                "--file <FILE>",
                "indexed touched-file path metadata",
                "--session <SESSION>",
                "--events",
                "--limit <LIMIT>",
                "Maximum results to return, from 1 to 200",
                "--refresh <REFRESH>",
                "Pre-search refresh behavior. auto best-effort refreshes",
                "--include-current-session",
                "Include the active Codex session tree when CODEX_THREAD_ID is set",
                "--json",
                "Print machine-readable JSON",
                "--verbose",
                "Print expanded text details",
            ],
        ),
        ("doctor", vec!["Usage: ctx doctor", "--json", "--progress"]),
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
        for forbidden in ["dashboard", "shim", "publish", "link-pr"] {
            assert!(
                !help.contains(forbidden),
                "{command} help leaked {forbidden} in\n{help}"
            );
        }
    }
}

#[test]
fn docs_commands_expose_embedded_docs_and_man_pages() {
    let temp = tempdir();

    let list = json_output(ctx(&temp).args(["docs", "list", "--json"]));
    assert_eq!(list["schema_version"], 1);
    assert!(list["topics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|topic| topic["id"] == "cli-reference"));
    for topic_id in ["docs", "mcp", "sql", "upgrade"] {
        assert!(list["topics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|topic| topic["id"] == topic_id));
    }

    let search = json_output(ctx(&temp).args(["docs", "search", "upgrade", "--json"]));
    assert_eq!(search["schema_version"], 1);
    assert_eq!(search["query"], "upgrade");
    assert!(!search["results"].as_array().unwrap().is_empty());

    let sql_search = json_output(ctx(&temp).args(["docs", "search", "sql", "--json"]));
    assert_eq!(sql_search["results"][0]["id"], "sql");

    let mcp_search = json_output(ctx(&temp).args(["docs", "search", "mcp", "--json"]));
    assert_eq!(mcp_search["results"][0]["id"], "mcp");

    let upgrade_search = json_output(ctx(&temp).args(["docs", "search", "upgrade", "--json"]));
    assert_eq!(upgrade_search["results"][0]["id"], "upgrade");

    let weak_search = json_output(ctx(&temp).args(["docs", "search", "a", "--json"]));
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

    let upgrade = json_output(ctx(&temp).args(["docs", "show", "upgrade", "--format", "json"]));
    assert!(upgrade["body"]
        .as_str()
        .unwrap()
        .contains("ctx upgrade status"));

    let missing_topic = failure_stderr(ctx(&temp).args(["docs", "show", "cli"]));
    assert!(missing_topic.contains("unknown ctx docs topic: cli"));
    assert!(missing_topic.contains("nearest topics:"));
    assert!(missing_topic.contains("ctx docs list"));
    assert!(missing_topic.contains("ctx docs search cli"));

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
}

#[test]
fn docs_show_out_creates_parent_directories() {
    let temp = tempdir();
    let out = temp.path().join("nested").join("doc.txt");

    ctx(&temp)
        .args([
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
}

#[cfg(unix)]
#[test]
fn provider_session_lookup_requires_explicit_provider_flags_in_help() {
    let temp = tempdir();
    for args in [
        vec!["show", "session", "--help"],
        vec!["locate", "session", "--help"],
        vec!["locate", "event", "--help"],
    ] {
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
            if args.as_slice() == ["locate", "event", "--help"] {
                continue;
            }
            assert!(
                help.contains(needle),
                "{args:?} help missing {needle} in\n{help}"
            );
        }
        if args[0] == "locate" {
            assert!(
                help.contains("[possible values: text, json]"),
                "{args:?} help should restrict locate formats to text/json in\n{help}"
            );
            assert!(
                !help.contains("markdown") && !help.contains("jsonl"),
                "{args:?} help leaked unsupported locate formats in\n{help}"
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
    ctx(&temp).arg("setup").assert().success();

    for args in [
        vec![
            "show",
            "session",
            "--provider",
            "codex",
            "--provider-session",
            " ",
        ],
        vec![
            "locate",
            "session",
            "--provider",
            "codex",
            "--provider-session",
            " ",
        ],
    ] {
        let stderr = failure_stderr(ctx(&temp).args(&args));
        assert!(
            stderr.contains("--provider-session cannot be empty"),
            "expected empty-value error for {args:?}, got: {stderr}"
        );
    }
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
    for removed in ["context", "list", "export", "validate"] {
        assert!(
            !commands.contains(removed),
            "removed {removed} command appeared in root help\n{root_help}"
        );
    }

    for args in [
        vec!["context", "onboarding", "--json"],
        vec!["list", "--json"],
        vec!["export", "session", "00000000-0000-0000-0000-000000000000"],
        vec!["validate", "--json"],
    ] {
        ctx(&temp).args(args.clone()).assert().failure().stderr(
            predicate::str::contains("unrecognized subcommand")
                .and(predicate::str::contains(args[0])),
        );
    }
}
