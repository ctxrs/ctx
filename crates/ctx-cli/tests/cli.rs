use assert_cmd::Command;
use predicates::prelude::*;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tempfile::{Builder, TempDir};

fn tempdir() -> TempDir {
    Builder::new().prefix("ctx-search-mvp-").tempdir().unwrap()
}

fn ctx(temp: &TempDir) -> Command {
    let mut command = Command::cargo_bin("ctx").unwrap();
    command.env("CTX_DATA_ROOT", temp.path());
    command.env("HOME", temp.path());
    command.env("CTX_ANALYTICS_OFF", "1");
    command
}

fn provider_history_fixture(name: &str) -> String {
    materialized_fixture("provider-history", name)
}

fn redaction_fixture(name: &str) -> String {
    materialized_fixture("redaction", name)
}

fn materialized_fixture(category: &str, name: &str) -> String {
    let source = match category {
        "provider-history" => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/provider-history")
            .join(name),
        "provider" => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/provider")
            .join(name),
        "redaction" => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/redaction")
            .join(name),
        _ => panic!("unknown fixture category {category}"),
    };
    let materialized_root = std::env::current_dir()
        .unwrap()
        .join("target/test-data/materialized-fixtures");
    fs::create_dir_all(&materialized_root).unwrap();
    let unique = format!(
        "{}-{}-{}-{}",
        category,
        name.replace(['/', '\\', '.'], "_"),
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let mut target = materialized_root.join(unique);
    if source.is_file() {
        if let Some(extension) = source.extension() {
            target.set_extension(extension);
        }
    }
    if source.is_dir() {
        copy_dir_all(&source, &target);
    } else {
        fs::copy(&source, &target).unwrap();
    }
    target.to_str().unwrap().to_owned()
}

fn copy_dir_all(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let entry_path = entry.path();
        let target = to.join(entry.file_name());
        if entry_path.is_dir() {
            copy_dir_all(&entry_path, &target);
        } else {
            fs::copy(entry_path, target).unwrap();
        }
    }
}

fn file_url(path: &Path) -> String {
    format!("file://{}", path.display())
}

fn json_output(command: &mut Command) -> Value {
    let output = command.assert().success().get_output().stdout.clone();
    serde_json::from_slice(&output).unwrap()
}

fn assert_omits_keys(value: &Value, forbidden_keys: &[&str]) {
    match value {
        Value::Object(map) => {
            for key in forbidden_keys {
                assert!(
                    !map.contains_key(*key),
                    "forbidden JSON key {key} appeared in {value:#}"
                );
            }
            for nested in map.values() {
                assert_omits_keys(nested, forbidden_keys);
            }
        }
        Value::Array(items) => {
            for item in items {
                assert_omits_keys(item, forbidden_keys);
            }
        }
        _ => {}
    }
}

fn assert_omits_sensitive_markers(label: &str, value: &str) {
    for forbidden in [
        "sk-fake00000000000000000000000000000000000000000000",
        "ghp_fake000000000000000000000000000000000000",
        "AKIAFAKE000000000000",
        "fake.jwt.token",
        "fake_password",
        "fake_secret_value",
        "fake-password-123",
        "fake_token@git.example.com",
        "person@example.invalid",
    ] {
        assert!(
            !value.contains(forbidden),
            "{label} leaked sensitive marker {forbidden} in {value}"
        );
    }
}

fn sqlite_column_text(conn: &Connection, sql: &str) -> String {
    let mut statement = conn.prepare(sql).unwrap();
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap();
    let mut text = String::new();
    for row in rows {
        text.push_str(&row.unwrap());
        text.push('\n');
    }
    text
}

fn sqlite_count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |row| row.get(0)).unwrap()
}

fn assert_search_provider_oracle(
    packet: &Value,
    provider: &str,
    query: &str,
    expected_results: usize,
    expected_match_reason: &str,
) {
    assert_eq!(packet["schema_version"], 1);
    assert_eq!(packet["query"], query);
    assert_eq!(packet["filters"]["provider"], provider);
    let results = packet["results"].as_array().unwrap();
    assert_eq!(
        results.len(),
        expected_results,
        "unexpected search result count in {packet:#}"
    );

    for result in results {
        assert_eq!(result["provider"], provider, "provider filter failed");
        assert_eq!(result["source_exists"], true, "source_exists failed");
        assert_eq!(result["item_type"], "event");
        assert!(result["ctx_event_id"].is_string());
        assert!(result["ctx_session_id"].is_string());
        assert!(result["provider_session_id"].is_string());
        assert!(result["source_path"].is_string());
        assert!(result["cursor"].is_string());
        assert_suggested_next_commands(result);
        assert!(result["why_matched"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason == expected_match_reason));
        assert_provider_citations(result, provider);
    }
}

fn assert_provider_citations(result: &Value, provider: &str) {
    let citations = result["citations"].as_array().unwrap();
    assert!(!citations.is_empty(), "missing citations in {result:#}");
    for citation in citations {
        assert!(
            citation["ctx_event_id"].is_string() || citation["ctx_session_id"].is_string(),
            "citation needs a ctx-owned event or session id in {citation:#}"
        );
        assert_eq!(citation["provider"], provider, "citation provider failed");
        assert_eq!(
            citation["source_exists"], true,
            "citation source_exists failed"
        );
        assert!(citation["source_path"].is_string());
        assert!(citation["cursor"].is_string());
    }
}

