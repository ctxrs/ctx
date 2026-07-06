mod support;

use support::*;

#[test]
fn history_source_plugins_are_listed_without_running() {
    let temp = tempdir();
    let plugin = write_history_source_plugin(&temp, "dorkos", false, None);

    let sources = json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args(["sources", "--json"]),
    );
    let plugin_source = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["history_source"] == "dorkos/default")
        .unwrap();
    assert_eq!(plugin_source["kind"], "history_source_plugin");
    assert_eq!(plugin_source["provider_key"], "dorkos");
    assert_eq!(plugin_source["enabled"], false);
    assert!(!plugin.run_marker.exists());
}

#[test]
fn invalid_installed_history_source_plugin_is_listed_as_invalid() {
    let temp = tempdir();
    let plugin_root = temp.path().join("history-plugins");
    let bad_dir = plugin_root.join("bad");
    fs::create_dir_all(&bad_dir).unwrap();
    fs::write(bad_dir.join("ctx-history-plugin.json"), "{not-json").unwrap();

    let sources = json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin_root)
            .args(["sources", "--json"]),
    );
    let invalid = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["kind"] == "history_source_plugin" && source["status"] == "invalid")
        .unwrap();
    assert_eq!(invalid["importable"], false);
    assert_eq!(invalid["enabled"], false);
    assert!(invalid["error"]
        .as_str()
        .unwrap()
        .contains("parse history source plugin manifest"));
}

#[test]
fn oversized_installed_history_source_plugin_is_listed_as_invalid() {
    let temp = tempdir();
    let plugin_root = temp.path().join("history-plugins");
    let bad_dir = plugin_root.join("oversized");
    fs::create_dir_all(&bad_dir).unwrap();
    fs::write(
        bad_dir.join("ctx-history-plugin.json"),
        vec![b' '; 2 * 1024 * 1024],
    )
    .unwrap();

    let sources = json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin_root)
            .args(["sources", "--json"]),
    );
    let invalid = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["kind"] == "history_source_plugin" && source["status"] == "invalid")
        .unwrap();
    assert_eq!(invalid["importable"], false);
    assert!(invalid["error"]
        .as_str()
        .unwrap()
        .contains("exceeds max bytes"));
}

#[test]
fn invalid_installed_history_source_plugin_does_not_block_valid_import() {
    let temp = tempdir();
    let plugin_root = temp.path().join("history-plugins");
    let good = write_history_source_plugin_at(&plugin_root, "dorkos", false, None);
    let bad_dir = plugin_root.join("bad");
    fs::create_dir_all(&bad_dir).unwrap();
    fs::write(bad_dir.join("ctx-history-plugin.json"), "{not-json").unwrap();

    let imported = json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin_root)
            .args([
                "import",
                "--history-source",
                "dorkos/default",
                "--json",
                "--progress",
                "none",
            ]),
    );

    assert_eq!(imported["totals"]["imported_sources"], 1);
    assert!(good.run_marker.exists());
}

#[test]
fn removed_history_source_plugin_aliases_and_legacy_discovery_are_ignored() {
    let temp = tempdir();
    let plugin = write_history_source_plugin(&temp, "dorkos", false, None);

    let stderr = failure_stderr(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args(["import", "--plugin", "dorkos/default"]),
    );
    assert!(stderr.contains("--plugin"), "{stderr}");

    let stderr = failure_stderr(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args(["import", "--plugin-manifest", "ctx-history-plugin.json"]),
    );
    assert!(stderr.contains("--plugin-manifest"), "{stderr}");

    let sources = json_output(
        ctx(&temp)
            .env_remove("CTX_HISTORY_PLUGIN_PATH")
            .env("CTX_PLUGIN_PATH", &plugin.manifest_dir)
            .args(["sources", "--json"]),
    );
    assert!(!sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|source| source["history_source"] == "dorkos/default"));

    let legacy_dir = temp.path().join("legacy-plugin");
    fs::create_dir_all(&legacy_dir).unwrap();
    fs::copy(
        plugin.manifest_dir.join("ctx-history-plugin.json"),
        legacy_dir.join("plugin.json"),
    )
    .unwrap();
    let sources = json_output(
        ctx(&temp)
            .env_remove("CTX_PLUGIN_PATH")
            .env("CTX_HISTORY_PLUGIN_PATH", &legacy_dir)
            .args(["sources", "--json"]),
    );
    assert!(!sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|source| source["history_source"] == "dorkos/default"));
}

#[test]
fn setup_does_not_execute_enabled_history_source_plugins() {
    let temp = tempdir();
    let plugin = write_history_source_plugin(&temp, "dorkos", true, None);

    json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args(["setup", "--json", "--progress", "none"]),
    );

    assert!(!plugin.run_marker.exists());
}

