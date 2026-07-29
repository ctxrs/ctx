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
    json_output(
        ctx(temp)
            .args(["import", "--all"])
            .args(["--no-daemon", "--format=json", "--progress", "none"])
            .env_remove("CTX_ANALYTICS_ENABLED")
            .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
            .env("CTX_UPGRADE_AUTO", "off"),
    )
}

fn assert_change(report: &Value, expected: &str) {
    assert_eq!(report["outcome"], "success", "{report:#}");
    assert_eq!(report["totals"]["change"], expected, "{report:#}");
    assert_eq!(report["sources"][0]["change"], expected, "{report:#}");
}

#[test]
fn codex_reimport_preserves_opaque_prior_epoch_and_rebuilds_from_provider_source() {
    let temp = tempdir();
    let _daemon = start_source_refresh_daemon(&temp);
    let source_root = temp.path().join(".codex/sessions");
    let source = source_root.join("2026/07/23/rollout-2026-07-23T01-00-00-source-authority.jsonl");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, codex_source("provider authority before")).unwrap();

    let data_root = temp.path();
    let legacy_path = data_root.join("work.sqlite");
    let legacy_bytes = b"opaque v0.25 prior-epoch rollback sentinel\n";
    fs::write(&legacy_path, legacy_bytes).unwrap();

    let initial = import_codex(&temp);
    assert_change(&initial, "changed");
    assert_eq!(fs::read(&legacy_path).unwrap(), legacy_bytes);

    let rewritten_source = fs::read_to_string(&source)
        .unwrap()
        .replace("provider authority before", "provider authority after!");
    fs::write(&source, rewritten_source).unwrap();

    let replay = import_codex(&temp);
    assert_change(&replay, "changed");
    assert_eq!(replay["totals"]["rejected_records"], 0, "{replay:#}");
    assert_eq!(fs::read(&legacy_path).unwrap(), legacy_bytes);

    let search = json_output(ctx(&temp).args([
        "search",
        "provider authority after",
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(search["retrieval"]["index"], "source_backed", "{search:#}");
    let results = search["results"].as_array().unwrap();
    assert_eq!(results.len(), 1, "{search:#}");
    assert_eq!(results[0]["provider"], "codex");
    assert!(results[0]["snippet"]
        .as_str()
        .is_some_and(|text| text.contains("provider authority after")));
    let superseded = json_output(ctx(&temp).args([
        "search",
        "provider authority before",
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert!(superseded["results"].as_array().unwrap().is_empty());
    assert_eq!(fs::read(&legacy_path).unwrap(), legacy_bytes);
}

#[test]
fn discovered_codex_session_tree_reports_admitted_source_format() {
    let tree_temp = tempdir();
    let _daemon = start_source_refresh_daemon(&tree_temp);
    let tree = tree_temp.path().join(".codex/sessions");
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
    assert_eq!(tree_report["totals"]["imported_sessions"], 0);
    assert_eq!(tree_report["totals"]["imported_events"], 1);
}
