#[allow(unused_imports)]
use super::*;

#[derive(Debug)]
pub(crate) struct HistorySourcePluginFixture {
    pub(crate) manifest_dir: PathBuf,
    pub(crate) run_marker: PathBuf,
}

pub(crate) fn write_raw_history_source_plugin(
    temp: &TempDir,
    provider: &str,
    script_body: &str,
) -> HistorySourcePluginFixture {
    write_raw_history_source_plugin_with_options(temp, provider, script_body, false, None)
}

pub(crate) fn write_raw_history_source_plugin_with_options(
    temp: &TempDir,
    provider: &str,
    script_body: &str,
    enabled: bool,
    refresh: Option<&str>,
) -> HistorySourcePluginFixture {
    write_raw_history_source_plugin_with_options_and_timeout(
        temp,
        provider,
        script_body,
        enabled,
        refresh,
        10,
    )
}

pub(crate) fn write_raw_history_source_plugin_with_options_and_timeout(
    temp: &TempDir,
    provider: &str,
    script_body: &str,
    enabled: bool,
    refresh: Option<&str>,
    timeout_seconds: u64,
) -> HistorySourcePluginFixture {
    let manifest_dir = temp.path().join("history-plugins").join(provider);
    fs::create_dir_all(&manifest_dir).unwrap();
    let script = manifest_dir.join("export.py");
    let run_marker = manifest_dir.join("ran");
    fs::write(&script, script_body).unwrap();
    let mut source_manifest = json!({
        "id": "default",
        "provider_key": provider,
        "source_id": "default",
        "source_format": format!("{provider}-history-v1"),
        "enabled": enabled,
        "command": [python_command(), script.display().to_string()],
        "timeout_seconds": timeout_seconds
    });
    if let Some(refresh) = refresh {
        source_manifest["refresh"] = json!(refresh);
    }
    let manifest = json!({
        "schema_version": 1,
        "name": provider,
        "history_sources": [source_manifest]
    });
    fs::write(
        manifest_dir.join("ctx-history-plugin.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    HistorySourcePluginFixture {
        manifest_dir,
        run_marker,
    }
}

#[test]
pub(crate) fn history_source_plugins_are_listed_without_running() {
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
pub(crate) fn invalid_installed_history_source_plugin_is_listed_as_invalid() {
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
pub(crate) fn oversized_installed_history_source_plugin_is_listed_as_invalid() {
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
pub(crate) fn invalid_installed_history_source_plugin_does_not_block_valid_import() {
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
pub(crate) fn removed_history_source_plugin_aliases_and_legacy_discovery_are_ignored() {
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
pub(crate) fn setup_does_not_execute_enabled_history_source_plugins() {
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
pub(crate) fn failed_history_source_plugin_import_does_not_leave_record_metadata() {
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
pub(crate) fn history_source_plugin_rejects_mismatched_machine_id_before_import() {
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
pub(crate) fn history_source_plugin_rejects_oversized_stdout_line() {
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
pub(crate) fn search_refresh_history_source_filter_runs_only_matching_auto_plugin() {
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
pub(crate) fn search_refresh_strict_fails_on_history_source_plugin_failure() {
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
pub(crate) fn search_refresh_auto_failure_without_prior_store_fails_instead_of_serving_empty_index()
{
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
