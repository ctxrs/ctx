use std::{
    io::Read,
    process::{Child, Command as StdCommand, Stdio},
};

use super::*;

pub(super) struct SourceRefreshDaemon {
    child: Option<Child>,
}

impl Drop for SourceRefreshDaemon {
    fn drop(&mut self) {
        if let Err(error) =
            terminate_and_reap_test_child(&mut self.child, "native-provider source-refresh daemon")
        {
            if std::thread::panicking() {
                eprintln!("native-provider daemon teardown also failed: {error}");
            } else {
                panic!("native-provider daemon teardown failed: {error}");
            }
        }
    }
}

pub(super) fn start_isolated_provider_daemon(temp: &TempDir) -> SourceRefreshDaemon {
    let data_root = data_root(temp);
    fs::create_dir_all(&data_root).unwrap();
    fs::write(
        data_root.join("config.toml"),
        "[daemon]\nenabled = true\nmode = \"full\"\n\n[search]\nsemantic = false\n",
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
        .current_dir(temp.path())
        .args([
            "daemon",
            "run",
            "--force",
            "--idle-exit-seconds",
            "600",
            "--loop-interval-seconds",
            "600",
        ])
        .env("CTX_DAEMON_MODE", "full")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let child = command
        .spawn()
        .unwrap_or_else(|error| panic!("start isolated source-refresh daemon: {error}"));
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
            wait_for_test_daemon_source_refresh(temp);
            return daemon;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for source-refresh daemon readiness: {status:#?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

pub(super) fn source_backed_count(temp: &TempDir, sql: &str) -> i64 {
    let deadline = Instant::now() + Duration::from_secs(60);
    let packet = loop {
        let output = ctx(temp)
            .args(["sql", sql, "--format=json"])
            .output()
            .unwrap();
        if output.status.success() {
            let packet = serde_json::from_slice::<Value>(&output.stdout).unwrap();
            if packet["snapshot"]["stale"] == true && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(25));
                continue;
            }
            assert_ne!(
                packet["snapshot"]["stale"], true,
                "source-backed SQL projection stayed stale for `{sql}`: {packet:#}"
            );
            break packet;
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if (stderr.contains("Core SQL projection")
            || stderr.contains("source-backed SQL projection")
            || stderr.contains("source-backed relational projection")
            || stderr.contains("no such table: source_backed_relational_state"))
            && Instant::now() < deadline
        {
            if let Ok(job) = fs::read(data_root(temp).join("daemon/jobs/relational-catch-up.json"))
                .and_then(|bytes| {
                    serde_json::from_slice::<Value>(&bytes).map_err(std::io::Error::other)
                })
            {
                if job["status"] == "error" {
                    panic!(
                        "source-backed SQL projection failed for `{sql}` ({}): {}",
                        job["error_code"].as_str().unwrap_or("unknown_error"),
                        job["last_error"]
                            .as_str()
                            .unwrap_or("unknown projection error")
                    );
                }
            }
            std::thread::sleep(Duration::from_millis(25));
            continue;
        }
        panic!("source-backed SQL failed for `{sql}`: {stderr}");
    };
    packet["rows"][0][0]
        .as_i64()
        .unwrap_or_else(|| panic!("expected integer SQL scalar in {packet:#}"))
}

pub(super) fn wait_for_imported_projections(temp: &TempDir, packet: &Value) {
    let generation = packet["sources"][0]["published_generation"]
        .as_str()
        .unwrap_or_else(|| panic!("import packet omitted published generation: {packet:#}"));
    wait_for_test_lexical_projection(temp, generation);
    wait_for_test_relational_projection(temp, generation);
}

pub(super) fn assert_source_backed_search(search: &Value, provider: &str, query: &str) {
    assert_eq!(search["schema_version"], 1, "{search:#}");
    assert_eq!(search["query"], query, "{search:#}");
    assert_eq!(search["filters"]["provider"], provider, "{search:#}");
    assert_eq!(search["retrieval"]["index"], "core", "{search:#}");
    let results = search["results"].as_array().unwrap();
    assert!(!results.is_empty(), "{search:#}");
    for result in results {
        assert_eq!(result["provider"], provider, "{search:#}");
        assert!(result["ctx_event_id"].is_string(), "{search:#}");
        assert!(result["ctx_session_id"].is_string(), "{search:#}");
        assert!(
            result["snippet"]
                .as_str()
                .is_some_and(|snippet| snippet.contains(query)),
            "{search:#}"
        );
    }
}
