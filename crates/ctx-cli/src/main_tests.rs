use super::{parse_event_window_limit, Cli};
use crate::cli::parse_search_limit;
use crate::commands::sql::parse_sql_timeout;
use crate::pro::parse_referral_codename;
use crate::search_filters::parse_since_filter;
use crate::transcript::{normalize_uuid_prefix, shell_quote_arg};
use clap::{error::ErrorKind, Command, CommandFactory, Parser};
use std::panic;

#[test]
fn shell_quote_arg_uses_single_quotes_for_shell_metacharacters() {
    assert_eq!(shell_quote_arg("onboarding"), "onboarding");
    assert_eq!(
        shell_quote_arg("$(touch /tmp/ctx-owned)'s"),
        "'$(touch /tmp/ctx-owned)'\\''s'"
    );
}

#[test]
fn parse_since_filter_rejects_large_day_window() {
    let err = parse_since_filter("500000000d").unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("invalid --since day window"),
        "expected error about invalid day window, got: {msg}"
    );
}

#[test]
fn cli_value_parsers_do_not_panic_on_adversarial_inputs() {
    let inputs = [
        "",
        " ",
        "0",
        "-1",
        "1",
        "30d",
        "500000000d",
        "9223372036854775807d",
        "-9223372036854775808d",
        "999999999999999999999999999999d",
        "NaN",
        "inf",
        "1e309",
        "1.5d",
        "1970-01-01T00:00:00Z",
        "999999-99-99T99:99:99Z",
        "zzzzzzzz",
        "ffffffff",
        "ffffffff-ffff-ffff-ffff-ffffffffffff",
        "\0",
        "１２３",
    ];

    for input in inputs {
        assert!(
            panic::catch_unwind(|| parse_since_filter(input)).is_ok(),
            "parse_since_filter panicked for {input:?}"
        );
        assert!(
            panic::catch_unwind(|| parse_search_limit(input)).is_ok(),
            "parse_search_limit panicked for {input:?}"
        );
        assert!(
            panic::catch_unwind(|| parse_event_window_limit(input)).is_ok(),
            "parse_event_window_limit panicked for {input:?}"
        );
        assert!(
            panic::catch_unwind(|| parse_sql_timeout(input)).is_ok(),
            "parse_sql_timeout panicked for {input:?}"
        );
        assert!(
            panic::catch_unwind(|| normalize_uuid_prefix(input, "test")).is_ok(),
            "normalize_uuid_prefix panicked for {input:?}"
        );
        assert!(
            panic::catch_unwind(|| parse_referral_codename(input)).is_ok(),
            "parse_referral_codename panicked for {input:?}"
        );
    }
}

#[test]
fn foreground_analytics_eligibility_is_closed_and_remote_safe() {
    for args in [
        vec![
            "ctx",
            "import",
            "--provider",
            "codex",
            "--path",
            "/tmp/history.jsonl",
            "--no-daemon",
        ],
        vec!["ctx", "status", "--json"],
        vec!["ctx", "index", "status", "--json"],
        vec!["ctx", "sql", "select 1"],
        vec!["ctx", "doctor"],
        vec!["ctx", "show", "event", "deadbeef"],
        vec!["ctx", "locate", "event", "deadbeef"],
    ] {
        let cli = Cli::try_parse_from(args).unwrap();
        assert!(
            crate::analytics::ClientOperationDraft::from_command(&cli.command, false).is_some(),
            "expected typed foreground telemetry for {cli:?}"
        );
    }

    for args in [
        vec!["ctx", "daemon", "disable", "--json"],
        vec!["ctx", "mcp", "serve"],
        vec!["ctx", "pro", "--json"],
        vec!["ctx", "referral", "status", "--json"],
        vec!["ctx", "blame", "commit", "deadbeef"],
    ] {
        let cli = Cli::try_parse_from(args).unwrap();
        assert!(
            crate::analytics::ClientOperationDraft::from_command(&cli.command, false).is_none(),
            "follow-on surface must not use the foreground CLI producer: {cli:?}"
        );
    }
}

#[test]
fn bare_pro_accepts_referral_and_json_as_independent_output_and_attribution_flags() {
    let cli = Cli::try_parse_from(["ctx", "pro", "--referral", "agent-smith", "--json"]).unwrap();
    let crate::cli::CommandRoot::Pro(args) = cli.command else {
        panic!("expected Pro command");
    };
    assert!(args.json_output());
}

#[test]
fn deprecated_control_warnings_are_limited_to_foreground_text_commands() {
    for args in [
        vec!["ctx", "status", "--json"],
        vec!["ctx", "mcp", "serve"],
        vec!["ctx", "daemon", "status"],
        vec!["ctx", "setup", "--progress", "json"],
        vec!["ctx", "import", "--progress", "json"],
        vec!["ctx", "doctor", "--progress", "json"],
    ] {
        let cli = Cli::try_parse_from(args).unwrap();
        assert!(!crate::dispatch::command_deprecation_warning_eligible(
            &cli.command
        ));
    }

    for args in [
        vec!["ctx", "status"],
        vec!["ctx", "setup", "--progress", "plain"],
        vec!["ctx", "doctor", "--progress", "none"],
    ] {
        let cli = Cli::try_parse_from(args).unwrap();
        assert!(crate::dispatch::command_deprecation_warning_eligible(
            &cli.command
        ));
    }
}

#[test]
fn complete_cli_grammar_renders_and_parses_help_recursively() {
    fn collect_paths(command: &Command, prefix: &[String], paths: &mut Vec<Vec<String>>) {
        paths.push(prefix.to_vec());
        for subcommand in command.get_subcommands() {
            let mut path = prefix.to_vec();
            path.push(subcommand.get_name().to_owned());
            collect_paths(subcommand, &path, paths);
        }
    }

    Cli::command().debug_assert();
    let command = Cli::command();
    let mut paths = Vec::new();
    collect_paths(&command, &[], &mut paths);
    assert!(
        paths.len() > 50,
        "unexpectedly shallow CLI grammar: {paths:?}"
    );

    for path in paths {
        let mut argv = vec!["ctx".to_owned()];
        argv.extend(path.iter().cloned());
        argv.push("--help".to_owned());
        let error = Cli::try_parse_from(argv).unwrap_err();
        assert_eq!(
            error.kind(),
            ErrorKind::DisplayHelp,
            "help failed for {path:?}"
        );
        let help = error.to_string();
        assert!(
            help.contains("Usage:"),
            "missing usage for {path:?}:\n{help}"
        );
    }
}