fn assert_suggested_next_commands(result: &Value) {
    let commands = result["suggested_next_commands"].as_array().unwrap();
    assert!(
        commands.iter().any(|command| command
            .as_str()
            .unwrap_or("")
            .starts_with("ctx show event ")),
        "missing show event suggestion in {result:#}"
    );
    assert!(
        commands.iter().any(|command| command
            .as_str()
            .unwrap_or("")
            .starts_with("ctx show session ")),
        "missing show session suggestion in {result:#}"
    );
    assert!(
        commands.iter().any(|command| command
            .as_str()
            .unwrap_or("")
            .starts_with("ctx locate event ")),
        "missing locate event suggestion in {result:#}"
    );
    assert!(
        commands.iter().any(|command| command
            .as_str()
            .unwrap_or("")
            .starts_with("ctx export session ")),
        "missing export session suggestion in {result:#}"
    );
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
        "setup", "status", "sources", "import", "list", "show", "search", "locate", "export",
        "doctor", "validate",
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
fn setup_does_not_migrate_legacy_shim_directory() {
    let temp = tempdir();
    let legacy_shims = temp.path().join("legacy-history").join("shims");
    fs::create_dir_all(&legacy_shims).unwrap();
    fs::write(legacy_shims.join("git"), "#!/bin/sh\n").unwrap();

    ctx(&temp).arg("setup").assert().success();

    assert!(
        !temp.path().join("shims").exists(),
        "setup must not create or migrate shim directories"
    );
    assert!(
        legacy_shims.join("git").exists(),
        "legacy shim files should be left in place instead of installed"
    );
}

#[test]
fn setup_writes_day_one_config_contract_without_overwriting_existing_config() {
    let temp = tempdir();
    let config_path = temp.path().join("config.toml");

    ctx(&temp).arg("setup").assert().success();
    assert_eq!(fs::read_to_string(&config_path).unwrap(), "");

    let user_config = "# user managed ctx config\n[analytics]\nenabled = false\n";
    fs::write(&config_path, user_config).unwrap();

    ctx(&temp).arg("setup").assert().success();
    assert_eq!(
        fs::read_to_string(&config_path).unwrap(),
        user_config,
        "setup must not overwrite an existing user config"
    );
}

#[test]
fn setup_catalog_only_catalogs_codex_sessions_without_import() {
    let temp = tempdir();
    let sessions = temp
        .path()
        .join(".codex")
        .join("sessions")
        .join("2026/06/24");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("rollout-2026-06-24T10-00-00-codex-session-setup.jsonl"),
        r#"{"timestamp":"2026-06-24T10:00:00.000Z","type":"session_meta","payload":{"id":"codex-session-setup","timestamp":"2026-06-24T10:00:00.000Z","cwd":"/repo/app","originator":"codex-cli","cli_version":"0.200.0","source":"cli","model_provider":"openai"}}"#,
    )
    .unwrap();

    let setup = json_output(ctx(&temp).args(["setup", "--catalog-only", "--json"]));
    assert_eq!(setup["catalog"]["cataloged_sessions"], 1);
    assert_eq!(setup["catalog"]["source_files"], 1);
    assert_eq!(setup["catalog"]["failed_sessions"], 0);
    assert_eq!(setup["import"]["ran"], false);

    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(status["cataloged_sessions"], 1);
    assert_eq!(status["indexed_catalog_sessions"], 0);
    assert_eq!(status["indexed_items"], 0);

    let human_setup = ctx(&temp)
        .args(["setup", "--catalog-only", "--progress", "none"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let human_setup = String::from_utf8(human_setup).unwrap();
    assert!(human_setup.contains("ctx catalog is ready; import is still pending"));
    assert!(human_setup.contains("  ctx import --all"));
    assert!(!human_setup.contains("ctx search \"what failed before\""));
}

#[test]
fn setup_imports_discovered_codex_sessions_by_default() {
    let temp = tempdir();
    let sessions = temp
        .path()
        .join(".codex")
        .join("sessions")
        .join("2026/06/24");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("rollout-2026-06-24T10-00-00-codex-session-setup.jsonl"),
        concat!(
            r#"{"timestamp":"2026-06-24T10:00:00.000Z","type":"session_meta","payload":{"id":"codex-session-setup","timestamp":"2026-06-24T10:00:00.000Z","cwd":"/repo/app","originator":"codex-cli","cli_version":"0.200.0","source":"cli","model_provider":"openai"}}"#,
            "\n",
            r#"{"timestamp":"2026-06-24T10:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"setup should import"}]}}"#,
            "\n"
        ),
    )
    .unwrap();

    let setup = json_output(ctx(&temp).args(["setup", "--json", "--progress", "none"]));
    assert_eq!(setup["catalog"]["cataloged_sessions"], 1);
    assert_eq!(setup["import"]["ran"], true);
    assert_eq!(setup["import"]["totals"]["failed_sources"], 0);
    assert_eq!(setup["import"]["totals"]["imported_sessions"], 1);
    assert!(
        setup["import"]["totals"]["imported_events"]
            .as_u64()
            .unwrap()
            >= 1
    );

    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(status["cataloged_sessions"], 1);
    assert_eq!(status["indexed_catalog_sessions"], 1);
    assert_eq!(status["pending_catalog_sessions"], 0);
    assert!(status["indexed_items"].as_u64().unwrap() > 0);

    let human_setup = ctx(&temp)
        .args(["setup", "--progress", "none"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let human_setup = String::from_utf8(human_setup).unwrap();
    assert!(human_setup.contains("ctx local agent history search is ready"));
    assert!(human_setup.contains("imported_sources: 1"));
    assert!(human_setup.contains("  ctx search \"what failed before\""));
}

#[test]
fn setup_skips_empty_codex_session_tree() {
    let temp = tempdir();
    fs::create_dir_all(temp.path().join(".codex").join("sessions")).unwrap();

    let setup = json_output(ctx(&temp).args(["setup", "--json", "--progress", "none"]));
    assert_eq!(setup["catalog"]["cataloged_sessions"], 0);
    assert_eq!(setup["catalog"]["source_files"], 0);
    assert_eq!(setup["import"]["totals"]["imported_sources"], 0);

    let sources = json_output(ctx(&temp).args(["sources", "--json"]));
    let codex_sessions = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| {
            source["provider"] == "codex" && source["source_format"] == "codex_session_jsonl_tree"
        })
        .unwrap();
    assert_eq!(codex_sessions["status"], "empty");
    assert_eq!(codex_sessions["importable"], false);
}

#[test]
fn import_progress_json_goes_to_stderr_without_polluting_stdout() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");
    let output = ctx(&temp)
        .args([
            "import",
            "--provider",
            "codex",
            "--path",
            &fixture,
            "--json",
            "--progress",
            "json",
        ])
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["schema_version"], 1);
    assert!(stdout["totals"]["imported_sessions"].as_u64().unwrap() > 0);

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains(r#""type":"ctx_progress""#), "{stderr}");
    assert!(stderr.contains(r#""operation":"import""#), "{stderr}");
}

#[test]
fn import_all_discovers_and_imports_providers_together() {
    let temp = tempdir();
    copy_dir_all(
        Path::new(&provider_history_fixture("codex-sessions")),
        &temp.path().join(".codex").join("sessions"),
    );
    let pi_home = temp.path().join(".pi");
    fs::create_dir_all(&pi_home).unwrap();
    fs::copy(
        provider_history_fixture("pi-session.jsonl"),
        pi_home.join("sessions.jsonl"),
    )
    .unwrap();

    let output = ctx(&temp)
        .args(["import", "--all", "--json", "--progress", "json"])
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["schema_version"], 1);
    assert!(stdout["totals"]["imported_sessions"].as_u64().unwrap() >= 3);
    let sources = stdout["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 2);
    assert!(sources.iter().any(|source| source["provider"] == "codex"));
    assert!(sources.iter().any(|source| source["provider"] == "pi"));

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains(r#""type":"ctx_progress""#), "{stderr}");
    assert!(stderr.contains(r#""phase":"finalizing""#), "{stderr}");
}

#[test]
fn import_all_skips_empty_gemini_source() {
    let temp = tempdir();
    copy_dir_all(
        Path::new(&provider_history_fixture("codex-sessions")),
        &temp.path().join(".codex").join("sessions"),
    );
    fs::create_dir_all(temp.path().join(".gemini")).unwrap();

    let sources = json_output(ctx(&temp).args(["sources", "--json"]));
    let gemini = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["provider"] == "gemini")
        .unwrap();
    assert_eq!(gemini["status"], "empty");
    assert_eq!(gemini["native_import"], true);
    assert_eq!(gemini["importable"], false);

    let imported =
        json_output(ctx(&temp).args(["import", "--all", "--json", "--progress", "none"]));
    assert_eq!(imported["totals"]["imported_sources"], 1);
    assert_eq!(imported["totals"]["failed_sources"], 0);
    assert!(imported["sources"]
        .as_array()
        .unwrap()
        .iter()
        .all(|source| source["provider"] != "gemini"));
}

#[test]
fn import_all_reports_source_failure_without_losing_successes() {
    let temp = tempdir();
    copy_dir_all(
        Path::new(&provider_history_fixture("codex-sessions")),
        &temp.path().join(".codex").join("sessions"),
    );
    let opencode_dir = temp.path().join(".local/share/opencode");
    fs::create_dir_all(&opencode_dir).unwrap();
    fs::write(opencode_dir.join("opencode.db"), b"not sqlite").unwrap();

    let output = ctx(&temp)
        .args(["import", "--all", "--json", "--progress", "none"])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["schema_version"], 1);
    assert_eq!(stdout["totals"]["imported_sources"], 1);
    assert_eq!(stdout["totals"]["failed_sources"], 1);
    assert!(stdout["totals"]["imported_sessions"].as_u64().unwrap() > 0);
    let sources = stdout["sources"].as_array().unwrap();
    assert!(sources
        .iter()
        .any(|source| source["provider"] == "codex" && source["status"] == "imported"));
    assert!(sources
        .iter()
        .any(|source| source["provider"] == "opencode" && source["status"] == "failed"));
    let opencode_failure = sources
        .iter()
        .find(|source| source["provider"] == "opencode")
        .unwrap();
    assert!(
        opencode_failure["error"]
            .as_str()
            .unwrap()
            .contains("not a database"),
        "{opencode_failure}"
    );
}

#[test]
fn failed_import_attempt_does_not_count_as_indexed_history() {
    let temp = tempdir();
    let opencode_dir = temp.path().join(".local/share/opencode");
    fs::create_dir_all(&opencode_dir).unwrap();
    fs::write(opencode_dir.join("opencode.db"), b"not sqlite").unwrap();

    ctx(&temp)
        .args(["import", "--all", "--json", "--progress", "none"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("all import sources failed"));

    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(status["indexed_items"], 0);
    assert_eq!(status["indexed_sources"], 0);
}

#[test]
fn provider_help_matches_implemented_importers() {
    let temp = tempdir();
    let output = ctx(&temp)
        .args(["import", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let help = String::from_utf8(output).unwrap();

    for value in [
        "codex",
        "pi",
        "claude",
        "opencode",
        "antigravity",
        "gemini",
        "cursor",
        "copilot-cli",
        "factory-ai-droid",
    ] {
        assert!(help.contains(value), "provider {value} missing in\n{help}");
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
                "[possible values: codex, pi, claude, opencode, antigravity, gemini, cursor, copilot-cli, factory-ai-droid]",
                "--path <PATH>",
                "--resume",
                "--json",
            ],
        ),
        ("list", vec!["Usage: ctx list", "--limit <LIMIT>", "--json"]),
        ("show", vec!["Usage: ctx show", "session", "event"]),
        ("locate", vec!["Usage: ctx locate", "session", "event"]),
        (
            "export",
            vec!["Usage: ctx export", "session"],
        ),
        (
            "search",
            vec![
                "Usage: ctx search",
                "[QUERY]",
                "--provider <PROVIDER>",
                "--repo <REPO>",
                "--since <SINCE>",
                "--primary-only",
                "--include-subagents",
                "--event-type <EVENT_TYPE>",
                "--file <FILE>",
                "--limit <LIMIT>",
                "Maximum results to return, from 1 to 200",
                "--refresh <REFRESH>",
                "Pre-search refresh behavior. auto best-effort refreshes",
                "--json",
            ],
        ),
        ("doctor", vec!["Usage: ctx doctor", "--json"]),
        ("validate", vec!["Usage: ctx validate", "--json"]),
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
fn provider_session_lookup_requires_explicit_provider_flags_in_help() {
    let temp = tempdir();
    for args in [
        vec!["show", "session", "--help"],
        vec!["locate", "session", "--help"],
        vec!["locate", "event", "--help"],
        vec!["export", "session", "--help"],
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
    }
}

#[test]
fn analytics_sends_coarse_cli_metadata_when_enabled() {
    let temp = tempdir();
    let events_path = temp.path().join("analytics.jsonl");

    ctx(&temp)
        .arg("status")
        .env_remove("CTX_ANALYTICS_OFF")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .assert()
        .success();

    let body = fs::read_to_string(&events_path).unwrap();
    let event: Value = serde_json::from_str(body.lines().next().unwrap()).unwrap();
    assert_eq!(event["broker_runtime"], "cli");
    assert_eq!(event["events"][0]["event_name"], "cli_invocation");
    assert_eq!(event["events"][0]["origin_runtime"], "cli");
    assert_eq!(event["events"][0]["surface"], "cli");
    assert_eq!(event["events"][0]["properties"]["action"], "status");
    assert_eq!(
        event["events"][0]["properties"]["analytics_client"],
        "ctx-cli"
    );
    assert_eq!(event["events"][0]["properties"]["initialized"], false);
    assert_eq!(
        event["events"][0]["properties"]["indexed_items_bucket"],
        "0"
    );
    assert_eq!(
        event["events"][0]["properties"]["cataloged_sessions_bucket"],
        "0"
    );
    for forbidden in [
        "command",
        "query",
        "query_text",
        "path",
        "file_path",
        "repo",
        "repo_name",
        "branch",
        "error",
        "error_message",
        "session_id",
        "item_id",
    ] {
        assert!(
            event["events"][0]["properties"].get(forbidden).is_none(),
            "analytics leaked forbidden property {forbidden}: {event:#}"
        );
    }
}

#[test]
fn analytics_config_opt_out_suppresses_delivery() {
    let temp = tempdir();
    fs::write(
        temp.path().join("config.toml"),
        "[analytics]\nenabled = false\n",
    )
    .unwrap();
    let events_path = temp.path().join("analytics.jsonl");

    ctx(&temp)
        .arg("status")
        .env_remove("CTX_ANALYTICS_OFF")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .assert()
        .success();

    assert!(
        !events_path.exists(),
        "analytics endpoint should not be touched"
    );
}

#[test]
fn context_command_is_removed() {
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
    assert!(
        !commands.contains("context"),
        "removed context command appeared in root help\n{root_help}"
    );

    ctx(&temp)
        .args(["context", "onboarding", "--json"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("unrecognized subcommand")
                .and(predicate::str::contains("context")),
        );
}

#[test]
fn fresh_home_search_mvp_flow() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");

    ctx(&temp)
        .arg("setup")
        .assert()
        .success()
        .stdout(predicate::str::contains("no local history was indexed"));

    let setup_json = json_output(ctx(&temp).args(["setup", "--json"]));
    assert_eq!(setup_json["schema_version"], 1);
    assert_eq!(setup_json["network_required"], false);
    assert_eq!(setup_json["repo_writes"], false);
    assert_eq!(setup_json["import"]["ran"], true);

    let sources = json_output(ctx(&temp).args(["sources", "--json"]));
    assert_eq!(sources["schema_version"], 1);
    assert!(sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|source| source["provider"] == "codex"));

    let import = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--json",
    ]));
    assert_eq!(import["schema_version"], 1);
    assert!(import["totals"]["imported_sessions"].as_u64().unwrap() > 0);
    assert!(import["totals"]["source_files"].as_u64().unwrap() > 0);
    assert!(import["totals"]["source_bytes"].as_u64().unwrap() > 0);

    let mut list_command = ctx(&temp);
    list_command.args(["list", "--json"]);
    let listed = json_output(&mut list_command);
    assert_eq!(listed["schema_version"], 1);
    assert_omits_keys(&listed, &["record_id", "history_record_id", "kind"]);
    assert_eq!(listed["items"][0]["item_type"], "session");
    assert!(listed["items"][0]["ctx_session_id"].is_string());
    assert!(listed["items"][0]["provider_session_id"].is_string());
    assert!(listed["items"][0]["item_id"].is_string());
    assert_eq!(listed["items"][0]["id"], listed["items"][0]["item_id"]);

    let search =
        json_output(ctx(&temp).args(["search", "onboarding", "--provider", "codex", "--json"]));
    assert_eq!(search["schema_version"], 1);
    assert_eq!(search["share_safe"], false);
    assert_omits_keys(
        &search,
        &[
            "record_id",
            "history_record_id",
            "raw_source_path",
            "kind",
            "external_session_id",
        ],
    );
    let first_result = &search["results"][0];
    assert_eq!(first_result["item_type"], "event");
    let ctx_event_id = first_result["ctx_event_id"].as_str().unwrap().to_owned();
    let ctx_session_id = first_result["ctx_session_id"].as_str().unwrap().to_owned();
    assert!(first_result["provider_session_id"].is_string());
    assert!(first_result["source_path"].is_string());
    assert!(first_result["cursor"].is_string());
    assert_suggested_next_commands(first_result);
    assert!(first_result["citations"][0]["ctx_event_id"].is_string());
    assert!(first_result["citations"][0]["ctx_session_id"].is_string());

    let human_search = ctx(&temp)
        .args(["search", "onboarding"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let human_search = String::from_utf8(human_search).unwrap();
    assert!(human_search.contains("ctx_event_id"));
    assert!(human_search.contains("ctx_session_id"));
    assert!(human_search.contains("provider_session_id"));
    assert!(human_search.contains("next: ctx show event"));
    assert!(human_search.contains("next: ctx locate event"));
    assert!(human_search.contains("next: ctx show session"));
    assert!(!human_search.contains("work_record"));
    assert!(!human_search.contains("history_record"));

    let file_search =
        json_output(ctx(&temp).args(["search", "--file", "crates/foo/src/lib.rs", "--json"]));
    assert_eq!(file_search["query"], "");
    assert!(file_search["results"].is_array());

    let show_event = json_output(ctx(&temp).args([
        "show",
        "event",
        &ctx_event_id,
        "--window",
        "2",
        "--format",
        "json",
    ]));
    assert_eq!(show_event["schema_version"], 1);
    assert_eq!(show_event["item_type"], "event_window");
    assert_eq!(show_event["event"]["ctx_event_id"], ctx_event_id);
    assert_eq!(show_event["event"]["ctx_session_id"], ctx_session_id);
    assert_omits_keys(
        &show_event,
        &[
            "record_id",
            "history_record_id",
            "kind",
            "payload",
            "payload_blob_id",
            "dedupe_key",
            "capture_source_id",
        ],
    );
    assert!(show_event["events"]
        .as_array()
        .unwrap()
        .iter()
        .all(|event| event["ctx_event_id"].is_string()
            && event["ctx_session_id"].is_string()
            && event["preview"].is_string()));

    let show_session = json_output(ctx(&temp).args([
        "show",
        "session",
        &ctx_session_id,
        "--mode",
        "lite",
        "--format",
        "json",
    ]));
    assert_eq!(show_session["schema_version"], 1);
    assert_eq!(show_session["item_type"], "session_transcript");
    assert_eq!(show_session["session"]["item_type"], "session");
    assert_eq!(show_session["session"]["item_id"], ctx_session_id);
    assert_eq!(show_session["mode"], "lite");

    let locate_event = json_output(ctx(&temp).args(["locate", "event", &ctx_event_id, "--json"]));
    assert_eq!(locate_event["schema_version"], 1);
    assert_eq!(locate_event["item_type"], "event_location");
    assert_eq!(locate_event["ctx_event_id"], ctx_event_id);
    assert_eq!(locate_event["ctx_session_id"], ctx_session_id);
    assert_eq!(locate_event["provider"], "codex");
    assert!(locate_event["provider_session_id"].is_string());
    assert!(locate_event["source"]["path"].is_string());
    assert!(locate_event["cursor"].is_string());

    let export_path = temp.path().join("transcript.md");
    ctx(&temp)
        .args([
            "export",
            "session",
            &ctx_session_id,
            "--mode",
            "lite",
            "--format",
            "markdown",
            "--out",
            export_path.to_str().unwrap(),
        ])
        .assert()
        .success();
    assert!(
        export_path.exists(),
        "export session should write the requested artifact path"
    );

    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(status["schema_version"], 1);
    assert!(status["indexed_items"].as_u64().unwrap() > 0);

    let doctor = json_output(ctx(&temp).args(["doctor", "--json"]));
    assert_eq!(doctor["schema_version"], 1);
    assert_eq!(doctor["ok"], true);

    let validate = json_output(ctx(&temp).args(["validate", "--json"]));
    assert_eq!(validate["schema_version"], 1);
    assert_eq!(validate["valid"], true);
}

#[test]
fn codex_cli_resume_is_idempotent_rescan_and_filters_subagents() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");

    let first = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--json",
    ]));
    assert_eq!(first["schema_version"], 1);
    assert_eq!(first["resume"], false);
    assert_eq!(first["resume_mode"], "normal_scan");
    assert_eq!(first["totals"]["imported_sessions"], 2);
    assert_eq!(first["totals"]["imported_events"], 4);
    assert_eq!(first["totals"]["imported_edges"], 1);

    let with_subagents = json_output(ctx(&temp).args(["search", "subagent", "--json"]));
    assert!(!with_subagents["results"].as_array().unwrap().is_empty());
    assert_eq!(with_subagents["filters"]["include_subagents"], true);

    let primary_only =
        json_output(ctx(&temp).args(["search", "subagent", "--primary-only", "--json"]));
    assert_eq!(primary_only["filters"]["include_subagents"], false);
    assert_eq!(primary_only["filters"]["primary_only"], true);
    assert!(
        primary_only["results"].as_array().unwrap().len()
            <= with_subagents["results"].as_array().unwrap().len()
    );

    let second = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--resume",
        "--json",
    ]));
    assert_eq!(second["schema_version"], 1);
    assert_eq!(second["resume"], true);
    assert_eq!(second["resume_mode"], "idempotent_rescan");
    assert_eq!(second["totals"]["imported_sessions"], 0);
    assert_eq!(second["totals"]["imported_events"], 0);
    assert_eq!(second["totals"]["imported_edges"], 0);
    assert!(second["totals"]["skipped"].as_u64().unwrap() > 0);
    assert_eq!(second["sources"][0]["imported_sessions"], 0);
    assert_eq!(second["sources"][0]["imported_events"], 0);
}

