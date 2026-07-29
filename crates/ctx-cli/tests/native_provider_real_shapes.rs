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

fn assert_source_backed_search(search: &Value, provider: &str, query: &str) {
    assert_eq!(search["schema_version"], 1, "{search:#}");
    assert_eq!(search["query"], query, "{search:#}");
    assert_eq!(search["filters"]["provider"], provider, "{search:#}");
    assert_eq!(search["retrieval"]["index"], "source_backed", "{search:#}");
    let results = search["results"].as_array().unwrap();
    assert_eq!(results.len(), 1, "{search:#}");
    assert_eq!(results[0]["provider"], provider, "{search:#}");
    assert!(results[0]["ctx_event_id"].is_string(), "{search:#}");
    assert!(results[0]["ctx_session_id"].is_string(), "{search:#}");
    assert!(
        results[0]["snippet"]
            .as_str()
            .is_some_and(|snippet| snippet.contains(query)),
        "{search:#}"
    );
}

fn source_backed_sql(temp: &TempDir, sql: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let output = ctx(temp)
            .args(["sql", sql, "--format=json"])
            .output()
            .unwrap();
        if output.status.success() {
            return serde_json::from_slice(&output.stdout).unwrap();
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("source-backed SQL projection") && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
            continue;
        }
        panic!("source-backed SQL failed: {stderr}");
    }
}

#[test]
fn codebuddy_cli_jsonl_imports_and_searches_through_public_cli() {
    let temp = tempdir();
    let query = "codebuddy-cli-real-shape-oracle";
    let path = write_native_codebuddy_cli_jsonl_fixture(&temp, query);

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codebuddy",
        "--path",
        &path,
        "--format=json",
    ]));
    assert_eq!(imported["schema_version"], 2);
    assert_eq!(imported["sources"][0]["provider"], "codebuddy");
    assert_eq!(
        imported["sources"][0]["source_format"],
        "codebuddy_history_json"
    );
    assert_eq!(imported["totals"]["rejected_records"], 0);
    assert_eq!(imported["totals"]["imported_sessions"], 1);
    assert_eq!(imported["totals"]["imported_events"], 2);

    let search = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "codebuddy",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(&search, "codebuddy", query, 1, "message");
}

#[test]
fn nanoclaw_import_preserves_text_timestamp_millis_and_integer_trigger() {
    let temp = tempdir();
    let query = "nanoclaw-real-text-timestamp-oracle";
    let path = write_native_nanoclaw_fixture(&temp, query);

    let central = Connection::open(Path::new(&path).join("data/v2.db")).unwrap();
    central
        .execute_batch(
            "update sessions
             set created_at = '2026-07-10T03:18:34.491Z',
                 last_active = '2026-07-10 03:19:51'",
        )
        .unwrap();
    let inbound = Connection::open(
        Path::new(&path)
            .join("data/v2-sessions/ag-1/session-1")
            .join("inbound.db"),
    )
    .unwrap();
    inbound
        .execute(
            "update messages_in set timestamp = ?1, trigger = 1 where id = 'in-1'",
            ["2026-07-10T03:18:34.491Z"],
        )
        .unwrap();
    let outbound = Connection::open(
        Path::new(&path)
            .join("data/v2-sessions/ag-1/session-1")
            .join("outbound.db"),
    )
    .unwrap();
    outbound
        .execute(
            "update messages_out set timestamp = ?1 where id = 'out-1'",
            ["2026-07-10 03:19:51"],
        )
        .unwrap();
    let (trigger_type, trigger_text): (String, String) = inbound
        .query_row(
            "select typeof(trigger), cast(trigger as text)
             from messages_in
             where id = 'in-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(trigger_type, "text");
    assert_eq!(trigger_text, "1");
    drop(central);
    drop(inbound);
    drop(outbound);

    let _daemon = start_source_refresh_daemon(&temp);
    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "nanoclaw",
        "--path",
        &path,
        "--no-daemon",
        "--format=json",
    ]));
    assert_eq!(imported["schema_version"], 2, "{imported:#}");
    assert_eq!(
        imported["sources"][0]["status"], "published",
        "{imported:#}"
    );
    assert_eq!(
        imported["sources"][0]["provider"], "nanoclaw",
        "{imported:#}"
    );
    assert_eq!(
        imported["sources"][0]["source_format"], "nanoclaw_project",
        "{imported:#}"
    );
    assert_eq!(imported["totals"]["rejected_records"], 0);
    assert_eq!(imported["totals"]["imported_events"], 0);
    assert!(
        imported["sources"][0]["published_generation"].is_string(),
        "{imported:#}"
    );

    let projection = source_backed_sql(
        &temp,
        "select occurred_at_ms from ctx_events \
         where provider = 'nanoclaw' and role = 'user'",
    );
    assert_eq!(projection["rows"], json!([[1_783_653_514_491_i64]]));
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "NanoClaw acceptance must use its provider-owned databases and source-backed projection"
    );

    let search = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "nanoclaw",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_source_backed_search(&search, "nanoclaw", query);
}