#[test]
fn bare_history_source_plugin_selector_fails_before_execution() {
    let temp = tempdir();
    let plugin_root = temp.path().join("history-plugins");
    let dorkos = write_history_source_plugin_at(&plugin_root, "dorkos", false, None);
    let hermes = write_history_source_plugin_at(&plugin_root, "hermes", false, None);

    let stderr = failure_stderr(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin_root)
            .args(["import", "--history-source", "dorkos", "--progress", "none"]),
    );

    assert!(
        stderr.contains("no history source plugin matched"),
        "{stderr}"
    );
    assert!(!dorkos.run_marker.exists());
    assert!(!hermes.run_marker.exists());
}

#[test]
fn explicit_history_source_manifest_reports_parse_errors() {
    let temp = tempdir();
    let bad_manifest = temp.path().join("bad-plugin.json");
    fs::write(&bad_manifest, "{not-json").unwrap();

    let stderr = failure_stderr(ctx(&temp).args([
        "import",
        "--history-source-manifest",
        bad_manifest.to_str().unwrap(),
        "--progress",
        "none",
    ]));

    assert!(
        stderr.contains("parse history source plugin manifest"),
        "{stderr}"
    );
}

#[test]
fn explicit_history_source_manifest_reports_nonexistent_path() {
    let temp = tempdir();
    let path = temp.path().join("no-such-manifest.json");

    let stderr = failure_stderr(ctx(&temp).args([
        "import",
        "--history-source-manifest",
        path.to_str().unwrap(),
        "--progress",
        "none",
    ]));

    assert!(stderr.contains("import path does not exist"), "{stderr}");
    assert!(stderr.contains(path.to_str().unwrap()), "{stderr}");
}

#[test]
fn failed_history_source_plugin_import_does_not_leave_record_metadata() {
    let temp = tempdir();
    let script = r#"#!/usr/bin/env python3
import json
provider = "badplugin"
records = [
  {"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"},
  {"record_type":"source","source_id":"default","provider_key":provider,"source_format":"badplugin-history-v1"},
  {"record_type":"event","source_id":"default","session_id":"missing","event_index":0,"event_type":"message","role":"assistant","occurred_at":"2026-07-01T12:00:00Z","preview":"should not import"}
]
for record in records:
    print(json.dumps(record))
"#;
    let plugin = write_raw_history_source_plugin(&temp, "badplugin", script);

    let stderr = failure_stderr(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args([
                "import",
                "--history-source",
                "badplugin/default",
                "--progress",
                "none",
            ]),
    );

    assert!(stderr.contains("import failed"), "{stderr}");
    let conn = Connection::open(temp.path().join("work.sqlite")).unwrap();
    assert_eq!(
        sqlite_count(&conn, "SELECT COUNT(*) FROM history_records"),
        0
    );
    assert_eq!(sqlite_count(&conn, "SELECT COUNT(*) FROM sessions"), 0);
    assert_eq!(sqlite_count(&conn, "SELECT COUNT(*) FROM events"), 0);
}

#[test]
fn history_source_plugin_rejects_mismatched_machine_id_before_import() {
    let temp = tempdir();
    let script = r#"#!/usr/bin/env python3
import json
records = [
  {"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"},
  {"record_type":"source","source_id":"default","provider_key":"machineplugin","source_format":"machineplugin-history-v1","machine_id":"other-machine"},
  {"record_type":"session","source_id":"default","session_id":"run","started_at":"2026-07-01T12:00:00Z"},
]
for record in records:
    print(json.dumps(record))
"#;
    let plugin = write_raw_history_source_plugin(&temp, "machineplugin", script);

    let stderr = failure_stderr(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args([
                "import",
                "--history-source",
                "machineplugin/default",
                "--progress",
                "none",
            ]),
    );

    assert!(stderr.contains("machine_id"), "{stderr}");
    let conn = Connection::open(temp.path().join("work.sqlite")).unwrap();
    assert_eq!(
        sqlite_count(&conn, "SELECT COUNT(*) FROM history_records"),
        0
    );
}

#[test]
fn history_source_plugin_rejects_oversized_stdout_line() {
    let temp = tempdir();
    let script = r#"#!/usr/bin/env python3
import sys
sys.stdout.write("x" * (17 * 1024 * 1024) + "\n")
"#;
    let plugin = write_raw_history_source_plugin(&temp, "bigline", script);

    let stderr = failure_stderr(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args([
                "import",
                "--history-source",
                "bigline/default",
                "--json",
                "--progress",
                "none",
            ]),
    );

    assert!(stderr.contains("line 1 exceeding max bytes"), "{stderr}");
}

#[test]
fn history_source_plugin_reset_requires_fresh_after_cursor() {
    let temp = tempdir();
    let script = r#"#!/usr/bin/env python3
import json
records = [
  {"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"},
  {"record_type":"source","source_id":"default","provider_key":"nocursor","source_format":"nocursor-history-v1"},
  {"record_type":"session","source_id":"default","session_id":"run","started_at":"2026-07-01T12:00:00Z"},
]
for record in records:
    print(json.dumps(record))
"#;
    let plugin = write_raw_history_source_plugin(&temp, "nocursor", script);

    let stderr = failure_stderr(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args([
                "import",
                "--history-source",
                "nocursor/default",
                "--reset-cursor",
                "--progress",
                "none",
            ]),
    );

    assert!(stderr.contains("source.cursor.after"), "{stderr}");
    let conn = Connection::open(temp.path().join("work.sqlite")).unwrap();
    assert_eq!(
        sqlite_count(&conn, "SELECT COUNT(*) FROM history_records"),
        0
    );
}

