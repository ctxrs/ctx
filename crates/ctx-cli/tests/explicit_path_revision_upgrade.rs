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
                && status["daemon"]["core_refresh_endpoint"]["available"] == true
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

fn write_codex_session(source: &Path, message: &str) {
    fs::write(
        source,
        [
            json!({
                "timestamp": "2026-07-18T12:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": "explicit-codex-source-revision",
                    "timestamp": "2026-07-18T12:00:00Z",
                    "cwd": "/workspace/ctx",
                    "originator": "codex-cli"
                }
            }),
            json!({
                "timestamp": "2026-07-18T12:00:01Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "id": "explicit-revision-message",
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
        .collect::<String>(),
    )
    .unwrap();
}

fn explicit_import(temp: &TempDir, source: &Path) -> Value {
    json_output(ctx(temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        source.to_str().unwrap(),
        "--no-daemon",
        "--progress",
        "none",
        "--format=json",
    ]))
}

fn assert_published_codex_source(report: &Value, source: &Path) -> String {
    assert_eq!(report["schema_version"], 2, "{report:#}");
    assert_eq!(report["outcome"], "success", "{report:#}");
    assert_eq!(
        report["totals"]["current_rejected_records"], 0,
        "{report:#}"
    );
    assert!(report["totals"]["current_indexed_documents"]
        .as_u64()
        .is_some_and(|count| count >= 1));
    for unsupported_delta in [
        "failed_sources",
        "imported_sources",
        "imported_sessions",
        "imported_events",
        "rejected_records",
    ] {
        assert!(
            report["totals"].get(unsupported_delta).is_none(),
            "unsupported per-run delta {unsupported_delta} appeared in {report:#}"
        );
    }
    assert_eq!(report["sources"][0]["status"], "published", "{report:#}");
    assert_eq!(report["sources"][0]["provider"], "codex", "{report:#}");
    assert_eq!(
        report["sources"][0]["source_format"], "codex_session_jsonl",
        "{report:#}"
    );
    assert_eq!(
        report["sources"][0]["path"],
        fs::canonicalize(source).unwrap().display().to_string(),
        "{report:#}"
    );
    assert_eq!(
        report["sources"][0]["daemon_request_metadata"]["owner"], "daemon",
        "{report:#}"
    );
    report["sources"][0]["published_generation"]
        .as_str()
        .expect("explicit import must publish a source-backed generation")
        .to_owned()
}

fn search_codex(temp: &TempDir, query: &str) -> Value {
    json_output(ctx(temp).args([
        "search",
        query,
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--format=json",
    ]))
}

fn assert_source_backed_codex_search(search: &Value, query: &str) {
    assert_eq!(search["schema_version"], 1, "{search:#}");
    assert_eq!(search["query"], query, "{search:#}");
    assert_eq!(search["filters"]["provider"], "codex", "{search:#}");
    assert_eq!(search["retrieval"]["index"], "core", "{search:#}");
    let results = search["results"].as_array().unwrap();
    assert_eq!(results.len(), 1, "{search:#}");
    assert_eq!(results[0]["provider"], "codex", "{search:#}");
    assert!(results[0]["ctx_event_id"].is_string(), "{search:#}");
    assert!(results[0]["ctx_session_id"].is_string(), "{search:#}");
    assert!(
        results[0]["snippet"]
            .as_str()
            .is_some_and(|snippet| snippet.contains(query)),
        "{search:#}"
    );
}

#[test]
fn explicit_codex_source_revision_republishes_source_backed_generation() {
    let temp = tempdir();
    let _daemon = start_source_refresh_daemon(&temp);
    let source = temp.path().join("explicit-codex-source.jsonl");
    write_codex_session(&source, "explicit source revision before");

    let first = explicit_import(&temp, &source);
    let first_generation = assert_published_codex_source(&first, &source);
    assert_eq!(first["sources"][0]["catalog_changed"], true, "{first:#}");
    let before = search_codex(&temp, "explicit source revision before");
    assert_eq!(before["retrieval"]["index"], "core", "{before:#}");
    assert_eq!(
        before["retrieval"]["generation_id"], first_generation,
        "{before:#}"
    );
    assert_source_backed_codex_search(&before, "explicit source revision before");
    let before_result = &before["results"][0];
    let before_event = before_result["ctx_event_id"].clone();
    let before_session = before_result["ctx_session_id"].clone();

    let unchanged = explicit_import(&temp, &source);
    let unchanged_generation = assert_published_codex_source(&unchanged, &source);
    assert_eq!(
        unchanged["sources"][0]["catalog_changed"], false,
        "{unchanged:#}"
    );
    assert_eq!(unchanged_generation, first_generation);

    write_codex_session(&source, "explicit source revision after!");
    let revised = explicit_import(&temp, &source);
    let revised_generation = assert_published_codex_source(&revised, &source);
    assert_eq!(
        revised["sources"][0]["catalog_changed"], false,
        "{revised:#}"
    );
    assert_ne!(revised_generation, first_generation);

    let after = search_codex(&temp, "explicit source revision after");
    assert_eq!(after["retrieval"]["index"], "core", "{after:#}");
    assert_eq!(
        after["retrieval"]["generation_id"], revised_generation,
        "{after:#}"
    );
    assert_source_backed_codex_search(&after, "explicit source revision after");
    assert_eq!(after["results"][0]["ctx_event_id"], before_event);
    assert_eq!(after["results"][0]["ctx_session_id"], before_session);

    let superseded = search_codex(&temp, "explicit source revision before");
    assert!(
        superseded["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|result| !result["snippet"]
                .as_str()
                .unwrap_or_default()
                .contains("explicit source revision before")),
        "{superseded:#}"
    );
}

#[cfg(target_os = "windows")]
#[test]
fn windows_explicit_codex_file_publishes_from_the_local_temp_directory() {
    let temp = tempdir();
    let _daemon = start_source_refresh_daemon(&temp);
    let source = temp.path().join("windows-local-codex-session.jsonl");
    write_codex_session(&source, "windows local explicit source oracle");

    let imported = explicit_import(&temp, &source);
    assert_published_codex_source(&imported, &source);
    let search = search_codex(&temp, "windows local explicit source oracle");
    assert_eq!(search["retrieval"]["index"], "core", "{search:#}");
    assert_source_backed_codex_search(&search, "windows local explicit source oracle");
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "source-backed explicit import must not create the previous-epoch Store"
    );
}