#[test]
fn search_refreshes_discovered_codex_sessions_before_query() {
    let temp = tempdir();
    let fixture = PathBuf::from(provider_history_fixture("codex-sessions"));
    let discovered = temp.path().join(".codex").join("sessions");
    copy_dir_all(&fixture, &discovered);

    let search =
        json_output(ctx(&temp).args(["search", "onboarding", "--provider", "codex", "--json"]));
    assert_search_provider_oracle(&search, "codex", "onboarding", 1, "message");
    assert_eq!(search["freshness"]["mode"], "auto");
    assert_eq!(search["freshness"]["status"], "completed");
    assert_eq!(search["freshness"]["source_count"], 1);
    assert_eq!(search["freshness"]["totals"]["imported_sessions"], 2);

    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(status["cataloged_sessions"], 2);
    assert_eq!(status["indexed_catalog_sessions"], 2);
    assert_eq!(status["pending_catalog_sessions"], 0);
}

#[test]
fn search_refresh_off_serves_existing_index_without_importing() {
    let temp = tempdir();
    let fixture = PathBuf::from(provider_history_fixture("codex-sessions"));
    let discovered = temp.path().join(".codex").join("sessions");
    copy_dir_all(&fixture, &discovered);

    let stale = json_output(ctx(&temp).args([
        "search",
        "onboarding",
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--json",
    ]));
    assert_eq!(stale["freshness"]["mode"], "off");
    assert_eq!(stale["freshness"]["status"], "skipped");
    assert!(stale["results"].as_array().unwrap().is_empty());

    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(status["cataloged_sessions"], 0);
    assert_eq!(status["indexed_catalog_sessions"], 0);

    let fresh =
        json_output(ctx(&temp).args(["search", "onboarding", "--provider", "codex", "--json"]));
    assert_search_provider_oracle(&fresh, "codex", "onboarding", 1, "message");
}

