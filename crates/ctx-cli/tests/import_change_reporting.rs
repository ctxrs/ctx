mod support;

use std::{
    io::{Read, Write},
    process::{Child, Command as StdCommand, Stdio},
};

use support::*;

fn command_env_value(command: &Command, name: &str) -> Option<String> {
    command
        .get_envs()
        .find(|(candidate, _)| *candidate == name)
        .and_then(|(_, value)| value)
        .map(|value| value.to_string_lossy().into_owned())
}

#[test]
fn bound_test_ctx_binary_preserves_ordinary_command_policy() {
    let temp = tempdir();
    let ordinary = ctx(&temp);
    let binary = bind_test_ctx_binary(&temp);
    let bound = ctx(&temp);

    assert_eq!(Path::new(bound.get_program()), binary);
    assert_ne!(ordinary.get_program(), bound.get_program());
    for name in [
        "CTX_ANALYTICS_ENABLED",
        "CTX_LOCAL_USAGE_ENABLED",
        "CTX_DAEMON_AUTOSTART_OFF",
        "CTX_DAEMON_AUTOSTART_IDLE_EXIT_SECONDS",
    ] {
        assert_eq!(
            command_env_value(&ordinary, name),
            command_env_value(&bound, name),
            "{name} changed while binding the copied test binary"
        );
    }
}

struct SourceRefreshDaemon {
    child: Option<Child>,
}

impl Drop for SourceRefreshDaemon {
    fn drop(&mut self) {
        if let Err(error) =
            terminate_and_reap_test_child(&mut self.child, "import-change source-refresh daemon")
        {
            if std::thread::panicking() {
                eprintln!("import-change daemon teardown also failed: {error}");
            } else {
                panic!("import-change daemon teardown failed: {error}");
            }
        }
    }
}

fn data_root(temp: &TempDir) -> PathBuf {
    temp.path().join("data")
}

fn home_root(temp: &TempDir) -> PathBuf {
    temp.path().join("home")
}

fn state_root(temp: &TempDir) -> PathBuf {
    temp.path().join("state")
}

fn prepare_test_roots(temp: &TempDir) {
    fs::create_dir_all(data_root(temp)).unwrap();
    fs::create_dir_all(home_root(temp).join(".codex/sessions")).unwrap();
    fs::create_dir_all(state_root(temp)).unwrap();
}

fn isolated_ctx(temp: &TempDir) -> Command {
    let mut command = ctx(temp);
    command
        .env("CTX_DATA_ROOT", data_root(temp))
        .env("HOME", home_root(temp))
        .env("XDG_STATE_HOME", state_root(temp))
        .env("LOCALAPPDATA", state_root(temp));
    command
}

fn isolated_ctx_from_binary(temp: &TempDir, binary: &Path) -> Command {
    let mut command = ctx_from_binary(temp, binary);
    command
        .env("CTX_DATA_ROOT", data_root(temp))
        .env("HOME", home_root(temp))
        .env("XDG_STATE_HOME", state_root(temp))
        .env("LOCALAPPDATA", state_root(temp));
    command
}

fn start_source_refresh_daemon(temp: &TempDir) -> SourceRefreshDaemon {
    bind_test_ctx_binary(temp);
    prepare_test_roots(temp);
    fs::write(
        data_root(temp).join("config.toml"),
        "[daemon]\nenabled = true\nmode = \"source-refresh-only\"\n\n[search]\nsemantic = false\n",
    )
    .unwrap();
    let binary = copied_ctx_binary(temp);
    let prepared = isolated_ctx_from_binary(temp, &binary);
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
        let status = isolated_ctx(temp)
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

fn codex_source(message: &str) -> String {
    [
        json!({
            "timestamp": "2026-07-23T01:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": "import-change-reporting",
                "timestamp": "2026-07-23T01:00:00Z",
                "cwd": "/workspace/project",
                "originator": "codex-cli"
            }
        }),
        json!({
            "timestamp": "2026-07-23T01:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "id": "message-one",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": message
                }]
            }
        }),
    ]
    .into_iter()
    .map(|record| format!("{}\n", serde_json::to_string(&record).unwrap()))
    .collect()
}

