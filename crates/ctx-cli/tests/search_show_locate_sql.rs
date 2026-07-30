mod support;

use std::{
    io::Read,
    process::{Child, Command as StdCommand, Stdio},
};

use support::{daemon_test_root as tempdir, *};

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
    start_source_refresh_daemon_with_env(temp, &[])
}

fn start_source_refresh_daemon_with_env(
    temp: &TempDir,
    extra_env: &[(&str, &Path)],
) -> SourceRefreshDaemon {
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
    for (name, value) in extra_env {
        command.env(name, value);
    }
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

fn source_backed_count(temp: &TempDir, sql: &str) -> i64 {
    let deadline = Instant::now() + Duration::from_secs(60);
    let packet = loop {
        let output = ctx(temp)
            .args(["sql", sql, "--format=json"])
            .output()
            .unwrap();
        if output.status.success() {
            break serde_json::from_slice::<Value>(&output.stdout).unwrap();
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if (stderr.contains("source-backed SQL projection")
            || stderr.contains("source-backed relational projection")
            || stderr.contains("no such table: source_backed_relational_state"))
            && Instant::now() < deadline
        {
            if let Ok(job) = fs::read(temp.path().join("daemon/jobs/relational-catch-up.json"))
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

#[test]
fn search_result_window_is_truthful_in_json_and_human_output() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");
    let (_daemon, imported) = import_codex_fixture_through_daemon(&temp, &fixture);
    assert!(imported["sources"][0]["published_generation"].is_string());

    let limited = json_output(ctx(&temp).args([
        "search",
        "search",
        "--provider",
        "codex",
        "--include-subagents",
        "--limit",
        "1",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(limited["results"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        limited["result_window"],
        json!({
            "limit": 1,
            "returned": 1,
            "more_available": true,
        })
    );
    assert!(limited.get("pagination").is_none(), "{limited:#}");
    assert!(limited["truncation"]["candidate_pool"].is_number());
    assert!(limited["truncation"]["candidate_pool_truncated"].is_boolean());
    assert!(limited["result_window"].get("candidate_pool").is_none());

    let limited_human = ctx(&temp)
        .args([
            "search",
            "search",
            "--provider",
            "codex",
            "--include-subagents",
            "--limit",
            "1",
            "--refresh",
            "off",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let limited_human = String::from_utf8(limited_human).unwrap();
    assert_eq!(limited_human.matches("More results available.").count(), 1);
    assert!(
        limited_human.ends_with("More results available.\n"),
        "{limited_human}"
    );

    let complete = json_output(ctx(&temp).args([
        "search",
        "search",
        "--provider",
        "codex",
        "--include-subagents",
        "--limit",
        "200",
        "--refresh",
        "off",
        "--format=json",
    ]));
    let returned = complete["results"].as_array().map(Vec::len).unwrap();
    assert!(returned > 1, "{complete:#}");
    assert_eq!(
        complete["result_window"],
        json!({
            "limit": 200,
            "returned": returned,
            "more_available": false,
        })
    );

    let complete_human = ctx(&temp)
        .args([
            "search",
            "search",
            "--provider",
            "codex",
            "--include-subagents",
            "--limit",
            "200",
            "--refresh",
            "off",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let complete_human = String::from_utf8(complete_human).unwrap();
    assert!(!complete_human.contains("More results available."));
}

fn measured_json_output(command: &mut Command) -> (Value, usize) {
    let output = command.assert().success().get_output().clone();
    let output_bytes = output.stdout.len();
    let value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("invalid JSON output: {error}: {output:#?}"));
    (value, output_bytes)
}

#[path = "support/search_show_locate_sql/import_paths.rs"]
mod import_paths;

#[test]
fn search_excludes_active_codex_session_by_default_when_available() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");
    json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--format=json",
        "--progress",
        "none",
    ]));

    let excluded = json_output(
        ctx(&temp)
            .env("CODEX_THREAD_ID", "codex-session-root")
            .args([
                "search",
                "onboarding",
                "--provider",
                "codex",
                "--refresh",
                "off",
                "--format=json",
            ]),
    );
    assert_eq!(excluded["results"].as_array().unwrap().len(), 0);
    assert!(excluded["filters"]["include_current_session"].is_null());

    let excluded_tree = json_output(
        ctx(&temp)
            .env("CODEX_THREAD_ID", "codex-session-root")
            .args([
                "search",
                "local history search",
                "--provider",
                "codex",
                "--refresh",
                "off",
                "--format=json",
            ]),
    );
    assert_eq!(
        excluded_tree["results"].as_array().unwrap().len(),
        0,
        "active session tree was not excluded: {excluded_tree:#}"
    );

    let included = json_output(
        ctx(&temp)
            .env("CODEX_THREAD_ID", "codex-session-root")
            .args([
                "search",
                "onboarding",
                "--provider",
                "codex",
                "--refresh",
                "off",
                "--include-current-session",
                "--format=json",
            ]),
    );
    assert_search_provider_oracle(&included, "codex", "onboarding", 1, "message");
    assert_eq!(included["filters"]["include_current_session"], true);

    let included_tree = json_output(
        ctx(&temp)
            .env("CODEX_THREAD_ID", "codex-session-root")
            .args([
                "search",
                "local history search",
                "--provider",
                "codex",
                "--refresh",
                "off",
                "--include-current-session",
                "--format=json",
            ]),
    );
    assert!(!included_tree["results"].as_array().unwrap().is_empty());
}

#[test]
fn sql_reads_generation_only_projection_and_supports_formats_and_input_sources() {
    let temp = tempdir();
    let generation_id = initialize_generation_only_sql_projection(temp.path());
    assert!(!temp.path().join("work.sqlite").exists());

    let json =
        json_output(ctx(&temp).args(["sql", "SELECT 1 AS one, 'two' AS two", "--format=json"]));
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["payload_type"], "sql_result");
    assert_eq!(json["read_only"], true);
    assert_eq!(json["share_safe"], false);
    assert_eq!(json["columns"], json!(["one", "two"]));
    assert_eq!(json["rows"], json!([[1, "two"]]));
    assert_eq!(json["returned_rows"], 1);

    let metadata = json_output(ctx(&temp).args([
        "sql",
        "SELECT core_generation_id, status FROM ctx_projection_metadata",
        "--format=json",
    ]));
    assert_eq!(metadata["rows"], json!([[generation_id, "ready"]]));

    let query_file = temp.path().join("query.sql");
    fs::write(&query_file, "SELECT 'a,b' AS value, 2 AS n").unwrap();
    let csv_output = ctx(&temp)
        .arg("sql")
        .arg("--file")
        .arg(&query_file)
        .args(["--format", "csv"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        String::from_utf8(csv_output).unwrap(),
        "value,n\n\"a,b\",2\n"
    );

    let oversized_file_stderr = failure_stderr(
        ctx(&temp)
            .arg("sql")
            .arg("--file")
            .arg(&query_file)
            .args(["--max-sql-bytes", "4"]),
    );
    assert!(
        oversized_file_stderr.contains("exceeds max_sql_bytes (4)"),
        "{oversized_file_stderr}"
    );

    let oversized_stdin_stderr = ctx(&temp)
        .args(["sql", "-", "--max-sql-bytes", "4"])
        .write_stdin("SELECT 1")
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let oversized_stdin_stderr = String::from_utf8(oversized_stdin_stderr).unwrap();
    assert!(
        oversized_stdin_stderr.contains("exceeds max_sql_bytes (4)"),
        "{oversized_stdin_stderr}"
    );

    let raw_output = ctx(&temp)
        .args(["sql", "-", "--format", "raw"])
        .write_stdin("SELECT 'abc' AS value")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(String::from_utf8(raw_output).unwrap(), "abc\n");

    let unsupported = failure_stderr(ctx(&temp).args(["sql", "SELECT * FROM events"]));
    assert!(
        unsupported.contains("no such table: events"),
        "{unsupported}"
    );
    assert!(!temp.path().join("work.sqlite").exists());
}

#[test]
fn fresh_sql_is_read_only_and_initializes_no_legacy_store() {
    let temp = tempdir();
    let json = json_output(ctx(&temp).args(["sql", "SELECT 1 AS one", "--format=json"]));
    assert_eq!(json["rows"], json!([[1]]));
    assert!(!temp.path().join("work.sqlite").exists());
    assert!(temp.path().join("relational.sqlite").is_file());

    let stderr = failure_stderr(ctx(&temp).args(["sql", "CREATE TABLE nope(x INTEGER)"]));
    assert!(stderr.contains("SQL query must be read-only"));
    let conn = Connection::open(temp.path().join("relational.sqlite")).unwrap();
    assert_eq!(
        sqlite_count(
            &conn,
            "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name = 'nope'"
        ),
        0
    );
    assert!(!temp.path().join("work.sqlite").exists());

    let stderr = failure_stderr(ctx(&temp).args(["sql", "SELECT 1; SELECT 2"]));
    assert!(stderr.contains("Multiple statements provided"));
}

#[test]
fn show_does_not_initialize_store() {
    let temp = tempdir();
    let stderr = failure_stderr(ctx(&temp).args(["show", "event", "deadbeef"]));
    assert!(stderr.contains("source-backed Core index is not initialized"));
    assert!(!temp.path().join("work.sqlite").exists());
}

#[test]
fn locate_does_not_initialize_store() {
    let temp = tempdir();
    let stderr = failure_stderr(ctx(&temp).args(["locate", "event", "deadbeef"]));
    assert!(
        stderr.contains("source-backed Core index is not initialized"),
        "{stderr}"
    );
    assert!(!temp.path().join("work.sqlite").exists());
}

#[test]
fn fresh_home_search_mvp_flow() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");

    ctx(&temp)
        .arg("setup")
        .assert()
        .success()
        .stdout(predicate::str::contains("Daemon is running"));
    assert!(
        !temp.path().join("config.toml").exists(),
        "setup should not write config.toml for implicit defaults"
    );

    let setup_json = json_output(ctx(&temp).args(["setup", "--format=json"]));
    assert_eq!(setup_json["schema_version"], 2);
    assert_eq!(setup_json["network_required"], false);
    assert_eq!(setup_json["repo_writes"], false);
    assert!(
        matches!(setup_json["mode"].as_str(), Some("pending" | "ready")),
        "{setup_json:#}"
    );
    assert_eq!(setup_json["daemon_autostart"]["status"], "degraded");
    assert_eq!(setup_json["daemon_autostart"]["requested"], true);
    assert!(setup_json.get("background_indexing").is_none());
    wait_for_test_daemon_source_refresh(&temp);

    let sources = json_output(ctx(&temp).args(["sources", "--format=json"]));
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
        "--format=json",
    ]));
    assert_explicit_source_publication(&import, "codex", "codex_session_jsonl_tree");
    assert!(import["totals"]["source_files"].as_u64().unwrap() > 0);
    assert!(import["totals"]["source_bytes"].as_u64().unwrap() > 0);

    let search = json_output(ctx(&temp).args([
        "search",
        "onboarding",
        "--provider",
        "codex",
        "--format=json",
    ]));
    assert_eq!(search["schema_version"], 1);
    assert_omits_keys(
        &search,
        &[
            "record_id",
            "history_record_id",
            "raw_source_path",
            "kind",
            "external_session_id",
        ],
    );
    let first_result = &search["results"][0];
    assert_eq!(first_result["result_type"], "session_result");
    assert_eq!(first_result["result_scope"], "session");
    let ctx_event_id = first_result["ctx_event_id"].as_str().unwrap().to_owned();
    let ctx_session_id = first_result["ctx_session_id"].as_str().unwrap().to_owned();
    assert!(first_result["provider_session_id"].is_string());
    assert!(first_result["source_path"].is_string());
    assert_session_suggested_next_commands(first_result);
    assert!(first_result["citations"][0]["ctx_event_id"].is_string());
    assert!(first_result["citations"][0]["ctx_session_id"].is_string());

    let term_search = json_output(ctx(&temp).args([
        "search",
        "zzzz-no-match",
        "--term",
        "onboarding",
        "--provider",
        "codex",
        "--format=json",
    ]));
    assert_eq!(term_search["query"], "zzzz-no-match OR onboarding");
    assert!(!term_search["results"].as_array().unwrap().is_empty());
    for result in term_search["results"].as_array().unwrap() {
        assert_session_suggested_next_commands(result);
    }

    let event_search = json_output(ctx(&temp).args([
        "search",
        "onboarding",
        "--provider",
        "codex",
        "--events",
        "--format=json",
    ]));
    assert_event_search_provider_oracle(&event_search, "codex", "onboarding", 1, "message");

    let session_events = json_output(ctx(&temp).args([
        "search",
        "onboarding",
        "--provider",
        "codex",
        "--session",
        &ctx_session_id,
        "--format=json",
    ]));
    assert_event_search_provider_oracle(&session_events, "codex", "onboarding", 1, "message");
    assert_eq!(session_events["filters"]["session"], ctx_session_id);
    assert!(session_events["results"]
        .as_array()
        .unwrap()
        .iter()
        .all(|result| result["ctx_session_id"] == ctx_session_id));

    let session_prefix = &ctx_session_id[..8];
    let prefixed_session_events = json_output(ctx(&temp).args([
        "search",
        "onboarding",
        "--provider",
        "codex",
        "--session",
        session_prefix,
        "--format=json",
    ]));
    assert_event_search_provider_oracle(
        &prefixed_session_events,
        "codex",
        "onboarding",
        1,
        "message",
    );
    assert_eq!(
        prefixed_session_events["filters"]["session"],
        session_prefix
    );

    let human_search = ctx(&temp)
        .args(["search", "onboarding"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let human_search = String::from_utf8(human_search).unwrap();
    assert!(human_search.contains("1. "));
    assert!(human_search.contains("codex | session "));
    assert!(human_search.contains("inspect: ctx show session"));
    assert!(!human_search.contains("ctx_event_id"));
    assert!(!human_search.contains("provider_session_id"));
    assert!(!human_search.contains("next:"));
    assert!(!human_search.contains("work_record"));
    assert!(!human_search.contains("history_record"));

    let verbose_search = ctx(&temp)
        .args(["search", "onboarding", "--verbose"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let verbose_search = String::from_utf8(verbose_search).unwrap();
    assert!(verbose_search.contains("ctx_event_id"));
    assert!(verbose_search.contains("ctx_session_id"));
    assert!(verbose_search.contains("provider_session_id"));
    assert!(verbose_search.contains("session_importance"));
    assert!(verbose_search.contains("next: ctx show session"));
    assert!(verbose_search.contains("next: ctx show event"));
    assert!(verbose_search.contains("next: ctx search onboarding --session"));
    assert!(!human_search.contains("work_record"));
    assert!(!human_search.contains("history_record"));

    let file_search = json_output(ctx(&temp).args([
        "search",
        "--file",
        "crates/foo/src/lib.rs",
        "--format=json",
    ]));
    assert_eq!(file_search["query"], "");
    assert!(file_search["results"].is_array());

    let oversized_after = failure_stderr(ctx(&temp).args([
        "show",
        "event",
        &ctx_event_id,
        "--after",
        "18446744073709551615",
    ]));
    assert!(
        oversized_after.contains("event window must be between 0 and 50"),
        "{oversized_after}"
    );

    let oversized_window = failure_stderr(ctx(&temp).args([
        "show",
        "event",
        &ctx_event_id,
        "--window",
        "18446744073709551615",
    ]));
    assert!(
        oversized_window.contains("event window must be between 0 and 50"),
        "{oversized_window}"
    );

    let locate_event =
        json_output(ctx(&temp).args(["locate", "event", &ctx_event_id, "--format=json"]));
    assert_eq!(locate_event["schema_version"], 1);
    assert_eq!(locate_event["payload_type"], "event_location");
    assert_eq!(locate_event["ctx_event_id"], ctx_event_id);
    assert_eq!(locate_event["ctx_session_id"], ctx_session_id);
    assert_eq!(locate_event["provider"], "codex");
    assert!(locate_event["provider_session_id"].is_string());
    assert!(locate_event["source"]["path"].is_string());

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(status["schema_version"], 2);
    assert!(status["indexed_items"].as_u64().unwrap() > 0);
    assert_eq!(status["semantic"]["status"], "disabled");
    assert_eq!(status["semantic"]["reason"], "semantic_disabled");
    assert_eq!(status["daemon"]["enabled"], true);
    assert!(status["daemon"]["jobs"]["source_backed_refresh"]["status"].is_string());

    let doctor_deadline = Instant::now() + Duration::from_secs(10);
    let doctor = loop {
        let doctor = json_output(ctx(&temp).args(["doctor", "--format=json"]));
        if doctor["ok"] == true {
            break doctor;
        }
        assert!(
            Instant::now() < doctor_deadline,
            "timed out waiting for healthy source epochs: {doctor:#}"
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(doctor["schema_version"], 1);
    assert_eq!(doctor["ok"], true);
    assert_eq!(doctor["daemon"]["enabled"], true);
    assert_eq!(doctor["source_epoch"]["lexical"]["status"], "ready");
    assert!(doctor["findings"].as_array().unwrap().is_empty());
}

#[test]
fn foreground_core_observations_are_truthful_and_recorded_once_after_output() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");
    json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--format=json",
        "--progress",
        "none",
    ]));

    let (search, search_output_bytes) =
        measured_json_output(ctx(&temp).env_remove("CTX_LOCAL_USAGE_ENABLED").args([
            "search",
            "onboarding",
            "--provider",
            "codex",
            "--refresh",
            "off",
            "--format=json",
        ]));
    let search_results = search["results"].as_array().unwrap();
    let search_result_count = search_results.len();
    let delivered_context_bytes = search_results
        .iter()
        .map(|result| result["snippet"].as_str().unwrap().len())
        .sum::<usize>();
    let ctx_event_id = search_results[0]["ctx_event_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let ctx_session_id = search_results[0]["ctx_session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let (empty_search, empty_search_output_bytes) =
        measured_json_output(ctx(&temp).env_remove("CTX_LOCAL_USAGE_ENABLED").args([
            "search",
            "no-result-marker-7f8a",
            "--provider",
            "codex",
            "--refresh",
            "off",
            "--format=json",
        ]));
    assert!(empty_search["results"].as_array().unwrap().is_empty());

    let (shown_session, show_session_output_bytes) =
        measured_json_output(ctx(&temp).env_remove("CTX_LOCAL_USAGE_ENABLED").args([
            "show",
            "session",
            &ctx_session_id,
            "--format=json",
        ]));
    assert!(!shown_session["events"].as_array().unwrap().is_empty());

    let (shown_event, show_event_output_bytes) =
        measured_json_output(ctx(&temp).env_remove("CTX_LOCAL_USAGE_ENABLED").args([
            "show",
            "event",
            &ctx_event_id,
            "--format=json",
        ]));
    assert!(!shown_event["events"].as_array().unwrap().is_empty());

    let (_, locate_session_output_bytes) =
        measured_json_output(ctx(&temp).env_remove("CTX_LOCAL_USAGE_ENABLED").args([
            "locate",
            "session",
            &ctx_session_id,
            "--format=json",
        ]));
    let (_, locate_event_output_bytes) =
        measured_json_output(ctx(&temp).env_remove("CTX_LOCAL_USAGE_ENABLED").args([
            "locate",
            "event",
            &ctx_event_id,
            "--format=json",
        ]));

    let (sources, sources_output_bytes) = measured_json_output(
        ctx(&temp)
            .env_remove("CTX_LOCAL_USAGE_ENABLED")
            .args(["sources", "--format=json"]),
    );
    assert!(!sources["sources"].as_array().unwrap().is_empty());

    let (sql, sql_output_bytes) =
        measured_json_output(ctx(&temp).env_remove("CTX_LOCAL_USAGE_ENABLED").args([
            "sql",
            "SELECT 1 AS value UNION ALL SELECT 2",
            "--format=json",
        ]));
    assert_eq!(sql["returned_rows"], 2);

    let blocked_parent = temp.path().join("blocked-show-output");
    fs::write(&blocked_parent, "not a directory").unwrap();
    ctx(&temp)
        .env_remove("CTX_LOCAL_USAGE_ENABLED")
        .arg("show")
        .arg("session")
        .arg(&ctx_session_id)
        .arg("--format=json")
        .arg("--out")
        .arg(blocked_parent.join("session.json"))
        .assert()
        .failure();

    let connection = Connection::open(temp.path().join("usage.sqlite")).unwrap();
    let totals = |operation: &str, outcome: &str, value_class: &str| {
        connection
            .query_row(
                r#"
                SELECT
                    SUM(calls),
                    SUM(result_count),
                    SUM(citation_count),
                    SUM(delivered_output_bytes),
                    SUM(delivered_context_bytes),
                    SUM(matched_normalized_session_bytes)
                FROM daily_usage
                WHERE surface = 'cli'
                  AND operation = ?1
                  AND outcome = ?2
                  AND value_class = ?3
                "#,
                params![operation, outcome, value_class],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .unwrap()
    };

    let search_complete: (String, i64) = connection
        .query_row(
            "SELECT context_coverage, matched_normalized_session_bytes \
             FROM daily_usage \
             WHERE surface = 'cli' AND operation = 'search' \
               AND value_class = 'result_bearing'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(search_complete.0, "complete");
    assert!(search_complete.1 >= delivered_context_bytes as i64);
    assert_eq!(
        totals("search", "success", "result_bearing"),
        (
            1,
            search_result_count as i64,
            0,
            search_output_bytes as i64,
            delivered_context_bytes as i64,
            search_complete.1,
        )
    );
    assert_eq!(
        totals("search", "success", "empty"),
        (1, 0, 0, empty_search_output_bytes as i64, 0, 0)
    );
    assert_eq!(
        totals("show_session", "success", "not_applicable"),
        (1, 0, 0, show_session_output_bytes as i64, 0, 0)
    );
    assert_eq!(
        totals("show_event", "success", "not_applicable"),
        (1, 0, 0, show_event_output_bytes as i64, 0, 0)
    );
    assert_eq!(
        totals("locate", "success", "not_applicable"),
        (
            2,
            0,
            0,
            locate_session_output_bytes.saturating_add(locate_event_output_bytes) as i64,
            0,
            0,
        )
    );
    assert_eq!(
        totals("sources", "success", "not_applicable"),
        (1, 0, 0, sources_output_bytes as i64, 0, 0)
    );
    assert_eq!(
        totals("sql", "success", "not_applicable"),
        (1, 0, 0, sql_output_bytes as i64, 0, 0)
    );
    assert_eq!(
        totals("show_session", "failure", "not_applicable"),
        (1, 0, 0, 0, 0, 0)
    );
}

#[test]
fn search_backend_defaults_and_supported_semantic_config_are_reported() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");
    json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--format=json",
        "--progress",
        "none",
    ]));

    let default_search = json_output(ctx(&temp).args([
        "search",
        "semantic-only-missing-sidecar",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(default_search["retrieval"]["requested_mode"], "lexical");
    assert_eq!(default_search["retrieval"]["effective_mode"], "lexical");
    assert!(default_search["retrieval"]["semantic_fallback_code"].is_null());

    let hybrid = json_output(ctx(&temp).args([
        "search",
        "onboarding",
        "--backend",
        "hybrid",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(hybrid["retrieval"]["requested_mode"], "hybrid");
    assert_eq!(hybrid["retrieval"]["effective_mode"], "lexical");
    assert_eq!(
        hybrid["retrieval"]["semantic_fallback_code"],
        "semantic_disabled"
    );

    let disabled_strict_semantic = ctx(&temp)
        .args([
            "search",
            "onboarding",
            "--backend",
            "semantic",
            "--refresh",
            "off",
            "--format=json",
        ])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let disabled_strict_semantic = String::from_utf8(disabled_strict_semantic).unwrap();
    assert!(
        disabled_strict_semantic.contains("semantic search is disabled"),
        "{disabled_strict_semantic}"
    );

    fs::write(
        temp.path().join("config.toml"),
        "[daemon]\nenabled = true\n\n[search]\nsemantic = true\n",
    )
    .unwrap();

    let supported_hybrid = json_output(ctx(&temp).args([
        "search",
        "onboarding",
        "--backend",
        "hybrid",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(supported_hybrid["retrieval"]["requested_mode"], "hybrid");
    assert_eq!(supported_hybrid["retrieval"]["effective_mode"], "lexical");
    assert_eq!(
        supported_hybrid["retrieval"]["semantic_fallback_code"],
        "semantic_store_missing"
    );

    let missing_index_strict_semantic = ctx(&temp)
        .args([
            "search",
            "onboarding",
            "--backend",
            "semantic",
            "--refresh",
            "off",
            "--format=json",
        ])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let missing_index_strict_semantic = String::from_utf8(missing_index_strict_semantic).unwrap();
    assert!(
        missing_index_strict_semantic.contains("semantic_store_missing"),
        "{missing_index_strict_semantic}"
    );

    let explicit_lexical = json_output(ctx(&temp).args([
        "search",
        "onboarding",
        "--backend",
        "lexical",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(explicit_lexical["retrieval"]["requested_mode"], "lexical");
    assert_eq!(explicit_lexical["retrieval"]["effective_mode"], "lexical");

    let status = json_output(ctx(&temp).args(["index", "status", "--format=json"]));
    assert_eq!(status["semantic"]["status"], "pending");
    assert!(
        matches!(
            status["semantic"]["reason"].as_str(),
            Some(
                "flat_f32_projection_missing"
                    | "projection_control_missing"
                    | "generation_not_acknowledged"
            )
        ),
        "{status:#}"
    );
    assert_eq!(status["semantic"]["enabled"], true);
    assert_eq!(status["semantic"]["config_source"], "config");
}

#[test]
fn doctor_reports_missing_store_without_creating_it() {
    let temp = tempdir();

    let doctor = json_output(ctx(&temp).args(["doctor", "--format=json"]));

    assert_eq!(doctor["schema_version"], 1);
    assert_eq!(doctor["ok"], false);
    assert_eq!(doctor["daemon"]["enabled"], true);
    assert!(doctor["source_epoch"]["lexical"]["status"].is_string());
    assert!(doctor["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|finding| { finding.as_str().unwrap().starts_with("lexical is ") }));
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "doctor should not create the ctx store"
    );
}

#[test]
fn codex_cli_resume_is_idempotent_rescan_and_filters_subagents() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");
    let _daemon = start_source_refresh_daemon(&temp);

    let first = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--no-daemon",
        "--format=json",
    ]));
    assert_explicit_source_publication(&first, "codex", "codex_session_jsonl_tree");
    assert_eq!(first["resume"], false);
    assert_eq!(first["resume_mode"], "normal_scan");
    wait_for_test_daemon_source_refresh(&temp);
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_sessions WHERE provider = 'codex'"
        ),
        2
    );
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_events WHERE provider = 'codex'"
        ),
        7
    );

    let primary_default =
        json_output(ctx(&temp).args(["search", "subagent", "--refresh", "off", "--format=json"]));
    assert!(primary_default["filters"]["include_subagents"].is_null());
    let primary_default_text = serde_json::to_string(&primary_default).unwrap();
    assert!(
        !primary_default_text.contains("codex-session-child"),
        "{primary_default_text}"
    );

    let default_events = json_output(ctx(&temp).args([
        "search",
        "subagent",
        "--events",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert!(default_events["filters"]["include_subagents"].is_null());
    let default_events_text = serde_json::to_string(&default_events).unwrap();
    assert!(
        !default_events_text.contains("codex-session-child"),
        "{default_events_text}"
    );

    let with_subagents = json_output(ctx(&temp).args([
        "search",
        "subagent",
        "--include-subagents",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert!(!with_subagents["results"].as_array().unwrap().is_empty());
    assert_eq!(with_subagents["filters"]["include_subagents"], true);
    assert!(serde_json::to_string(&with_subagents)
        .unwrap()
        .contains("codex-session-child"));

    let child_session_lookup = json_output(ctx(&temp).args([
        "sql",
        "SELECT ctx_session_id FROM ctx_sessions WHERE provider_session_id = 'codex-session-child'",
        "--format",
        "json",
    ]));
    let child_session_id = child_session_lookup["rows"][0][0].as_str().unwrap();
    let explicit_child_session = json_output(ctx(&temp).args([
        "search",
        "subagent",
        "--session",
        child_session_id,
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(
        explicit_child_session["filters"]["session"],
        child_session_id
    );
    assert!(serde_json::to_string(&explicit_child_session)
        .unwrap()
        .contains("codex-session-child"));

    let primary_only = json_output(ctx(&temp).args([
        "search",
        "subagent",
        "--primary-only",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert!(primary_only["filters"]["include_subagents"].is_null());
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
        "--no-daemon",
        "--format=json",
    ]));
    assert_explicit_source_publication(&second, "codex", "codex_session_jsonl_tree");
    assert_eq!(second["resume"], true);
    assert_eq!(second["resume_mode"], "idempotent_rescan");
    assert_eq!(second["totals"]["skipped"], 0);
    assert_eq!(second["sources"][0]["catalog_changed"], false, "{second:#}");
}

#[test]
fn search_rejects_unbounded_limit() {
    let temp = tempdir();
    ctx(&temp)
        .args(["search", "anything", "--limit", "201"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid value"));
}

#[test]
fn codex_cli_default_import_uses_catalog_state_for_incremental_catch_up() {
    let temp = tempdir();
    let _daemon = start_source_refresh_daemon(&temp);
    let fixture = provider_history_fixture("codex-sessions");

    let first = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--no-daemon",
        "--format=json",
    ]));
    assert_explicit_source_publication(&first, "codex", "codex_session_jsonl_tree");
    assert_eq!(first["resume"], false);
    assert_eq!(first["resume_mode"], "normal_scan");
    assert_eq!(first["totals"]["rejected_records"], 0);
    let first_generation = first["sources"][0]["published_generation"]
        .as_str()
        .unwrap()
        .to_owned();

    let status_deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        let status = json_output(ctx(&temp).args(["status", "--format=json"]));
        if status["indexed_sessions"] == 2
            && status["indexed_events"] == 7
            && status["indexed_sources"] == 2
            && status["lexical"]["status"] == "ready"
        {
            break status;
        }
        assert!(
            Instant::now() < status_deadline,
            "timed out waiting for imported catalog state: {status:#}"
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(status["indexed_sessions"], 2);
    assert_eq!(status["indexed_events"], 7);
    assert_eq!(status["indexed_sources"], 2);
    assert_eq!(status["lexical"]["status"], "ready");

    let second = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--no-daemon",
        "--format=json",
    ]));
    assert_explicit_source_publication(&second, "codex", "codex_session_jsonl_tree");
    assert_eq!(second["resume"], false);
    assert_eq!(second["resume_mode"], "normal_scan");
    assert_eq!(second["totals"]["skipped"], 0);
    assert_eq!(second["totals"]["rejected_records"], 0);
    assert_eq!(second["sources"][0]["catalog_changed"], false, "{second:#}");
    assert_eq!(
        second["sources"][0]["published_generation"], first_generation,
        "{second:#}"
    );
}

#[test]
fn codex_cli_provider_oracle_covers_retrieval_and_claimed_fidelity() {
    let temp = tempdir();
    let _daemon = start_source_refresh_daemon(&temp);
    let fixture = temp.path().join("combined-codex-sessions");
    copy_dir_all(
        Path::new(&provider_history_fixture("codex-sessions")),
        &fixture,
    );
    copy_dir_all(
        Path::new(&provider_history_fixture("codex-rich-sessions")),
        &fixture,
    );

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        fixture.to_str().unwrap(),
        "--no-daemon",
        "--format=json",
    ]));
    assert_explicit_source_publication(&imported, "codex", "codex_session_jsonl_tree");

    let query = "setup flow";
    let search = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_source_backed_search(&search, "codex", query);

    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_sessions WHERE provider = 'codex' AND fidelity = 'imported'"
        ),
        3
    );
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_events WHERE provider = 'codex' AND fidelity = 'imported'"
        ),
        12
    );
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_events WHERE provider = 'codex' AND event_type = 'message' AND role = 'user'"
        ),
        3
    );
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_events WHERE provider = 'codex' AND event_type = 'message' AND role = 'assistant'"
        ),
        2
    );
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_events WHERE provider = 'codex' AND event_type = 'tool_call'"
        ),
        4
    );
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_events WHERE provider = 'codex' AND event_type = 'tool_output'"
        ),
        0
    );
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_events WHERE provider = 'codex' AND event_type = 'command_output'"
        ),
        0
    );
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM pragma_table_info('ctx_events') WHERE name = 'payload_json'"
        ),
        0,
        "the metadata-only relational projection must expose no payload column"
    );
    assert_eq!(
        source_backed_count(&temp, "SELECT COUNT(*) FROM ctx_files_touched"),
        1
    );
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_projection_metadata WHERE status = 'ready'"
        ),
        1
    );
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "Codex acceptance must use the source-backed generation and relational projection"
    );
}

include!("support/search_show_locate_sql/pi_flow.rs");
include!("support/search_show_locate_sql/search_filters.rs");