#[test]
fn search_refresh_auto_imports_fresh_work_despite_large_existing_catalog() {
    let temp = tempdir();
    let fixture = PathBuf::from(provider_history_fixture("codex-sessions"));
    let discovered = temp.path().join(".codex").join("sessions");
    copy_dir_all(&fixture, &discovered);
    let _ = json_output(ctx(&temp).args(["search", "anything", "--refresh", "off", "--json"]));

    let mut conn = Connection::open(temp.path().join("work.sqlite")).unwrap();
    let tx = conn.transaction().unwrap();
    {
        let mut stmt = tx
            .prepare(
                "INSERT INTO catalog_sessions (
                    source_path, provider, source_format, source_root,
                    external_session_id, agent_type, file_size_bytes,
                    file_modified_at_ms, cataloged_at_ms, indexed_status,
                    indexed_at_ms, indexed_file_size_bytes,
                    indexed_file_modified_at_ms, metadata_json
                ) VALUES (?1, 'codex', 'codex_session_jsonl_tree', ?2, ?3,
                    'primary', 2, 1782259200000, 1782259200000, 'indexed',
                    1782259200000, 2, 1782259200000, '{}')",
            )
            .unwrap();
        for index in 0..10_000 {
            stmt.execute(params![
                format!("{}/seed-{index:05}.jsonl", discovered.display()),
                discovered.display().to_string(),
                format!("large-catalog-session-{index:05}"),
            ])
            .unwrap();
        }
    }
    tx.commit().unwrap();
    let search =
        json_output(ctx(&temp).args(["search", "onboarding", "--provider", "codex", "--json"]));
    assert_eq!(search["freshness"]["mode"], "auto");
    assert_eq!(search["freshness"]["status"], "completed");
    assert_eq!(search["freshness"]["source_count"], 1);
    assert_eq!(search["freshness"]["totals"]["imported_sessions"], 2);
    assert_search_provider_oracle(&search, "codex", "onboarding", 1, "message");

    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(status["pending_catalog_sessions"], 0);
}