fn import_codex(temp: &TempDir) -> Value {
    let events_path = temp.path().join("analytics.jsonl");
    let output = isolated_ctx(temp)
        .args(["import", "--all"])
        .args(["--no-daemon", "--format=json", "--progress", "none"])
        .env("CTX_ANALYTICS_ENABLED", "true")
        .env("CTX_ANALYTICS_DEBUG", "1")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("CTX_UPGRADE_AUTO", "off")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "import failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        events_path.exists(),
        "analytics sender did not create {}: {}",
        events_path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn assert_change(report: &Value, expected: &str) {
    assert_eq!(report["totals"]["change"], expected, "{report:#}");
    assert_eq!(report["sources"][0]["change"], expected, "{report:#}");
}

fn assert_current_generation(
    report: &Value,
    source_count: u64,
    indexed_documents: u64,
    rejected_records: u64,
) {
    assert_eq!(
        report["totals"]["current_source_count"], source_count,
        "{report:#}"
    );
    assert_eq!(
        report["totals"]["current_indexed_documents"], indexed_documents,
        "{report:#}"
    );
    assert_eq!(
        report["totals"]["current_rejected_records"], rejected_records,
        "{report:#}"
    );
    assert_eq!(
        report["sources"][0]["current_source_count"], source_count,
        "{report:#}"
    );
    assert_eq!(
        report["sources"][0]["current_indexed_documents"], indexed_documents,
        "{report:#}"
    );
    assert_eq!(
        report["sources"][0]["current_rejected_records"], rejected_records,
        "{report:#}"
    );
    for key in [
        "source_files",
        "source_bytes",
        "imported_sources",
        "sources_completed_with_rejections",
        "failed_sources",
        "imported_sessions",
        "imported_events",
        "imported_edges",
        "skipped_sessions",
        "skipped_events",
        "skipped_edges",
        "skipped",
        "rejected_records",
    ] {
        assert!(
            report["totals"].get(key).is_none(),
            "unsupported per-run total {key} was synthesized: {report:#}"
        );
        assert!(
            report["sources"][0].get(key).is_none(),
            "unsupported per-run source fact {key} was synthesized: {report:#}"
        );
    }
}

fn latest_source_refresh_properties(temp: &TempDir) -> serde_json::Map<String, Value> {
    let event = read_analytics_events(&temp.path().join("analytics.jsonl"))
        .last()
        .and_then(|payload| payload["events"].as_array())
        .and_then(|events| {
            events
                .iter()
                .find(|event| event["event_name"] == "provider_refresh_completed")
        })
        .cloned()
        .expect("provider-neutral source refresh analytics event");
    assert_eq!(event["surface"], "cli", "{event:#}");
    assert_eq!(event["operation"], "refresh", "{event:#}");
    event["properties"]
        .as_object()
        .cloned()
        .expect("provider-neutral source refresh analytics properties")
}

#[test]
fn codex_reimport_rebuilds_from_provider_source() {
    let temp = tempdir();
    let _daemon = start_source_refresh_daemon(&temp);
    let source_root = home_root(&temp).join(".codex/sessions");
    let source = source_root.join("2026/07/23/rollout-2026-07-23T01-00-00-source-authority.jsonl");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, codex_source("ctxsupersededtoken")).unwrap();

    let initial = import_codex(&temp);
    assert_change(&initial, "changed");
    assert_eq!(initial["outcome"], "success", "{initial:#}");
    assert_current_generation(&initial, 1, 1, 0);
    assert!(initial["sources"][0].get("previous_generation").is_none());
    assert_eq!(initial["sources"][0]["generation_changed"], true);
    let initial_generation = initial["sources"][0]["published_generation"]
        .as_str()
        .unwrap()
        .to_owned();
    let initial_analytics = latest_source_refresh_properties(&temp);
    assert_eq!(initial_analytics["change"], "changed");
    assert!(initial_analytics.get("provider").is_none());
    assert!(initial_analytics.get("events_bucket").is_none());
    assert!(initial_analytics.get("rejections_bucket").is_none());
    let unchanged = import_codex(&temp);
    assert_change(&unchanged, "no_op");
    assert_eq!(unchanged["outcome"], "success", "{unchanged:#}");
    assert_current_generation(&unchanged, 1, 1, 0);
    assert_eq!(
        unchanged["sources"][0]["previous_generation"],
        initial_generation
    );
    assert_eq!(
        unchanged["sources"][0]["published_generation"],
        initial_generation
    );
    assert_eq!(unchanged["sources"][0]["generation_changed"], false);
    assert_eq!(latest_source_refresh_properties(&temp)["change"], "no_op");

    let rewritten_source = fs::read_to_string(&source)
        .unwrap()
        .replace("ctxsupersededtoken", "ctxreplacementtext");
    assert_eq!(
        rewritten_source.len(),
        fs::metadata(&source).unwrap().len() as usize
    );
    let mut source_file = fs::OpenOptions::new().write(true).open(&source).unwrap();
    source_file.write_all(rewritten_source.as_bytes()).unwrap();
    source_file.sync_all().unwrap();

    let replay = import_codex(&temp);
    assert_change(&replay, "changed");
    assert_eq!(replay["outcome"], "success", "{replay:#}");
    assert_current_generation(&replay, 1, 1, 0);
    assert_eq!(
        replay["sources"][0]["previous_generation"],
        initial_generation
    );
    assert_ne!(
        replay["sources"][0]["published_generation"],
        initial_generation
    );
    assert_eq!(replay["sources"][0]["generation_changed"], true);
    let search = json_output(isolated_ctx(&temp).args([
        "search",
        "ctxreplacementtext",
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(search["retrieval"]["index"], "core", "{search:#}");
    let results = search["results"].as_array().unwrap();
    assert_eq!(results.len(), 1, "{search:#}");
    assert_eq!(results[0]["provider"], "codex");
    assert!(results[0]["snippet"]
        .as_str()
        .is_some_and(|text| text.contains("ctxreplacementtext")));
    let superseded = json_output(isolated_ctx(&temp).args([
        "search",
        "ctxsupersededtoken",
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert!(
        superseded["results"].as_array().unwrap().is_empty(),
        "{superseded:#}"
    );
    let with_rejection = format!(
        "{}{{malformed json}}\n",
        fs::read_to_string(&source).unwrap()
    );
    fs::write(&source, with_rejection).unwrap();
    let rejected = import_codex(&temp);
    assert_change(&rejected, "changed");
    assert_eq!(rejected["outcome"], "success", "{rejected:#}");
    assert_current_generation(&rejected, 1, 1, 1);
    assert!(rejected["totals"].get("rejected_records").is_none());
    assert!(rejected["totals"]
        .get("sources_completed_with_rejections")
        .is_none());
    assert!(rejected["sources"][0].get("failure_scope").is_none());
    assert!(rejected["sources"][0].get("failure_type").is_none());
    assert!(
        rejected["sources"][0].get("rejections").is_none(),
        "unavailable per-record details must not be synthesized: {rejected:#}"
    );
    let rejection_analytics = latest_source_refresh_properties(&temp);
    assert_eq!(rejection_analytics["refresh_result"], "complete");
    assert_eq!(rejection_analytics["failure_scope"], "none");
    assert_eq!(rejection_analytics["failure_type"], "none");
    assert!(rejection_analytics.get("source_mode").is_none());

    let rejected_generation = rejected["sources"][0]["published_generation"]
        .as_str()
        .unwrap()
        .to_owned();
    fs::remove_file(&source).unwrap();
    let deleted = import_codex(&temp);
    assert_change(&deleted, "changed");
    assert_eq!(deleted["outcome"], "success", "{deleted:#}");
    assert_current_generation(&deleted, 0, 0, 0);
    assert_eq!(
        deleted["sources"][0]["previous_generation"],
        rejected_generation
    );
    assert_ne!(
        deleted["sources"][0]["published_generation"],
        rejected_generation
    );
    assert_eq!(deleted["sources"][0]["generation_changed"], true);
    assert_eq!(deleted["sources"][0]["removed_source_count"], 1);
    assert_eq!(deleted["totals"]["removed_source_count"], 1);
}

#[test]
fn discovered_codex_session_tree_reports_admitted_source_format() {
    let tree_temp = tempdir();
    let _daemon = start_source_refresh_daemon(&tree_temp);
    let tree = home_root(&tree_temp).join(".codex/sessions");
    fs::create_dir_all(tree.join("2026/07/23")).unwrap();
    fs::write(
        tree.join("2026/07/23/rollout-2026-07-23T01-00-00-tree-dispatch.jsonl"),
        codex_source("tree dispatch"),
    )
    .unwrap();
    let tree_report = import_codex(&tree_temp);
    assert_eq!(tree_report["outcome"], "success", "{tree_report:#}");
    assert_eq!(
        tree_report["sources"][0]["source_format"],
        "provider_authoritative_all"
    );
    assert_eq!(tree_report["sources"][0]["certified_source_count"], 1);
    assert!(tree_report["sources"][0]["published_generation"].is_string());
    assert_eq!(tree_report["totals"]["change"], "changed");
    assert!(tree_report["totals"].get("imported_sessions").is_none());
    assert!(tree_report["totals"].get("imported_events").is_none());
    assert_eq!(tree_report["totals"]["current_indexed_documents"], 1);
}
