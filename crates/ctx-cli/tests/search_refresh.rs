mod support;

use support::*;

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
    let indexed_fixture = provider_history_fixture("codex-sessions");
    json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &indexed_fixture,
        "--json",
    ]));
    let discovered_fixture = provider_history_fixture("codex-rich-sessions");
    let discovered = temp.path().join(".codex").join("sessions");
    copy_dir_all(&PathBuf::from(discovered_fixture), &discovered);

    let stale = json_output(ctx(&temp).args([
        "search",
        "redacted sample app",
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
    assert_eq!(status["cataloged_sessions"], 2);
    assert_eq!(status["indexed_catalog_sessions"], 2);

    let fresh =
        json_output(ctx(&temp).args(["search", "onboarding", "--provider", "codex", "--json"]));
    assert_search_provider_oracle(&fresh, "codex", "onboarding", 1, "message");
}

#[test]
fn search_refresh_auto_runs_enabled_auto_history_source_plugins_incrementally() {
    let temp = tempdir();
    let cursor_log = temp.path().join("cursor-log.txt");
    let plugin = write_history_source_plugin_with_refresh(
        &temp,
        "hermes",
        true,
        Some("auto"),
        Some(&cursor_log),
    );

    let initial = json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args([
                "search",
                "hermes plugin initial marker",
                "--provider",
                "custom",
                "--json",
            ]),
    );
    assert_eq!(initial["freshness"]["mode"], "auto");
    assert_eq!(initial["freshness"]["status"], "completed");
    assert_eq!(initial["freshness"]["source_count"], 1);
    assert_eq!(initial["freshness"]["totals"]["imported_sources"], 1);
    assert_eq!(initial["freshness"]["totals"]["imported_sessions"], 1);
    assert_eq!(initial["freshness"]["totals"]["imported_events"], 1);
    assert!(
        !initial["results"].as_array().unwrap().is_empty(),
        "initial plugin refresh was not searchable before query: {initial:#}"
    );
    assert!(plugin.run_marker.exists());

    fs::remove_file(&plugin.run_marker).unwrap();
    let incremental = json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args([
                "search",
                "hermes plugin incremental marker",
                "--provider",
                "custom",
                "--json",
            ]),
    );
    assert_eq!(incremental["freshness"]["mode"], "auto");
    assert_eq!(incremental["freshness"]["status"], "completed");
    assert_eq!(incremental["freshness"]["source_count"], 1);
    assert_eq!(incremental["freshness"]["totals"]["imported_sources"], 1);
    assert_eq!(incremental["freshness"]["totals"]["imported_events"], 1);
    assert!(
        !incremental["results"].as_array().unwrap().is_empty(),
        "incremental plugin refresh was not searchable before query: {incremental:#}"
    );
    assert!(plugin.run_marker.exists());

    let cursor_log = fs::read_to_string(cursor_log).unwrap();
    assert!(cursor_log.contains(r#""message_id":7"#), "{cursor_log}");
    assert!(cursor_log.contains("cursor_file="), "{cursor_log}");
}

#[test]
fn search_refresh_history_source_filter_runs_only_matching_auto_plugin() {
    let temp = tempdir();
    let plugin_root = temp.path().join("history-plugins");
    let dorkos = write_history_source_plugin_at_with_refresh(
        &plugin_root,
        "dorkos",
        true,
        Some("auto"),
        None,
    );
    let hermes = write_history_source_plugin_at_with_refresh(
        &plugin_root,
        "hermes",
        true,
        Some("auto"),
        None,
    );

    let search = json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin_root)
            .args([
                "search",
                "dorkos plugin initial marker",
                "--history-source",
                "dorkos/default",
                "--json",
            ]),
    );

    assert_eq!(search["filters"]["provider"], "custom");
    assert_eq!(search["filters"]["history_source"], "dorkos/default");
    assert_eq!(search["freshness"]["status"], "completed");
    assert_eq!(search["freshness"]["source_count"], 1);
    assert!(dorkos.run_marker.exists());
    assert!(!hermes.run_marker.exists());
    assert!(
        !search["results"].as_array().unwrap().is_empty(),
        "source-filtered refresh did not import matching plugin: {search:#}"
    );
}