#[test]
fn search_refresh_auto_tail_imports_appended_codex_session_event() {
    let temp = tempdir();
    let fixture = PathBuf::from(provider_history_fixture("codex-sessions"));
    let discovered = temp.path().join(".codex").join("sessions");
    copy_dir_all(&fixture, &discovered);
    let root_session = discovered.join("2026/06/23/root.jsonl");
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&root_session)
        .unwrap();
    for index in 0..250 {
        writeln!(
            file,
            "{}",
            json!({
                "timestamp": "2026-06-23T15:00:00.000Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": format!("tail-refresh-baseline-{index}")}]
                }
            })
        )
        .unwrap();
    }
    drop(file);

    let first =
        json_output(ctx(&temp).args(["search", "onboarding", "--provider", "codex", "--json"]));
    assert_search_provider_oracle(&first, "codex", "onboarding", 1, "message");

    let appended_needle = "tail-refresh-append-oracle";
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&root_session)
        .unwrap();
    writeln!(
        file,
        "{}",
        json!({
            "timestamp": "2026-06-23T15:00:30.000Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": appended_needle}]
            }
        })
    )
    .unwrap();

    let started = Instant::now();
    let refreshed =
        json_output(ctx(&temp).args(["search", appended_needle, "--provider", "codex", "--json"]));
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "tail refresh took {elapsed:?}"
    );
    assert_eq!(refreshed["freshness"]["status"], "completed");
    assert_eq!(refreshed["freshness"]["totals"]["imported_events"], 1);
    assert!(
        refreshed["freshness"]["totals"]["skipped"]
            .as_u64()
            .unwrap()
            < 20,
        "tail refresh unexpectedly reprocessed old events: {}",
        refreshed["freshness"]["totals"]
    );
    assert_search_provider_oracle(&refreshed, "codex", appended_needle, 1, "message");
}

#[test]
fn search_refresh_auto_imports_discovered_top_provider_sources() {
    for (cli_provider, stored_provider, install_fixture) in [
        (
            "claude",
            "claude",
            install_default_claude_fixture as fn(&TempDir, &str),
        ),
        ("pi", "pi", install_default_pi_fixture),
        ("cursor", "cursor", install_default_cursor_fixture),
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
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(2),
            "{cli_provider} no-op refresh took {elapsed:?}"
        );
        assert_eq!(refreshed["freshness"]["mode"], "auto");
        assert_eq!(refreshed["freshness"]["status"], "completed");
        assert_eq!(refreshed["freshness"]["totals"]["imported_sessions"], 0);
        assert_eq!(refreshed["freshness"]["totals"]["imported_events"], 0);
        assert_search_provider_oracle(&refreshed, stored_provider, &query, 1, "message");
    }
}