#[test]
fn large_history_source_plugin_cursor_uses_cursor_file_without_inline_env() {
    let temp = tempdir();
    let log = temp.path().join("large-cursor.log");
    let log_json = serde_json::to_string(&log.display().to_string()).unwrap();
    let script = format!(
        r#"#!/usr/bin/env python3
import json
import os
import pathlib

cursor_file = os.environ.get("CTX_HISTORY_CURSOR_FILE")
inline = os.environ.get("CTX_HISTORY_CURSOR")
cursor_text = pathlib.Path(cursor_file).read_text() if cursor_file else inline
if cursor_text:
    with open({log_json}, "a", encoding="utf-8") as handle:
        handle.write("inline=" + ("1" if inline else "0") + "\n")
        handle.write("file_len=" + str(len(cursor_text)) + "\n")
next_cursor = "x" * 9000 if not cursor_text else "done"
observed = "2026-07-01T12:00:00Z"
records = [
  {{"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"}},
  {{"record_type":"source","source_id":"default","provider_key":"largecursor","source_format":"largecursor-history-v1","cursor":{{"after":{{"stream":os.environ["CTX_HISTORY_CURSOR_STREAM"],"cursor":next_cursor,"observed_at":observed}}}}}},
  {{"record_type":"session","source_id":"default","session_id":"run","started_at":"2026-07-01T12:00:00Z"}},
  {{"record_type":"event","source_id":"default","session_id":"run","event_index":1 if cursor_text else 0,"event_type":"message","role":"assistant","occurred_at":observed,"preview":"large cursor marker"}},
]
for record in records:
    print(json.dumps(record))
"#
    );
    let plugin = write_raw_history_source_plugin(&temp, "largecursor", &script);

    json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args([
                "import",
                "--history-source",
                "largecursor/default",
                "--json",
                "--progress",
                "none",
            ]),
    );
    json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args([
                "import",
                "--history-source",
                "largecursor/default",
                "--json",
                "--progress",
                "none",
            ]),
    );

    let log = fs::read_to_string(log).unwrap();
    assert!(log.contains("inline=0"), "{log}");
    assert!(log.contains("file_len=9000"), "{log}");
}

#[test]
fn import_history_source_plugin_is_searchable_and_receives_cursor() {
    let temp = tempdir();
    let cursor_log = temp.path().join("cursor-log.txt");
    let plugin = write_history_source_plugin(&temp, "hermes", false, Some(&cursor_log));

    let first = json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args([
                "import",
                "--history-source",
                "hermes/default",
                "--resume",
                "--json",
                "--progress",
                "none",
            ]),
    );
    assert_eq!(first["totals"]["imported_sessions"], 1);
    assert_eq!(first["totals"]["imported_events"], 1);
    assert_eq!(first["sources"][0]["history_source"], "hermes/default");

    let initial = json_output(ctx(&temp).args([
        "search",
        "hermes plugin initial marker",
        "--provider",
        "custom",
        "--refresh",
        "off",
        "--json",
    ]));
    assert!(
        !initial["results"].as_array().unwrap().is_empty(),
        "initial plugin import was not searchable: {initial:#}"
    );
    let initial_by_history_source = json_output(ctx(&temp).args([
        "search",
        "hermes plugin initial marker",
        "--history-source",
        "hermes/default",
        "--refresh",
        "off",
        "--json",
    ]));
    let source_filtered_result = &initial_by_history_source["results"][0];
    assert_eq!(source_filtered_result["provider"], "custom");
    assert_eq!(source_filtered_result["history_source"], "hermes/default");
    assert_eq!(source_filtered_result["history_source_plugin"], "hermes");
    assert_eq!(source_filtered_result["provider_key"], "hermes");
    assert_eq!(source_filtered_result["source_id"], "default");
    assert_eq!(source_filtered_result["source_format"], "hermes-history-v1");

    let second = json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args([
                "import",
                "--history-source",
                "hermes/default",
                "--json",
                "--progress",
                "none",
            ]),
    );
    assert_eq!(second["totals"]["imported_sessions"], 0);
    assert_eq!(second["totals"]["imported_events"], 1);
    assert_eq!(second["resume"], false);
    assert_eq!(second["resume_mode"], "normal_scan");

    let incremental = json_output(ctx(&temp).args([
        "search",
        "hermes plugin incremental marker",
        "--provider",
        "custom",
        "--refresh",
        "off",
        "--json",
    ]));
    assert!(
        !incremental["results"].as_array().unwrap().is_empty(),
        "incremental plugin import was not searchable: {incremental:#}"
    );
    let cursor_log = fs::read_to_string(cursor_log).unwrap();
    assert!(cursor_log.contains(r#""message_id":7"#), "{cursor_log}");
    assert!(cursor_log.contains("cursor_file="), "{cursor_log}");
}
