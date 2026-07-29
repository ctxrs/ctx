mod support;

use std::{
    io::Read,
    process::{Child, Command as StdCommand, Stdio},
};

use support::*;

struct SourceRefreshDaemon {
    child: Option<Child>,
}

impl Drop for SourceRefreshDaemon {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn start_source_refresh_daemon(temp: &TempDir) -> SourceRefreshDaemon {
    fs::write(
        temp.path().join("config.toml"),
        "[daemon]\nenabled = true\nmode = \"source-refresh-only\"\n\n[search]\nsemantic = false\n",
    )
    .unwrap();
    let binary = copied_ctx_binary(temp);
    let prepared = ctx_from_binary(temp, &binary);
    let mut command = StdCommand::new(prepared.get_program());
    for (name, value) in prepared.get_envs() {
        match value {
            Some(value) => {
                command.env(name, value);
            }
            None => {
                command.env_remove(name);
            }
        }
    }
    command
        .args([
            "daemon",
            "run",
            "--force",
            "--idle-exit-seconds",
            "600",
            "--loop-interval-seconds",
            "600",
        ])
        .env("CTX_DAEMON_MODE", "source-refresh-only")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let spawn_deadline = Instant::now() + Duration::from_secs(1);
    let child = loop {
        match command.spawn() {
            Ok(child) => break child,
            Err(error) if error.raw_os_error() == Some(26) && Instant::now() < spawn_deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("start isolated source-refresh daemon: {error}"),
        }
    };
    let mut daemon = SourceRefreshDaemon { child: Some(child) };
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(exit) = daemon.child.as_mut().unwrap().try_wait().unwrap() {
            let mut stderr = String::new();
            daemon
                .child
                .as_mut()
                .unwrap()
                .stderr
                .as_mut()
                .unwrap()
                .read_to_string(&mut stderr)
                .unwrap();
            panic!("source-refresh daemon exited before becoming ready ({exit}): {stderr}");
        }
        let status = ctx(temp)
            .args(["daemon", "status", "--format=json"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| serde_json::from_slice::<Value>(&output.stdout).ok());
        if status.as_ref().is_some_and(|status| {
            status["daemon"]["running"] == true
                && status["daemon"]["source_refresh_endpoint"]["available"] == true
        }) {
            return daemon;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for source-refresh daemon readiness: {status:#?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn import_plugin(
    temp: &TempDir,
    plugin: &HistorySourcePluginFixture,
    history_source: &str,
) -> Value {
    json_output(
        ctx(temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args([
                "import",
                "--history-source",
                history_source,
                "--no-daemon",
                "--progress",
                "none",
                "--format=json",
            ]),
    )
}

fn search_plugin(temp: &TempDir, query: &str, identity_args: &[&str]) -> Value {
    let mut command = ctx(temp);
    command.args(["search", query]);
    command.args(identity_args);
    command.args(["--refresh", "off", "--format=json"]);
    json_output(&mut command)
}

fn first_result(value: &Value) -> &Value {
    value["results"]
        .as_array()
        .and_then(|results| results.first())
        .expect("expected one plugin search result")
}

#[test]
fn valid_history_source_plugin_is_truthfully_available_without_discovery_execution() {
    let temp = tempdir();
    let plugin =
        write_history_source_plugin_with_refresh(&temp, "dorkos", true, Some("auto"), None);
    let manifest_path = plugin.manifest_dir.join("ctx-history-plugin.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["name"] = json!("status-plugin");
    manifest["history_sources"][0]["id"] = json!("primary");
    manifest["history_sources"][0]["provider_key"] = json!("status-provider");
    manifest["history_sources"][0]["source_id"] = json!("archive");
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let sources = json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args(["sources", "--format=json"]),
    );
    let plugin_source = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["plugin_source"] == "status-plugin/primary")
        .unwrap();
    assert_eq!(plugin_source["kind"], "history_source_plugin");
    assert_eq!(plugin_source["history_source"], "status-provider/archive");
    assert_eq!(plugin_source["status"], "available");
    assert_eq!(plugin_source["importable"], true);
    assert_eq!(plugin_source["import_mode"], "explicit_source_backed");
    assert_eq!(plugin_source["provider_source_authority"], true);
    assert!(plugin_source["unsupported_reason"].is_null());
    assert!(!plugin.run_marker.exists());

    let human = ctx(&temp)
        .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
        .args(["sources"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let human = String::from_utf8(human).unwrap();
    assert!(human.contains("status-plugin/primary available"), "{human}");
    assert!(human.contains("explicit source-backed import"), "{human}");
    assert!(!plugin.run_marker.exists());
}

#[test]
fn invalid_oversized_and_missing_plugin_manifests_fail_closed() {
    let temp = tempdir();
    let plugin_root = temp.path().join("history-plugins");
    let empty = plugin_root.join("empty");
    let invalid = plugin_root.join("invalid");
    let oversized = plugin_root.join("oversized");
    fs::create_dir_all(&empty).unwrap();
    fs::create_dir_all(&invalid).unwrap();
    fs::create_dir_all(&oversized).unwrap();
    fs::write(
        empty.join("ctx-history-plugin.json"),
        r#"{"schema_version":1,"name":"empty","history_sources":[]}"#,
    )
    .unwrap();
    let invalid_manifest = invalid.join("ctx-history-plugin.json");
    fs::write(&invalid_manifest, "{not-json").unwrap();
    fs::write(
        oversized.join("ctx-history-plugin.json"),
        vec![b' '; 2 * 1024 * 1024],
    )
    .unwrap();

    let sources = json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin_root)
            .args(["sources", "--format=json"]),
    );
    let failures = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|source| source["kind"] == "history_source_plugin" && source["status"] == "invalid")
        .collect::<Vec<_>>();
    assert_eq!(failures.len(), 3);
    assert!(failures.iter().all(|source| source["importable"] == false));
    assert!(failures.iter().any(|source| source["error"]
        .as_str()
        .unwrap()
        .contains("parse history source plugin manifest")));
    assert!(failures.iter().any(|source| source["error"]
        .as_str()
        .unwrap()
        .contains("exceeds max bytes")));
    assert!(failures.iter().any(|source| source["error"]
        .as_str()
        .unwrap()
        .contains("must declare at least one history source")));

    let invalid_error = failure_stderr(ctx(&temp).args([
        "import",
        "--history-source-manifest",
        invalid_manifest.to_str().unwrap(),
        "--no-daemon",
        "--progress",
        "none",
    ]));
    assert!(
        invalid_error.contains("parse history source plugin manifest"),
        "{invalid_error}"
    );

    let missing = temp.path().join("missing-plugin.json");
    let missing_error = failure_stderr(ctx(&temp).args([
        "import",
        "--history-source-manifest",
        missing.to_str().unwrap(),
        "--no-daemon",
        "--progress",
        "none",
    ]));
    assert!(
        missing_error.contains("import path does not exist"),
        "{missing_error}"
    );
    assert!(!temp.path().join("work.sqlite").exists());
    assert!(!temp.path().join("history-source-plugin-sources").exists());
}

#[test]
fn selected_plugin_cold_append_and_noop_publish_one_source_backed_epoch() {
    let temp = tempdir();
    let cursor_log = temp.path().join("cursor.log");
    let plugin = write_history_source_plugin_with_refresh(
        &temp,
        "dorkos",
        true,
        Some("auto"),
        Some(&cursor_log),
    );
    let manifest_path = plugin.manifest_dir.join("ctx-history-plugin.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["name"] = json!("dorkos-plugin");
    manifest["history_sources"][0]["id"] = json!("primary");
    manifest["history_sources"][0]["provider_key"] = json!("dorkos-provider");
    manifest["history_sources"][0]["source_id"] = json!("archive");
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let history_source = "dorkos-plugin/primary";
    let _daemon = start_source_refresh_daemon(&temp);

    let cold = json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args([
                "import",
                "--history-source-manifest",
                manifest_path.to_str().unwrap(),
                "--history-source",
                history_source,
                "--no-daemon",
                "--progress",
                "none",
                "--format=json",
            ]),
    );
    assert_eq!(cold["outcome"], "success", "{cold:#}");
    assert_eq!(cold["totals"]["imported_sources"], 1, "{cold:#}");
    assert_eq!(cold["totals"]["imported_sessions"], 1, "{cold:#}");
    assert_eq!(cold["totals"]["imported_events"], 1, "{cold:#}");
    assert_eq!(cold["sources"][0]["status"], "published", "{cold:#}");
    assert_eq!(
        cold["sources"][0]["history_source"], "dorkos-provider/archive",
        "{cold:#}"
    );
    assert_eq!(
        cold["sources"][0]["plugin_source"], history_source,
        "{cold:#}"
    );
    assert_eq!(cold["sources"][0]["work_kind"], "cold", "{cold:#}");
    assert_eq!(
        cold["sources"][0]["daemon_request_metadata"]["owner"], "daemon",
        "{cold:#}"
    );
    assert_eq!(
        cold["sources"][0]["provider_source_authority"], true,
        "{cold:#}"
    );
    assert_eq!(
        cold["sources"][0]["legacy_store_fallback"], false,
        "{cold:#}"
    );
    let cold_generation = cold["sources"][0]["published_generation"]
        .as_str()
        .unwrap()
        .to_owned();
    let snapshot = PathBuf::from(cold["sources"][0]["path"].as_str().unwrap());
    assert!(snapshot.is_file());
    assert!(!temp.path().join("work.sqlite").exists());

    let initial = search_plugin(
        &temp,
        "dorkos plugin initial marker",
        &["--history-source", "dorkos-provider/archive"],
    );
    assert_eq!(
        initial["retrieval"]["index"], "source_backed",
        "{initial:#}"
    );
    assert_eq!(
        initial["retrieval"]["generation_id"], cold_generation,
        "{initial:#}"
    );
    let initial_result = first_result(&initial);
    assert_eq!(initial_result["provider"], "custom", "{initial:#}");
    let initial_event_id = initial_result["ctx_event_id"].as_str().unwrap();

    let provider_key = search_plugin(
        &temp,
        "dorkos plugin initial marker",
        &[
            "--provider-key",
            "dorkos-provider",
            "--source-id",
            "archive",
        ],
    );
    assert_eq!(
        first_result(&provider_key)["ctx_event_id"],
        initial_event_id,
        "{provider_key:#}"
    );
    let wrong_source = search_plugin(
        &temp,
        "dorkos plugin initial marker",
        &["--provider-key", "dorkos-provider", "--source-id", "other"],
    );
    assert!(
        wrong_source["results"].as_array().unwrap().is_empty(),
        "{wrong_source:#}"
    );

    let initial_show = json_output(ctx(&temp).args([
        "show",
        "event",
        initial_event_id,
        "--content",
        "complete",
        "--format=json",
    ]));
    assert_eq!(
        initial_show["event"]["text"], "dorkos plugin initial marker",
        "{initial_show:#}"
    );
    assert_eq!(
        initial_show["event"]["content"]["origin"], "provider_source",
        "{initial_show:#}"
    );
    assert_eq!(
        initial_show["event"]["content"]["source_verified"], true,
        "{initial_show:#}"
    );

    let append = import_plugin(&temp, &plugin, history_source);
    assert_eq!(append["outcome"], "success", "{append:#}");
    assert_eq!(append["sources"][0]["work_kind"], "append", "{append:#}");
    assert_eq!(
        append["sources"][0]["generation_changed"], true,
        "{append:#}"
    );
    assert_eq!(append["totals"]["imported_events"], 1, "{append:#}");
    let append_generation = append["sources"][0]["published_generation"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(append_generation, cold_generation);

    let incremental = search_plugin(
        &temp,
        "dorkos plugin incremental marker",
        &["--history-source", "dorkos-provider/archive"],
    );
    assert_eq!(
        incremental["retrieval"]["generation_id"], append_generation,
        "{incremental:#}"
    );
    let incremental_event_id = first_result(&incremental)["ctx_event_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let no_op = import_plugin(&temp, &plugin, history_source);
    assert_eq!(no_op["outcome"], "success", "{no_op:#}");
    assert_eq!(no_op["sources"][0]["work_kind"], "no_op", "{no_op:#}");
    assert_eq!(
        no_op["sources"][0]["generation_changed"], false,
        "{no_op:#}"
    );
    assert_eq!(
        no_op["sources"][0]["published_generation"], append_generation,
        "{no_op:#}"
    );

    let cursor_log = fs::read_to_string(cursor_log).unwrap();
    assert!(cursor_log.contains("\"offset\":128"), "{cursor_log}");
    assert!(cursor_log.contains("\"offset\":256"), "{cursor_log}");
    let snapshot_body = fs::read_to_string(&snapshot).unwrap();
    assert!(
        snapshot_body.contains("dorkos plugin initial marker"),
        "{snapshot_body}"
    );
    assert!(
        snapshot_body.contains("dorkos plugin incremental marker"),
        "{snapshot_body}"
    );
    assert!(!temp.path().join("work.sqlite").exists());

    let tampered = snapshot_body.replace(
        "dorkos plugin incremental marker",
        "dorkos plugin tampered marker",
    );
    fs::write(&snapshot, tampered).unwrap();
    let hydration_error = failure_stderr(ctx(&temp).args([
        "show",
        "event",
        &incremental_event_id,
        "--content",
        "complete",
        "--format=json",
    ]));
    assert!(
        hydration_error.contains("source")
            || hydration_error.contains("locator")
            || hydration_error.contains("digest"),
        "{hydration_error}"
    );
    assert!(!temp.path().join("work.sqlite").exists());
}

#[test]
fn mismatched_plugin_output_does_not_publish_source_or_cursor_state() {
    let temp = tempdir();
    let script = r#"#!/usr/bin/env python3
import json
records = [
  {"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"},
  {"record_type":"source","source_id":"wrong","provider_key":"wrong","source_format":"wrong-v1"},
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
                "--no-daemon",
                "--progress",
                "none",
            ]),
    );
    assert!(stderr.contains("emitted source identity"), "{stderr}");
    assert!(!temp.path().join("work.sqlite").exists());
    let managed_root = temp.path().join("history-source-plugin-sources");
    let managed_files = if managed_root.exists() {
        fs::read_dir(managed_root)
            .unwrap()
            .flat_map(|provider| fs::read_dir(provider.unwrap().path()).unwrap())
            .flat_map(|route| fs::read_dir(route.unwrap().path()).unwrap())
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    assert!(
        managed_files
            .iter()
            .all(|path| path.file_name().unwrap() == "route.lock"),
        "{managed_files:#?}"
    );
}