#[test]
fn search_refresh_strict_json_emits_progress_on_stderr() {
    let temp = tempdir();
    let fixture = PathBuf::from(provider_history_fixture("codex-sessions"));
    copy_dir_all(&fixture, &temp.path().join(".codex").join("sessions"));

    let output = ctx(&temp)
        .args([
            "search",
            "onboarding",
            "--provider",
            "codex",
            "--refresh",
            "strict",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["freshness"]["status"], "completed");
    assert_search_provider_oracle(&stdout, "codex", "onboarding", 1, "message");

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains(r#""type":"ctx_progress""#), "{stderr}");
    assert!(
        stderr.contains(r#""operation":"search-refresh""#),
        "{stderr}"
    );
}

#[test]
fn search_refresh_strict_fails_when_no_supported_refresh_source_exists() {
    let temp = tempdir();
    ctx(&temp)
        .args(["search", "anything", "--refresh", "strict", "--json"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "strict search refresh found no supported",
        ));
}

#[test]
fn search_rejects_unbounded_limit() {
    let temp = tempdir();
    ctx(&temp)
        .args(["search", "anything", "--limit", "201"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid value"));
}

#[test]
fn codex_cli_default_import_uses_catalog_state_for_incremental_catch_up() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");

    let first = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--json",
    ]));
    assert_eq!(first["resume"], false);
    assert_eq!(first["resume_mode"], "normal_scan");
    assert_eq!(first["totals"]["imported_sessions"], 2);
    assert_eq!(first["totals"]["imported_events"], 4);
    assert_eq!(first["totals"]["failed"], 0);

    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(status["cataloged_sessions"], 2);
    assert_eq!(status["indexed_catalog_sessions"], 2);
    assert_eq!(status["pending_catalog_sessions"], 0);
    assert_eq!(status["failed_catalog_sessions"], 0);

    let second = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--json",
    ]));
    assert_eq!(second["resume"], false);
    assert_eq!(second["resume_mode"], "normal_scan");
    assert_eq!(second["totals"]["imported_sessions"], 0);
    assert_eq!(second["totals"]["imported_events"], 0);
    assert_eq!(second["totals"]["imported_edges"], 0);
    assert_eq!(second["totals"]["skipped"], 0);
    assert_eq!(second["totals"]["failed"], 0);
}

#[test]
fn codex_cli_provider_oracle_covers_retrieval_and_claimed_fidelity() {
    let temp = tempdir();
    let basic_fixture = provider_history_fixture("codex-sessions");
    let rich_fixture = provider_history_fixture("codex-rich-sessions");

    let basic = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &basic_fixture,
        "--json",
    ]));
    assert_eq!(basic["totals"]["imported_sessions"], 2);
    assert_eq!(basic["totals"]["imported_events"], 4);
    assert_eq!(basic["totals"]["imported_edges"], 1);

    let rich = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &rich_fixture,
        "--json",
    ]));
    assert_eq!(rich["totals"]["imported_sessions"], 1);
    assert_eq!(rich["totals"]["imported_events"], 1);

    let query = "setup flow";
    let search = json_output(ctx(&temp).args(["search", query, "--provider", "codex", "--json"]));
    assert_search_provider_oracle(&search, "codex", query, 1, "message");

    let conn = Connection::open(temp.path().join("work.sqlite")).unwrap();
    assert_eq!(
        sqlite_count(
            &conn,
            "SELECT COUNT(*) FROM sessions WHERE provider = 'codex' AND fidelity = 'imported'"
        ),
        3
    );
    assert_eq!(
        sqlite_count(
            &conn,
            "SELECT COUNT(*) FROM events e JOIN sessions s ON e.session_id = s.id WHERE s.provider = 'codex' AND e.fidelity = 'imported'"
        ),
        5
    );
    assert_eq!(
        sqlite_count(
            &conn,
            "SELECT COUNT(*) FROM events e JOIN sessions s ON e.session_id = s.id WHERE s.provider = 'codex' AND e.event_type = 'message' AND e.role = 'user'"
        ),
        3
    );
    assert_eq!(
        sqlite_count(
            &conn,
            "SELECT COUNT(*) FROM events e JOIN sessions s ON e.session_id = s.id WHERE s.provider = 'codex' AND e.event_type = 'message' AND e.role = 'assistant'"
        ),
        2
    );
    assert_eq!(
        sqlite_count(
            &conn,
            "SELECT COUNT(*) FROM events e JOIN sessions s ON e.session_id = s.id WHERE s.provider = 'codex' AND e.event_type = 'tool_call'"
        ),
        0
    );
    assert_eq!(
        sqlite_count(
            &conn,
            "SELECT COUNT(*) FROM events e JOIN sessions s ON e.session_id = s.id WHERE s.provider = 'codex' AND e.event_type = 'tool_output'"
        ),
        0
    );
    assert_eq!(
        sqlite_count(
            &conn,
            "SELECT COUNT(*) FROM events e JOIN sessions s ON e.session_id = s.id WHERE s.provider = 'codex' AND e.event_type = 'command_output'"
        ),
        0
    );
    assert_eq!(
        sqlite_count(
            &conn,
            "SELECT COUNT(*) FROM sessions WHERE provider = 'codex' AND metadata_json LIKE '%model_provider%'"
        ),
        3
    );
    assert_eq!(
        sqlite_count(
            &conn,
            "SELECT COUNT(*) FROM events e JOIN sessions s ON e.session_id = s.id WHERE s.provider = 'codex' AND e.payload_json LIKE '%token_usage%'"
        ),
        0
    );
    assert_eq!(sqlite_count(&conn, "SELECT COUNT(*) FROM session_edges"), 1);
    assert_eq!(sqlite_count(&conn, "SELECT COUNT(*) FROM artifacts"), 0);
    assert_eq!(sqlite_count(&conn, "SELECT COUNT(*) FROM files_touched"), 0);
}

#[test]
fn pi_cli_import_search_flow() {
    let temp = tempdir();
    let fixture = provider_history_fixture("pi-session.jsonl");

    let imported =
        json_output(ctx(&temp).args(["import", "--provider", "pi", "--path", &fixture, "--json"]));
    assert_eq!(imported["schema_version"], 1);
    assert_eq!(imported["sources"][0]["provider"], "pi");
    assert_eq!(imported["sources"][0]["source_format"], "pi_session_jsonl");
    assert_eq!(imported["totals"]["imported_sessions"], 1);
    assert_eq!(imported["totals"]["imported_events"], 6);

    let search =
        json_output(ctx(&temp).args(["search", "provider metadata", "--provider", "pi", "--json"]));
    assert_search_provider_oracle(&search, "pi", "provider metadata", 1, "message");

    let second = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "pi",
        "--path",
        &fixture,
        "--resume",
        "--json",
    ]));
    assert_eq!(second["resume"], true);
    assert_eq!(second["resume_mode"], "idempotent_rescan");
    assert_eq!(second["totals"]["imported_sessions"], 0);
    assert_eq!(second["totals"]["imported_events"], 0);
    assert_eq!(second["totals"]["skipped"].as_u64().unwrap(), 7);

    let conn = Connection::open(temp.path().join("work.sqlite")).unwrap();
    assert_eq!(
        sqlite_count(
            &conn,
            "SELECT COUNT(*) FROM sessions WHERE provider = 'pi' AND fidelity = 'imported'"
        ),
        1
    );
    assert_eq!(
        sqlite_count(
            &conn,
            "SELECT COUNT(*) FROM events e JOIN sessions s ON e.session_id = s.id WHERE s.provider = 'pi' AND e.fidelity = 'imported'"
        ),
        6
    );
    assert_eq!(
        sqlite_count(
            &conn,
            "SELECT COUNT(*) FROM events e JOIN sessions s ON e.session_id = s.id WHERE s.provider = 'pi' AND e.event_type = 'message' AND e.role = 'user'"
        ),
        1
    );
    assert_eq!(
        sqlite_count(
            &conn,
            "SELECT COUNT(*) FROM events e JOIN sessions s ON e.session_id = s.id WHERE s.provider = 'pi' AND e.event_type = 'message' AND e.role = 'assistant'"
        ),
        1
    );
    assert_eq!(
        sqlite_count(
            &conn,
            "SELECT COUNT(*) FROM events e JOIN sessions s ON e.session_id = s.id WHERE s.provider = 'pi' AND json_type(e.metadata_json, '$.metadata.model') = 'text'"
        ),
        2
    );
    assert_eq!(sqlite_count(&conn, "SELECT COUNT(*) FROM session_edges"), 0);
}