#[test]
fn search_refresh_auto_combines_native_sources_and_auto_history_source_plugins() {
    let temp = tempdir();
    let fixture = PathBuf::from(provider_history_fixture("codex-sessions"));
    copy_dir_all(&fixture, &temp.path().join(".codex").join("sessions"));
    let plugin =
        write_history_source_plugin_with_refresh(&temp, "hermes", true, Some("auto"), None);

    let search = json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args(["search", "hermes plugin initial marker", "--json"]),
    );

    assert_eq!(search["freshness"]["mode"], "auto");
    assert_eq!(search["freshness"]["status"], "completed");
    assert_eq!(search["freshness"]["source_count"], 2);
    assert!(
        search["freshness"]["totals"]["imported_sessions"]
            .as_u64()
            .unwrap()
            >= 3
    );
    assert!(
        !search["results"].as_array().unwrap().is_empty(),
        "combined refresh did not make plugin history searchable: {search:#}"
    );
    assert!(plugin.run_marker.exists());
}

#[test]
fn search_refresh_provider_filter_does_not_execute_history_source_plugins() {
    let temp = tempdir();
    let fixture = PathBuf::from(provider_history_fixture("codex-sessions"));
    copy_dir_all(&fixture, &temp.path().join(".codex").join("sessions"));
    let plugin =
        write_history_source_plugin_with_refresh(&temp, "hermes", true, Some("auto"), None);

    let search = json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args(["search", "onboarding", "--provider", "codex", "--json"]),
    );

    assert_eq!(search["freshness"]["mode"], "auto");
    assert_eq!(search["freshness"]["status"], "completed");
    assert_eq!(search["freshness"]["source_count"], 1);
    assert_search_provider_oracle(&search, "codex", "onboarding", 1, "message");
    assert!(!plugin.run_marker.exists());
}

#[test]
fn search_refresh_off_does_not_execute_history_source_plugins() {
    let temp = tempdir();
    json_output(ctx(&temp).args(["setup", "--json"]));
    let plugin =
        write_history_source_plugin_with_refresh(&temp, "hermes", true, Some("auto"), None);

    let search = json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args([
                "search",
                "hermes plugin initial marker",
                "--provider",
                "custom",
                "--refresh",
                "off",
                "--json",
            ]),
    );

    assert_eq!(search["freshness"]["mode"], "off");
    assert_eq!(search["freshness"]["status"], "skipped");
    assert!(search["results"].as_array().unwrap().is_empty());
    assert!(!plugin.run_marker.exists());
}

#[test]
fn search_refresh_auto_skips_disabled_or_manual_history_source_plugins() {
    let temp = tempdir();
    let plugin_root = temp.path().join("history-plugins");
    let manual = write_history_source_plugin_at_with_refresh(
        &plugin_root,
        "hermes",
        true,
        Some("manual"),
        None,
    );
    let disabled = write_history_source_plugin_at_with_refresh(
        &plugin_root,
        "dorkos",
        false,
        Some("auto"),
        None,
    );

    let search = json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin_root)
            .args([
                "search",
                "plugin initial marker",
                "--provider",
                "custom",
                "--json",
            ]),
    );

    assert_eq!(search["freshness"]["mode"], "auto");
    assert_eq!(search["freshness"]["status"], "no_sources");
    assert_eq!(search["freshness"]["source_count"], 0);
    assert!(search["results"].as_array().unwrap().is_empty());
    assert!(!manual.run_marker.exists());
    assert!(!disabled.run_marker.exists());
}

#[test]
fn search_refresh_strict_fails_on_history_source_plugin_failure() {
    let temp = tempdir();
    let script = r#"#!/usr/bin/env python3
import sys
print("plugin exploded", file=sys.stderr)
sys.exit(23)
"#;
    let plugin = write_raw_history_source_plugin_with_options(
        &temp,
        "badplugin",
        script,
        true,
        Some("auto"),
    );

    let stderr = failure_stderr(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args([
                "search",
                "anything",
                "--provider",
                "custom",
                "--refresh",
                "strict",
                "--json",
            ]),
    );

    assert!(stderr.contains("search refresh failed"), "{stderr}");
    assert!(
        stderr.contains("history source plugin badplugin/default failed"),
        "{stderr}"
    );
    assert!(stderr.contains("plugin exploded"), "{stderr}");
}

#[test]
fn search_refresh_auto_failure_without_prior_store_fails_instead_of_serving_empty_index() {
    let temp = tempdir();
    let script = r#"#!/usr/bin/env python3
import sys
print("plugin exploded", file=sys.stderr)
sys.exit(23)
"#;
    let plugin = write_raw_history_source_plugin_with_options(
        &temp,
        "badplugin",
        script,
        true,
        Some("auto"),
    );

    let stderr = failure_stderr(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args(["search", "anything", "--provider", "custom", "--json"]),
    );

    assert!(
        stderr.contains("search refresh failed and no existing ctx index is available"),
        "{stderr}"
    );
    assert!(
        stderr.contains("history source plugin badplugin/default failed"),
        "{stderr}"
    );
    assert!(stderr.contains("plugin exploded"), "{stderr}");
}

