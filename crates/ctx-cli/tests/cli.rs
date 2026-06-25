use assert_cmd::Command;
use predicates::prelude::*;
use rusqlite::Connection;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};
use tempfile::{Builder, TempDir};

fn tempdir() -> TempDir {
    let root = std::env::current_dir().unwrap().join("target/test-data");
    fs::create_dir_all(&root).unwrap();
    Builder::new()
        .prefix("ctx-search-mvp-")
        .tempdir_in(root)
        .unwrap()
}

fn ctx(temp: &TempDir) -> Command {
    let mut command = Command::cargo_bin("ctx").unwrap();
    command.env("CTX_DATA_ROOT", temp.path());
    command.env("HOME", temp.path());
    command.env("CTX_ANALYTICS_OFF", "1");
    command.env("CTX_DISABLE_AUTO_UPDATE", "1");
    command
}

fn provider_history_fixture(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/provider-history")
        .join(name)
        .to_str()
        .unwrap()
        .to_owned()
}

fn provider_fixture(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/provider")
        .join(name)
        .to_str()
        .unwrap()
        .to_owned()
}

fn redaction_fixture(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/redaction")
        .join(name)
        .to_str()
        .unwrap()
        .to_owned()
}

fn write_multi_session_provider_fixture(
    temp: &TempDir,
    provider: &str,
    query_marker: &str,
) -> String {
    let path = temp
        .path()
        .join(format!("{provider}-normalized-multi-session.jsonl"));
    let primary = format!("{provider}-normalized-primary");
    let followup = format!("{provider}-normalized-followup");
    let lines = [
        json!({
            "provider": provider,
            "session": {
                "provider_session_id": primary,
                "agent_type": "primary",
                "role_hint": "primary",
                "is_primary": true,
                "status": "imported",
                "started_at": "2026-06-24T12:00:00Z",
                "cwd": "/workspace/provider-e2e",
                "metadata": {"source": "cli-test", "scenario": "multi-session"}
            },
            "event": {
                "provider_event_index": 0,
                "cursor": format!("{provider}-primary-user-0"),
                "event_type": "message",
                "role": "user",
                "occurred_at": "2026-06-24T12:00:01Z",
                "payload": {"text": format!("{query_marker} primary asks for release smoke proof")},
                "metadata": {"source": "cli-test"}
            }
        }),
        json!({
            "provider": provider,
            "session": {
                "provider_session_id": format!("{provider}-normalized-worker"),
                "parent_provider_session_id": primary,
                "root_provider_session_id": primary,
                "external_agent_id": format!("{provider}-worker"),
                "agent_type": "subagent",
                "role_hint": "worker",
                "is_primary": false,
                "status": "imported",
                "started_at": "2026-06-24T12:00:02Z",
                "cwd": "/workspace/provider-e2e",
                "metadata": {"source": "cli-test", "scenario": "multi-session"}
            },
            "event": {
                "provider_event_index": 0,
                "cursor": format!("{provider}-worker-assistant-0"),
                "event_type": "summary",
                "role": "assistant",
                "occurred_at": "2026-06-24T12:00:03Z",
                "payload": {"text": format!("{query_marker} worker reports provider-filter citations")},
                "metadata": {"source": "cli-test"}
            }
        }),
        json!({
            "provider": provider,
            "session": {
                "provider_session_id": followup,
                "agent_type": "primary",
                "role_hint": "primary",
                "is_primary": true,
                "status": "imported",
                "started_at": "2026-06-24T12:01:00Z",
                "cwd": "/workspace/provider-e2e",
                "metadata": {"source": "cli-test", "scenario": "multi-session"}
            },
            "event": {
                "provider_event_index": 0,
                "cursor": format!("{provider}-followup-user-0"),
                "event_type": "message",
                "role": "user",
                "occurred_at": "2026-06-24T12:01:01Z",
                "payload": {"text": format!("{query_marker} followup checks release smoke retrieval")},
                "metadata": {"source": "cli-test"}
            }
        }),
    ];
    let body = lines
        .iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(&path, body).unwrap();
    path.to_str().unwrap().to_owned()
}