#[test]
fn native_provider_cli_flow_imports_new_supported_provider_paths() {
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
    ] {
        let temp = tempdir();
        let query = format!("{stored_provider}-native-cli-oracle");
        let path = fixture(&temp, &query);

        let imported = json_output(ctx(&temp).args([
            "import",
            "--provider",
            cli_provider,
            "--path",
            &path,
            "--json",
        ]));
        assert_eq!(imported["schema_version"], 1);
        assert_eq!(imported["sources"][0]["provider"], stored_provider);
        assert_eq!(imported["sources"][0]["source_format"], expected_format);
        assert_eq!(imported["totals"]["failed"], 0);
        assert!(imported["totals"]["imported_sessions"].as_u64().unwrap() >= 1);
        assert!(imported["totals"]["imported_events"].as_u64().unwrap() >= 1);

        let search =
            json_output(ctx(&temp).args(["search", &query, "--provider", cli_provider, "--json"]));
        assert_search_provider_oracle(&search, stored_provider, &query, 1, "message");

        let status = json_output(ctx(&temp).args(["status", "--json"]));
        assert!(status["indexed_items"].as_u64().unwrap() >= 2);
        assert!(status["indexed_sources"].as_u64().unwrap() >= 1);

        let validate = json_output(ctx(&temp).args(["validate", "--json"]));
        assert_eq!(validate["valid"], true);
    }
}

fn install_default_claude_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_claude_fixture(temp, query));
    copy_dir_all(&source, &temp.path().join(".claude").join("projects"));
}

fn install_default_pi_fixture(temp: &TempDir, query: &str) {
    let root = temp.path().join(".pi");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("sessions.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "type": "session",
                "version": 3,
                "id": "pi-default-refresh",
                "timestamp": "2026-06-24T12:00:00.000Z",
                "cwd": "/workspace"
            }),
            json!({
                "type": "message",
                "id": "pi-default-refresh-user",
                "timestamp": "2026-06-24T12:00:01.000Z",
                "message": {"role": "user", "content": query}
            })
        ),
    )
    .unwrap();
}

fn install_default_cursor_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_cursor_fixture(temp, query));
    copy_dir_all(&source, &temp.path().join(".cursor").join("projects"));
}

fn write_native_claude_fixture(temp: &TempDir, query: &str) -> String {
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

fn write_native_opencode_fixture(temp: &TempDir, query: &str) -> String {
    let path = temp.path().join("native-opencode.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table session (
            id text primary key,
            project_id text not null,
            parent_id text,
            slug text not null,
            directory text not null,
            title text not null,
            version text not null,
            share_url text,
            summary_additions integer,
            summary_deletions integer,
            summary_files integer,
            summary_diffs text,
            revert text,
            permission text,
            time_created integer not null,
            time_updated integer not null,
            time_compacting integer,
            time_archived integer,
            workspace_id text
        );
        create table message (
            id text primary key,
            session_id text not null,
            time_created integer not null,
            time_updated integer not null,
            data text not null
        );
        create table part (
            id text primary key,
            message_id text not null,
            session_id text not null,
            time_created integer not null,
            time_updated integer not null,
            data text not null
        );",
    )
    .unwrap();
    conn.execute(
        "insert into session (
            id, project_id, parent_id, slug, directory, title, version, permission,
            time_created, time_updated
        ) values (?1, 'project-1', null, 'native', '/workspace', 'native', '0.8.0',
            'default', 1782259200000, 1782259200000)",
        ["opencode-cli-native"],
    )
    .unwrap();
    conn.execute(
        "insert into message values (?1, ?2, 1782259200000, 1782259200000, ?3)",
        [
            "opencode-cli-native-user",
            "opencode-cli-native",
            &format!(r#"{{"role":"user","time":{{"created":1782259200000}},"text":"{query}"}}"#),
        ],
    )
    .unwrap();
    path.to_str().unwrap().to_owned()
}

fn write_native_gemini_fixture(temp: &TempDir, query: &str) -> String {
    let chats = temp.path().join("native-gemini/.gemini/tmp/project/chats");
    fs::create_dir_all(&chats).unwrap();
    fs::write(
        chats.join("session-native.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "sessionId": "gemini-cli-native",
                "startTime": "2026-06-24T12:00:00Z",
                "kind": "main",
                "directories": ["/workspace"]
            }),
            json!({
                "id": "gemini-cli-native-user",
                "timestamp": "2026-06-24T12:00:01Z",
                "type": "user",
                "content": query
            })
        ),
    )
    .unwrap();
    temp.path()
        .join("native-gemini/.gemini")
        .to_str()
        .unwrap()
        .to_owned()
}

fn write_native_cursor_fixture(temp: &TempDir, query: &str) -> String {
    let root = temp
        .path()
        .join("native-cursor/projects/sanitized-workspace/agent-transcripts/cursor-cli-native");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("cursor-cli-native.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "timestamp": "2026-06-24T12:00:00Z",
                "role": "user",
                "message": {"role": "user", "content": [{"type": "text", "text": query}]}
            }),
            json!({
                "timestamp": "2026-06-24T12:00:01Z",
                "role": "assistant",
                "message": {"role": "assistant", "content": [{"type": "text", "text": "native import ok"}]}
            })
        ),
    )
    .unwrap();
    temp.path()
        .join("native-cursor/projects")
        .to_str()
        .unwrap()
        .to_owned()
}

fn write_native_copilot_fixture(temp: &TempDir, query: &str) -> String {
    let root = temp
        .path()
        .join("native-copilot/session-state/copilot-cli-native");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("events.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "id": "copilot-cli-native-start",
                "timestamp": "2026-06-24T12:00:00Z",
                "type": "session.start",
                "data": {
                    "sessionId": "copilot-cli-native",
                    "startTime": "2026-06-24T12:00:00Z",
                    "selectedModel": "gpt-5-mini",
                    "context": {"cwd": "/workspace"}
                }
            }),
            json!({
                "id": "copilot-cli-native-user",
                "timestamp": "2026-06-24T12:00:01Z",
                "type": "user.message",
                "data": {"content": query}
            })
        ),
    )
    .unwrap();
    temp.path()
        .join("native-copilot/session-state")
        .to_str()
        .unwrap()
        .to_owned()
}