#[test]
fn search_refresh_auto_failure_serves_prior_index() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");
    let script = r#"#!/usr/bin/env python3
import sys
print("plugin exploded", file=sys.stderr)
sys.exit(23)
"#;
    let plugin = write_raw_history_source_plugin_with_options(
        &temp,
        "badplugin",
        script,
        true,
        Some("auto"),
    );
    json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--json",
    ]));

    let search = json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args(["search", "onboarding", "--json"]),
    );

    assert_eq!(search["freshness"]["status"], "failed");
    assert!(search["freshness"]["error"]
        .as_str()
        .unwrap()
        .contains("history source plugin badplugin/default failed"));
    assert!(!search["results"].as_array().unwrap().is_empty());
}

#[test]
fn search_refresh_strict_times_out_when_plugin_helper_keeps_stdout_open() {
    let temp = tempdir();
    let script = r#"#!/usr/bin/env python3
import json
import os
import subprocess

observed = "2026-07-01T12:00:00Z"
source_id = os.environ["CTX_HISTORY_SOURCE_ID"]
provider_key = os.environ["CTX_HISTORY_PROVIDER_KEY"]
source_format = os.environ["CTX_HISTORY_SOURCE_FORMAT"]
cursor_stream = os.environ["CTX_HISTORY_CURSOR_STREAM"]
records = [
    {"record_type": "manifest", "schema_version": "ctx-history-jsonl-v1"},
    {"record_type": "source", "source_id": source_id, "provider_key": provider_key, "source_format": source_format, "observed_at": observed, "cursor": {"after": {"stream": cursor_stream, "cursor": json.dumps({"seq": 1}), "observed_at": observed}}},
    {"record_type": "session", "source_id": source_id, "session_id": "hanging-session", "started_at": observed, "agent_type": "primary", "is_primary": True, "status": "completed"},
    {"record_type": "event", "source_id": source_id, "session_id": "hanging-session", "event_index": 0, "event_type": "message", "role": "assistant", "occurred_at": observed, "payload": {"text": "hanging plugin marker"}, "preview": "hanging plugin marker"},
]
for record in records:
    print(json.dumps(record, separators=(",", ":")), flush=True)
subprocess.Popen(["sh", "-c", "sleep 5"])
"#;
    let plugin = write_raw_history_source_plugin_with_options_and_timeout(
        &temp,
        "hanging",
        script,
        true,
        Some("auto"),
        1,
    );

    let started = Instant::now();
    let stderr = failure_stderr(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args([
                "search",
                "hanging plugin marker",
                "--provider",
                "custom",
                "--refresh",
                "strict",
                "--json",
            ]),
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "plugin timeout did not bound pipe draining: {stderr}"
    );
    assert!(
        stderr.contains("history source plugin hanging/default timed out after 1s"),
        "{stderr}"
    );
}

#[test]
fn search_refresh_auto_imports_fresh_work_despite_large_existing_catalog() {
    let temp = tempdir();
    let fixture = PathBuf::from(provider_history_fixture("codex-sessions"));
    let _ = json_output(ctx(&temp).args(["setup", "--json"]));
    let discovered = temp.path().join(".codex").join("sessions");
    copy_dir_all(&fixture, &discovered);

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

    let second_append_needle = "tail-refresh-second-append-oracle";
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&root_session)
        .unwrap();
    writeln!(
        file,
        "{}",
        json!({
            "timestamp": "2026-06-23T15:00:31.000Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": second_append_needle}]
            }
        })
    )
    .unwrap();

    let second_refreshed = json_output(ctx(&temp).args([
        "search",
        second_append_needle,
        "--provider",
        "codex",
        "--json",
    ]));
    assert_eq!(second_refreshed["freshness"]["status"], "completed");
    assert_eq!(
        second_refreshed["freshness"]["totals"]["imported_events"],
        1
    );
    assert!(
        second_refreshed["freshness"]["totals"]["skipped"]
            .as_u64()
            .unwrap()
            < 20,
        "second tail refresh unexpectedly reprocessed old events: {}",
        second_refreshed["freshness"]["totals"]
    );
    assert_search_provider_oracle(
        &second_refreshed,
        "codex",
        second_append_needle,
        1,
        "message",
    );
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
