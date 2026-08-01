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
    fs::create_dir_all(data_root(temp)).unwrap();
    fs::write(
        data_root(temp).join("config.toml"),
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

fn assert_source_backed_search(search: &Value, provider: &str, query: &str) {
    assert_eq!(search["schema_version"], 1, "{search:#}");
    assert_eq!(search["query"], query, "{search:#}");
    assert_eq!(search["filters"]["provider"], provider, "{search:#}");
    assert_eq!(search["retrieval"]["index"], "core", "{search:#}");
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
                "--include-subagents",
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
                "--include-subagents",
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
    let generation_id = initialize_generation_only_sql_projection(&data_root(&temp));
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
    assert!(data_root(&temp).join("relational.sqlite").is_file());

    let stderr = failure_stderr(ctx(&temp).args(["sql", "CREATE TABLE nope(x INTEGER)"]));
    assert!(stderr.contains("SQL query must be read-only"));
    let conn = Connection::open(data_root(&temp).join("relational.sqlite")).unwrap();
    assert_eq!(
        sqlite_count(
            &conn,
            "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name = 'nope'"
        ),
        0
    );
    assert!(!temp.path().join("work.sqlite").exists());

    let stderr = failure_stderr(ctx(&temp).args(["sql", "SELECT 1; SELECT 2"]));
    assert_eq!(
        stderr,
        concat!(
            "✗ One read-only SQL statement required\n",
            "ctx sql accepts one read-only statement per command. Run each statement separately.\n",
            "\n",
            "Hint: Resolve the issue and retry\n",
            "\n",
            "Next\n",
            "  ctx sql 'SELECT 1'\n",
        )
    );

    let ansi = failure_stderr(ctx(&temp).args(["--color=always", "sql", "SELECT 1; SELECT 2"]));
    assert!(ansi.as_bytes().contains(&0x1b));
    let mut stripped = anstream::StripStream::new(Vec::new());
    stripped.write_all(ansi.as_bytes()).unwrap();
    assert_eq!(String::from_utf8(stripped.into_inner()).unwrap(), stderr);

    for format in ["raw", "csv", "json"] {
        let machine_stderr = failure_stderr(ctx(&temp).args([
            "--color=always",
            "sql",
            "SELECT 1; SELECT 2",
            "--format",
            format,
        ]));
        assert_eq!(machine_stderr, "Error: Multiple statements provided\n");
    }
}

#[test]
fn bounded_sql_preview_has_copyable_human_recovery_and_unchanged_raw_controls() {
    let temp = tempdir();
    let sql = "SELECT 'abcdefgh' AS value UNION ALL SELECT 'ijklmnop'";
    let args = ["sql", sql, "--max-rows", "1", "--max-value-bytes", "4"];

    let plain = ctx(&temp)
        .args(args)
        .assert()
        .success()
        .get_output()
        .clone();
    assert_eq!(
        String::from_utf8(plain.stdout).unwrap(),
        "✓ 1 row returned\n\nResults\nvalue\nabcd...\n"
    );
    let plain_stderr = String::from_utf8(plain.stderr).unwrap();
    let recovery =
        "  ctx sql 'SELECT '\\''abcdefgh'\\'' AS value UNION ALL SELECT '\\''ijklmnop'\\'''";
    assert!(plain_stderr.contains(&format!("Next\n{recovery}\n")));
    assert_eq!(unicode_width::UnicodeWidthStr::width(recovery), 78);
    assert!(!plain_stderr.contains("--max-rows 2"));
    assert!(!plain_stderr.contains("--max-value-bytes 8"));

    let ansi = ctx(&temp)
        .arg("--color=always")
        .args(args)
        .assert()
        .success()
        .get_output()
        .clone();
    assert!(ansi.stderr.contains(&0x1b));
    let mut stripped = anstream::StripStream::new(Vec::new());
    stripped.write_all(&ansi.stderr).unwrap();
    assert_eq!(
        String::from_utf8(stripped.into_inner()).unwrap(),
        plain_stderr
    );

    let raw = ctx(&temp)
        .arg("--color=always")
        .args(args)
        .args(["--format", "raw"])
        .assert()
        .success()
        .get_output()
        .clone();
    assert_eq!(raw.stdout, b"abcd\n");
    assert_eq!(
        raw.stderr,
        concat!(
            "warning: rows truncated at 1; rerun with --max-rows for more\n",
            "warning: values truncated at 4 bytes; rerun with --max-value-bytes for more\n",
        )
        .as_bytes()
    );
}

#[test]
fn show_does_not_initialize_store() {
    let temp = tempdir();
    let stderr = failure_stderr(ctx(&temp).args(["show", "event", "deadbeef"]));
    assert!(stderr.contains("the Core index does not exist; retry with daemon refresh enabled"));
    assert!(!temp.path().join("work.sqlite").exists());
}

include!("support/search_show_locate_sql/search_flows.rs");

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
        .clone();
    assert!(
        disabled_strict_semantic.stdout.is_empty(),
        "{disabled_strict_semantic:#?}"
    );
    let disabled_strict_semantic: Value =
        serde_json::from_slice(&disabled_strict_semantic.stderr).unwrap();
    assert_eq!(
        disabled_strict_semantic["error_code"], "semantic_disabled",
        "{disabled_strict_semantic:#}"
    );
    assert_eq!(disabled_strict_semantic["retryable"], false);
    assert!(disabled_strict_semantic["detail"]
        .as_str()
        .is_some_and(|detail| detail.contains("semantic search is disabled")));

    fs::write(
        data_root(&temp).join("config.toml"),
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
        .clone();
    assert!(
        missing_index_strict_semantic.stdout.is_empty(),
        "{missing_index_strict_semantic:#?}"
    );
    let missing_index_strict_semantic: Value =
        serde_json::from_slice(&missing_index_strict_semantic.stderr).unwrap();
    assert!(
        matches!(
            missing_index_strict_semantic["error_code"].as_str(),
            Some("semantic_store_missing" | "semantic_generation_not_acknowledged")
        ),
        "{missing_index_strict_semantic:#}"
    );
    assert_eq!(missing_index_strict_semantic["retryable"], true);
    assert!(missing_index_strict_semantic["detail"]
        .as_str()
        .is_some_and(|detail| detail.contains("flat-F32")));

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
        8
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
    assert_eq!(second["totals"]["current_rejected_records"], 0);
    assert_noop_publication(&second);
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
    assert_eq!(first["totals"]["current_rejected_records"], 0);
    let first_generation = first["sources"][0]["published_generation"]
        .as_str()
        .unwrap()
        .to_owned();

    let status_deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        let status = json_output(ctx(&temp).args(["status", "--format=json"]));
        if status["indexed_sessions"] == 2
            && status["indexed_events"] == 8
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
    assert_eq!(status["indexed_events"], 8);
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
    assert_eq!(second["totals"]["current_rejected_records"], 0);
    assert_noop_publication(&second);
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
            "SELECT COUNT(*) FROM ctx_sessions WHERE provider = 'codex'"
        ),
        3
    );
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_events WHERE provider = 'codex'"
        ),
        15
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
        1
    );
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_events WHERE provider = 'codex' AND event_type = 'command_output'"
        ),
        2
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
        0,
        "unscoped fixture paths must not cross the Core repository boundary"
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
        "Codex acceptance must use the Core generation and relational projection"
    );
}

include!("support/search_show_locate_sql/pi_flow.rs");
include!("support/search_show_locate_sql/search_filters.rs");