fn write_native_factory_droid_fixture(temp: &TempDir, query: &str) -> String {
    let root = temp.path().join("native-droid/sessions/project");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("droid-cli-native.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "type": "session_start",
                "sessionId": "droid-cli-native",
                "timestamp": "2026-06-24T12:00:00Z",
                "cwd": "/workspace",
                "model": "factory/droid"
            }),
            json!({
                "type": "message",
                "id": "droid-cli-native-user",
                "timestamp": "2026-06-24T12:00:01Z",
                "role": "user",
                "content": [{"type": "text", "text": query}]
            })
        ),
    )
    .unwrap();
    temp.path()
        .join("native-droid/sessions")
        .to_str()
        .unwrap()
        .to_owned()
}

#[test]
fn native_provider_cli_requires_existing_history_or_explicit_path() {
    for (cli_provider, expected_blocker) in [
        ("claude", "no native claude history found"),
        ("opencode", "no native opencode history found"),
        ("antigravity", "no native antigravity history found"),
        ("gemini", "no native gemini history found"),
        ("cursor", "no native cursor history found"),
        ("copilot-cli", "no native copilot_cli history found"),
        (
            "factory-ai-droid",
            "no native factory_ai_droid history found",
        ),
    ] {
        let temp = tempdir();
        ctx(&temp)
            .args(["import", "--provider", cli_provider, "--json"])
            .assert()
            .failure()
            .stderr(predicate::str::contains(expected_blocker));
    }
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
        "--json",
    ]));
    assert_eq!(imported["schema_version"], 1);
    assert_eq!(imported["sources"][0]["provider"], "antigravity");
    assert_eq!(
        imported["sources"][0]["source_format"],
        "antigravity_cli_transcript_jsonl_tree"
    );
    assert_eq!(imported["totals"]["imported_sessions"], 4);
    assert_eq!(imported["totals"]["imported_events"], 11);
    assert_eq!(imported["totals"]["failed"], 1);

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
fn codex_cli_reports_malformed_partial_import_progress() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-malformed-session.jsonl");

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
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
fn pi_cli_reports_malformed_partial_and_schema_failures() {
    let temp = tempdir();
    let fixture = provider_history_fixture("pi-malformed-partial.jsonl");

    let imported =
        json_output(ctx(&temp).args(["import", "--provider", "pi", "--path", &fixture, "--json"]));
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
fn human_search_reports_no_results() {
    let temp = tempdir();
    let fresh = ctx(&temp)
        .args(["search", "definitely-no-results-here"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let fresh = String::from_utf8(fresh).unwrap();
    assert!(fresh.contains("no results"));
    assert!(fresh.contains("next: ctx import --all"));

    let fixture = provider_history_fixture("codex-sessions");
    ctx(&temp)
        .args([
            "import",
            "--provider",
            "codex",
            "--path",
            &fixture,
            "--progress",
            "none",
        ])
        .assert()
        .success();
    let indexed = ctx(&temp)
        .args(["search", "definitely-no-results-here"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let indexed = String::from_utf8(indexed).unwrap();
    assert!(indexed.contains("no results"));
    assert!(indexed.contains("next: ctx list --limit 20"));
}

#[test]
fn pi_cli_rejects_directory_import_path() {
    let temp = tempdir();
    let path = temp.path().join("pi-sessions-dir");
    fs::create_dir_all(&path).unwrap();

    ctx(&temp)
        .args([
            "import",
            "--provider",
            "pi",
            "--path",
            path.to_str().unwrap(),
            "--json",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no importable pi history files"));
}

#[test]
fn codex_cli_marks_deleted_raw_source_citations_unavailable() {
    let temp = tempdir();
    let source = PathBuf::from(provider_history_fixture("codex-sessions"));
    let copied = temp.path().join("copied-codex-sessions");
    copy_dir_all(&source, &copied);
    let copied_text = copied.to_str().unwrap().to_owned();

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &copied_text,
        "--json",
    ]));
    assert_eq!(imported["totals"]["imported_events"], 4);

    fs::remove_dir_all(&copied).unwrap();

    let search = json_output(ctx(&temp).args(["search", "onboarding", "--json"]));
    assert!(search["results"]
        .as_array()
        .unwrap()
        .iter()
        .any(|result| result["source_exists"] == false));
    assert!(search["results"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|result| result["citations"].as_array().unwrap().iter())
        .any(|citation| citation["source_exists"] == false));
}

#[test]
fn privacy_redaction_oracle_covers_cli_json_and_sqlite() {
    let temp = tempdir();
    let fixture = redaction_fixture("codex-sessions");

    let import = json_output(
        ctx(&temp)
            .env("CTX_CODEX_TOOL_OUTPUT_MODE", "full")
            .env("CTX_CODEX_EVENT_MODE", "rich")
            .env("CTX_CODEX_INCLUDE_NOTICES", "1")
            .args([
                "import",
                "--provider",
                "codex",
                "--path",
                &fixture,
                "--json",
            ]),
    );
    assert_eq!(import["schema_version"], 1);
    assert_eq!(import["totals"]["failed"], 0);
    assert!(import["totals"]["imported_sessions"].as_u64().unwrap() > 0);

    let search = json_output(ctx(&temp).args(["search", "redaction oracle", "--json"]));
    assert_eq!(search["schema_version"], 1);
    assert_eq!(search["share_safe"], false);
    assert!(!search["results"].as_array().unwrap().is_empty());

    let listed = json_output(ctx(&temp).args(["list", "--json"]));
    let ctx_session_id = listed["items"][0]["ctx_session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let show = json_output(ctx(&temp).args([
        "show",
        "session",
        &ctx_session_id,
        "--mode",
        "log",
        "--format",
        "json",
    ]));
    assert_eq!(show["schema_version"], 1);
    assert!(show["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| event["preview"]
            .as_str()
            .unwrap_or("")
            .contains("[REDACTED")));

    let cli_json = format!("{import}\n{search}\n{show}");
    assert!(cli_json.contains("[REDACTED"));
    assert_omits_sensitive_markers("cli json", &cli_json);

    let conn = Connection::open(temp.path().join("work.sqlite")).unwrap();
    let event_payloads = sqlite_column_text(&conn, "SELECT COALESCE(payload_json, '') FROM events");
    let event_index = sqlite_column_text(
        &conn,
        "SELECT COALESCE(safe_preview_text, '') FROM event_search",
    );
    let record_index = sqlite_column_text(
        &conn,
        "SELECT COALESCE(title, '') || ' ' || COALESCE(summary, '') || ' ' || COALESCE(primary_user_text, '') || ' ' || COALESCE(decision_text, '') || ' ' || COALESCE(context_text, '') || ' ' || COALESCE(tag_text, '') FROM ctx_history_search",
    );
    let sqlite_text = format!("{event_payloads}\n{event_index}\n{record_index}");
    assert!(sqlite_text.contains("[REDACTED"));
    assert!(event_index.contains("[REDACTED_PATH]"));
    assert_omits_sensitive_markers("sqlite indexed output", &sqlite_text);
}