fn copy_dir_all(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let file_type = entry.file_type().unwrap();
        let target = to.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_all(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn file_url(path: &Path) -> String {
    format!("file://{}", path.display())
}

fn write_update_manifest(
    temp: &TempDir,
    version: &str,
    artifact: &Path,
    sha_override: Option<&str>,
) -> PathBuf {
    let artifact_bytes = fs::read(artifact).unwrap();
    let sha256 = sha_override
        .map(str::to_owned)
        .unwrap_or_else(|| hex_sha256(&artifact_bytes));
    let manifest = temp.path().join("latest.json");
    fs::write(
        &manifest,
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "version": version,
            "platforms": {
                (platform_key()): {
                    "cli": {
                        "url_path": file_url(artifact),
                        "sha256": sha256,
                        "bytes": artifact_bytes.len()
                    }
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    manifest
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

fn platform_key() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "linux-x64",
        ("linux", "aarch64") => "linux-arm64",
        ("macos", "aarch64") => "macos-arm64",
        ("macos", "x86_64") => "macos-x64",
        ("windows", "x86_64") => "windows-x64",
        ("freebsd", "x86_64") => "freebsd-x64",
        _ => "unknown",
    }
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
        assert!(result["item_id"].is_string());
        assert!(result["item_type"].is_string());
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
        assert!(citation["item_id"].is_string());
        assert!(citation["item_type"].is_string());
        assert_eq!(citation["provider"], provider, "citation provider failed");
        assert_eq!(
            citation["source_exists"], true,
            "citation source_exists failed"
        );
        assert!(citation["cursor"].is_string());
    }
}

fn assert_provider_citation_count(packet: &Value, provider: &str, expected: usize) {
    let citations = packet["results"][0]["citations"].as_array().unwrap();
    assert_eq!(
        citations.len(),
        expected,
        "unexpected citation count in {packet:#}"
    );

    let mut sessions = BTreeSet::new();
    for citation in citations {
        assert_eq!(citation["provider"], provider, "citation provider failed");
        assert_eq!(
            citation["source_exists"], true,
            "citation source_exists failed"
        );
        assert!(citation["cursor"].is_string());
        sessions.insert(citation["session_id"].as_str().unwrap().to_owned());
    }
    assert_eq!(sessions.len(), expected, "expected citations from sessions");
}

#[test]
fn help_exposes_only_search_mvp_commands() {
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
        "setup", "status", "sources", "import", "list", "show", "search", "update", "doctor",
        "validate",
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
        "export",
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
        "export",
        "schema",
        "workspace",
        "work",
        "service",
        "capture",
        "vcs",
        "pr",
        "repair",
        "watch",
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
    let legacy_shims = temp.path().join("work-record").join("shims");
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
    let expected = "[updates]\n\
channel = \"stable\"\n\
auto_update = true\n";

    ctx(&temp).arg("setup").assert().success();
    assert_eq!(fs::read_to_string(&config_path).unwrap(), expected);

    let user_config = "# user managed ctx config\n[updates]\nchannel = \"nightly\"\n";
    fs::write(&config_path, user_config).unwrap();

    ctx(&temp).arg("setup").assert().success();
    assert_eq!(
        fs::read_to_string(&config_path).unwrap(),
        user_config,
        "setup must not overwrite an existing user config"
    );
}

#[test]
fn setup_catalogs_codex_sessions_without_deep_import() {
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

    let setup = json_output(ctx(&temp).args(["setup", "--json"]));
    assert_eq!(setup["catalog"]["cataloged_sessions"], 1);
    assert_eq!(setup["catalog"]["source_files"], 1);
    assert_eq!(setup["catalog"]["failed_sessions"], 0);

    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(status["cataloged_sessions"], 1);
    assert_eq!(status["indexed_catalog_sessions"], 0);
    assert_eq!(status["indexed_items"], 0);
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
        "amp",
    ] {
        assert!(help.contains(value), "provider {value} missing in\n{help}");
    }
}

#[test]
fn public_subcommand_help_is_golden_enough_for_search_mvp() {
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
                "[possible values: codex, pi, claude, opencode, antigravity, gemini, cursor, copilot-cli, factory-ai-droid, amp]",
                "--path <PATH>",
                "--resume",
                "--json",
            ],
        ),
        ("list", vec!["Usage: ctx list", "--limit <LIMIT>", "--json"]),
        ("show", vec!["Usage: ctx show", "<ID>", "--json"]),
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
                "--json",
            ],
        ),
        (
            "update",
            vec![
                "Usage: ctx update",
                "--json",
                "--check-only",
                "--apply",
                "--force",
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
fn update_check_uses_local_manifest_without_touching_binary() {
    let temp = tempdir();
    let artifact = temp.path().join("ctx-new");
    fs::write(&artifact, b"new ctx binary").unwrap();
    let manifest = write_update_manifest(&temp, "9.9.9", &artifact, None);

    let update = json_output(
        ctx(&temp)
            .args(["update", "--check-only", "--json"])
            .env("CTX_UPDATE_MANIFEST_URL", file_url(&manifest)),
    );

    assert_eq!(update["schema_version"], 1);
    assert_eq!(update["current_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(update["latest_version"], "9.9.9");
    assert_eq!(update["update_available"], true);
    assert_eq!(update["action"], "check_only");
    assert_eq!(update["applied"], false);
}

#[test]
fn update_apply_verifies_sha256_and_replaces_target_atomically() {
    let temp = tempdir();
    let artifact = temp.path().join("ctx-new");
    fs::write(&artifact, b"new ctx binary").unwrap();
    let manifest = write_update_manifest(&temp, "9.9.9", &artifact, None);
    let target = temp.path().join("bin").join("ctx");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, b"old ctx binary").unwrap();

    let update = json_output(
        ctx(&temp)
            .args(["update", "--json"])
            .env("CTX_UPDATE_MANIFEST_URL", file_url(&manifest))
            .env("CTX_UPDATE_TARGET", &target),
    );

    assert_eq!(update["action"], "applied");
    assert_eq!(update["applied"], true);
    assert_eq!(fs::read(&target).unwrap(), b"new ctx binary");
}

#[test]
fn update_apply_rejects_bad_checksum_and_preserves_target() {
    let temp = tempdir();
    let artifact = temp.path().join("ctx-new");
    fs::write(&artifact, b"new ctx binary").unwrap();
    let manifest = write_update_manifest(&temp, "9.9.9", &artifact, Some("00"));
    let target = temp.path().join("bin").join("ctx");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, b"old ctx binary").unwrap();

    ctx(&temp)
        .args(["update", "--json"])
        .env("CTX_UPDATE_MANIFEST_URL", file_url(&manifest))
        .env("CTX_UPDATE_TARGET", &target)
        .assert()
        .failure()
        .stderr(predicate::str::contains("sha256 mismatch"));

    assert_eq!(fs::read(&target).unwrap(), b"old ctx binary");
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
    assert_eq!(event["broker_runtime"], "daemon");
    assert_eq!(event["events"][0]["event_name"], "cli_invocation");
    assert_eq!(event["events"][0]["origin_runtime"], "daemon");
    assert_eq!(event["events"][0]["surface"], "cli");
    assert_eq!(event["events"][0]["properties"]["action"], "status");
    assert!(event["events"][0]["properties"].get("command").is_none());
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
        .stdout(predicate::str::contains(
            "local agent history search is ready",
        ));

    let setup_json = json_output(ctx(&temp).args(["setup", "--json"]));
    assert_eq!(setup_json["schema_version"], 1);
    assert_eq!(setup_json["network_required"], false);
    assert_eq!(setup_json["repo_writes"], false);

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
    assert_omits_keys(&listed, &["record_id", "work_record_id", "kind"]);
    assert_eq!(listed["items"][0]["item_type"], "agent_history");
    let first_id = listed["items"][0]["item_id"].as_str().unwrap().to_owned();
    assert_eq!(listed["items"][0]["id"], listed["items"][0]["item_id"]);

    let search = json_output(ctx(&temp).args(["search", "onboarding", "--json"]));
    assert_eq!(search["schema_version"], 1);
    assert_eq!(search["share_safe"], false);
    assert_omits_keys(
        &search,
        &["record_id", "work_record_id", "raw_source_path", "kind"],
    );
    assert!(search["results"][0]["item_id"].is_string());
    assert_eq!(search["results"][0]["item_type"], "agent_history");
    assert!(search["results"][0]["citations"][0]["item_id"].is_string());
    assert!(search["results"][0]["citations"][0]["item_type"].is_string());

    let file_search =
        json_output(ctx(&temp).args(["search", "--file", "crates/foo/src/lib.rs", "--json"]));
    assert_eq!(file_search["query"], "");
    assert!(file_search["results"].is_array());

    let show = json_output(ctx(&temp).args(["show", &first_id, "--json"]));
    assert_eq!(show["schema_version"], 1);
    assert_eq!(show["item"]["item_id"], first_id);
    assert_eq!(show["item"]["item_type"], "agent_history");
    assert_omits_keys(
        &show,
        &[
            "record_id",
            "work_record_id",
            "kind",
            "payload",
            "payload_blob_id",
            "dedupe_key",
            "capture_source_id",
        ],
    );
    assert!(show["events"]
        .as_array()
        .unwrap()
        .iter()
        .all(|event| event["item_type"] == "event" && event["preview"].is_string()));

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
    assert_eq!(first["totals"]["imported_events"], 6);
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
    assert_eq!(first["totals"]["imported_events"], 6);
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
    assert_eq!(basic["totals"]["imported_events"], 6);
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
    assert_eq!(rich["totals"]["imported_events"], 5);

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
        11
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
        4
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
fn normalized_provider_cli_flow_covers_all_harness_providers_with_multiple_sessions() {
    for (cli_provider, stored_provider) in [
        ("claude", "claude"),
        ("opencode", "opencode"),
        ("antigravity", "antigravity"),
        ("gemini", "gemini"),
        ("cursor", "cursor"),
        ("copilot-cli", "copilot_cli"),
        ("factory-ai-droid", "factory_ai_droid"),
        ("amp", "amp"),
    ] {
        let temp = tempdir();
        let query = format!("{stored_provider}-multi-session-oracle");
        let fixture = write_multi_session_provider_fixture(&temp, stored_provider, &query);

        let mut import_cmd = ctx(&temp);
        import_cmd.env("CTX_PROVIDER_NORMALIZED_IMPORT_DEV", "1");
        let imported = json_output(import_cmd.args([
            "import",
            "--provider",
            cli_provider,
            "--path",
            &fixture,
            "--json",
        ]));
        assert_eq!(imported["schema_version"], 1);
        assert_eq!(imported["sources"][0]["provider"], stored_provider);
        assert_eq!(
            imported["sources"][0]["source_format"],
            "normalized_provider_jsonl"
        );
        assert_eq!(imported["totals"]["imported_sessions"], 3);
        assert_eq!(imported["totals"]["imported_events"], 3);
        assert_eq!(imported["totals"]["imported_edges"], 1);
        assert_eq!(imported["totals"]["failed"], 0);

        let search =
            json_output(ctx(&temp).args(["search", &query, "--provider", cli_provider, "--json"]));
        assert_search_provider_oracle(&search, stored_provider, &query, 1, "message");
        assert_provider_citation_count(&search, stored_provider, 3);

        let status = json_output(ctx(&temp).args(["status", "--json"]));
        assert_eq!(status["indexed_items"], 4);
        assert_eq!(status["indexed_sources"], 3);

        let doctor = json_output(ctx(&temp).args(["doctor", "--json"]));
        assert_eq!(doctor["ok"], true);

        let validate = json_output(ctx(&temp).args(["validate", "--json"]));
        assert_eq!(validate["valid"], true);

        let mut second_cmd = ctx(&temp);
        second_cmd.env("CTX_PROVIDER_NORMALIZED_IMPORT_DEV", "1");
        let second = json_output(second_cmd.args([
            "import",
            "--provider",
            cli_provider,
            "--path",
            &fixture,
            "--resume",
            "--json",
        ]));
        assert_eq!(second["resume"], true);
        assert_eq!(second["resume_mode"], "idempotent_rescan");
        assert_eq!(second["totals"]["imported_sessions"], 0);
        assert_eq!(second["totals"]["imported_events"], 0);
        assert_eq!(second["totals"]["imported_edges"], 0);
        assert!(second["totals"]["skipped"].as_u64().unwrap() >= 6);

        let conn = Connection::open(temp.path().join("work.sqlite")).unwrap();
        assert_eq!(
            sqlite_count(
                &conn,
                &format!("SELECT COUNT(*) FROM sessions WHERE provider = '{stored_provider}'")
            ),
            3
        );
        assert_eq!(
            sqlite_count(
                &conn,
                &format!("SELECT COUNT(*) FROM events e JOIN sessions s ON e.session_id = s.id WHERE s.provider = '{stored_provider}'")
            ),
            3
        );
        assert_eq!(
            sqlite_count(
                &conn,
                &format!("SELECT COUNT(*) FROM session_edges se JOIN sessions child ON se.to_session_id = child.id WHERE child.provider = '{stored_provider}'")
            ),
            1
        );
    }
}

#[test]
fn normalized_provider_cli_requires_explicit_path_for_non_discovered_providers() {
    for (cli_provider, expected_blocker) in [
        ("claude", "native Claude local-history import is blocked"),
        ("opencode", "native OpenCode import is blocked"),
        ("antigravity", "native Antigravity import is blocked"),
        ("gemini", "native Gemini import is blocked"),
        ("cursor", "native Cursor import is blocked"),
        (
            "copilot-cli",
            "Copilot CLI local history is detected but unsupported",
        ),
        (
            "factory-ai-droid",
            "Factory AI Droid native import is blocked",
        ),
        ("amp", "Amp native local thread import is blocked"),
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
fn normalized_provider_cli_requires_developer_opt_in_for_explicit_path() {
    let temp = tempdir();
    let fixture = provider_fixture("claude.jsonl");

    ctx(&temp)
        .args([
            "import",
            "--provider",
            "claude",
            "--path",
            &fixture,
            "--json",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "claude normalized provider JSONL import is a developer-only input",
        ));
}

#[test]
fn normalized_provider_cli_rejects_provider_mismatches() {
    let temp = tempdir();
    let fixture = provider_fixture("claude.jsonl");
    let mut import_cmd = ctx(&temp);
    import_cmd.env("CTX_PROVIDER_NORMALIZED_IMPORT_DEV", "1");
    let imported = json_output(import_cmd.args([
        "import",
        "--provider",
        "gemini",
        "--path",
        &fixture,
        "--json",
    ]));
    assert_eq!(imported["schema_version"], 1);
    assert_eq!(imported["sources"][0]["provider"], "gemini");
    assert_eq!(imported["totals"]["imported_sessions"], 0);
    assert_eq!(imported["totals"]["imported_events"], 0);
    assert_eq!(imported["totals"]["failed"], 2);
    assert_eq!(imported["sources"][0]["failed"], 2);
    assert!(imported["sources"][0].get("failures").is_none());
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

    let query = "after malformed line";
    let search = json_output(ctx(&temp).args(["search", query, "--provider", "pi", "--json"]));
    assert_search_provider_oracle(&search, "pi", query, 1, "message");
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
        .stderr(predicate::str::contains(
            "provider transcript paths must be regular files",
        ));
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
    assert_eq!(imported["totals"]["imported_events"], 6);

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
    let item_id = search["results"][0]["item_id"].as_str().unwrap().to_owned();

    let show = json_output(ctx(&temp).args(["show", &item_id, "--json"]));
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
        "SELECT COALESCE(title, '') || ' ' || COALESCE(summary, '') || ' ' || COALESCE(primary_user_text, '') || ' ' || COALESCE(decision_text, '') || ' ' || COALESCE(context_text, '') || ' ' || COALESCE(tag_text, '') FROM work_record_search",
    );
    let sqlite_text = format!("{event_payloads}\n{event_index}\n{record_index}");
    assert!(sqlite_text.contains("[REDACTED"));
    assert!(event_index.contains("[REDACTED_PATH]"));
    assert_omits_sensitive_markers("sqlite indexed output", &sqlite_text);
}
