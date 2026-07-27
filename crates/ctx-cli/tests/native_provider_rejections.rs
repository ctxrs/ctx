mod support;

use support::*;

#[test]
fn antigravity_cli_import_skips_malformed_file_among_valid_files() {
    let temp = tempdir();
    let brain = write_antigravity_valid_and_malformed_file_tree(&temp);

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "antigravity",
        "--path",
        brain.to_str().unwrap(),
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(imported["totals"]["source_files"], 2, "{imported:#}");
    assert_eq!(imported["totals"]["imported_sessions"], 1, "{imported:#}");
    assert_eq!(imported["totals"]["imported_events"], 3, "{imported:#}");
    assert_eq!(imported["totals"]["rejected_records"], 1, "{imported:#}");
    assert_eq!(imported["totals"]["failed_sources"], 0, "{imported:#}");
    assert!(imported["sources"][0]["rejections"]
        .as_array()
        .unwrap()
        .iter()
        .any(|failure| failure["error"].as_str().unwrap().contains("agy-bad")));

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(status["source_import_files"], 1, "{status:#}");
    assert_eq!(status["indexed_source_import_files"], 1, "{status:#}");
    assert_eq!(status["failed_source_import_files"], 0, "{status:#}");
    assert_eq!(status["pending_source_import_files"], 0, "{status:#}");

    let search = json_output(ctx(&temp).args([
        "search",
        "write_to_file",
        "--provider",
        "antigravity",
        "--format=json",
    ]));
    assert_search_provider_oracle(&search, "antigravity", "write_to_file", 1, "tool_call");
}

#[test]
fn mixed_source_replay_remains_completed_with_rejections() {
    let temp = tempdir();
    let brain = write_antigravity_valid_and_malformed_file_tree(&temp);
    let path = brain.to_str().unwrap();

    let first = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "antigravity",
        "--path",
        path,
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(first["outcome"], "completed_with_rejections", "{first:#}");

    let replay = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "antigravity",
        "--path",
        path,
        "--resume",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(replay["outcome"], "completed_with_rejections", "{replay:#}");
    assert_eq!(replay["failure_scope"], "record", "{replay:#}");
    assert_eq!(replay["totals"]["failed_sources"], 0, "{replay:#}");
    assert_eq!(replay["totals"]["rejected_records"], 1, "{replay:#}");
    assert_eq!(
        replay["sources"][0]["status"], "completed_with_rejections",
        "{replay:#}"
    );
}

#[test]
fn firebender_replay_preserves_mixed_and_all_invalid_outcomes() {
    let mixed_temp = tempdir();
    let mixed_project =
        write_native_firebender_fixture(&mixed_temp, "firebender mixed rejection replay oracle");
    let mixed_database = Path::new(&mixed_project)
        .join(".idea")
        .join("firebender")
        .join("chat_history.db");
    let conn = Connection::open(&mixed_database).unwrap();
    conn.execute(
        "update chat_sessions set updated_at = 20 where id = 'firebender-fixture-session'",
        [],
    )
    .unwrap();
    conn.execute(
        "insert into chat_sessions
         (id, name, created_at, updated_at, messages_json, metadata_json)
         values (?1, ?2, ?3, ?4, ?5, ?6)",
        params!["firebender-invalid", "invalid", 9_i64, 10_i64, "{", "{}"],
    )
    .unwrap();
    drop(conn);

    for resume in [false, true] {
        let mut command = ctx(&mixed_temp);
        command.args([
            "import",
            "--provider",
            "firebender",
            "--path",
            &mixed_project,
            "--format=json",
            "--progress",
            "none",
        ]);
        if resume {
            command.arg("--resume");
        }
        let report = json_output(&mut command);
        assert_eq!(
            report["outcome"], "completed_with_rejections",
            "resume={resume}: {report:#}"
        );
        assert_eq!(report["failure_scope"], "record", "{report:#}");
        assert_eq!(report["totals"]["failed_sources"], 0, "{report:#}");
        assert_eq!(report["totals"]["rejected_records"], 1, "{report:#}");
        assert!(
            report["totals"]["imported_events"].as_u64().unwrap() > 0 || resume,
            "{report:#}"
        );
        assert_eq!(
            report["sources"][0]["status"], "completed_with_rejections",
            "{report:#}"
        );
    }

    let invalid_temp = tempdir();
    let invalid_project =
        write_native_firebender_fixture(&invalid_temp, "unused all-invalid oracle");
    let invalid_database = Path::new(&invalid_project)
        .join(".idea")
        .join("firebender")
        .join("chat_history.db");
    let conn = Connection::open(&invalid_database).unwrap();
    conn.execute("update chat_sessions set messages_json = '{'", [])
        .unwrap();
    drop(conn);

    for resume in [false, true] {
        let mut command = ctx(&invalid_temp);
        command.args([
            "import",
            "--provider",
            "firebender",
            "--path",
            &invalid_project,
            "--format=json",
            "--progress",
            "none",
        ]);
        if resume {
            command.arg("--resume");
        }
        let output = command.assert().failure().get_output().clone();
        let report: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "invalid Firebender import JSON ({error}); stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            )
        });
        assert_eq!(report["outcome"], "failure", "resume={resume}: {report:#}");
        assert_eq!(report["failure_scope"], "source", "{report:#}");
        assert_eq!(
            report["failure_type"], "record_rejection_and_source_failure",
            "{report:#}"
        );
        assert_eq!(report["totals"]["imported_sessions"], 0, "{report:#}");
        assert_eq!(report["totals"]["imported_events"], 0, "{report:#}");
        assert_eq!(report["totals"]["rejected_records"], 1, "{report:#}");
        assert_eq!(
            report["sources"][0]["failure_type"], "record_rejection",
            "{report:#}"
        );
    }

    let conn = Connection::open(invalid_temp.path().join("work.sqlite")).unwrap();
    for table in ["capture_sources", "sessions", "events"] {
        let count = conn
            .query_row(&format!("select count(*) from {table}"), [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap();
        assert_eq!(count, 0, "unexpected all-invalid rows in {table}");
    }
}

#[test]
fn codex_mixed_session_replay_remains_completed_with_rejections() {
    let temp = tempdir();
    let session = temp.path().join("codex-mixed-replay.jsonl");
    fs::write(
        &session,
        concat!(
            r#"{"timestamp":"2026-07-13T12:00:00.000Z","type":"session_meta","payload":{"id":"codex-mixed-replay","timestamp":"2026-07-13T12:00:00.000Z","cwd":"/repo","originator":"codex-cli","cli_version":"0.200.0","source":"cli","model_provider":"openai"}}"#,
            "\n",
            r#"{"timestamp":"2026-07-13T12:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"codex mixed replay oracle"}]}}"#,
            "\n",
            r#"{"timestamp":"2026-07-13T12:00:02.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":["#,
            "\n",
        ),
    )
    .unwrap();
    let path = session.to_str().unwrap();

    for resume in [false, true] {
        let mut command = ctx(&temp);
        command.args([
            "import",
            "--provider",
            "codex",
            "--path",
            path,
            "--format=json",
            "--progress",
            "none",
        ]);
        if resume {
            command.arg("--resume");
        }
        let report = json_output(&mut command);
        assert_eq!(
            report["outcome"], "completed_with_rejections",
            "resume={resume}: {report:#}"
        );
        assert_eq!(report["failure_scope"], "record", "{report:#}");
        assert_eq!(report["totals"]["failed_sources"], 0, "{report:#}");
        assert_eq!(report["totals"]["rejected_records"], 1, "{report:#}");
        assert_eq!(
            report["sources"][0]["status"], "completed_with_rejections",
            "{report:#}"
        );
    }

    let search = json_output(ctx(&temp).args([
        "search",
        "codex mixed replay oracle",
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(&search, "codex", "codex mixed replay oracle", 1, "message");
}

#[test]
fn corrected_manifested_file_retries_rejected_row_idempotently() {
    let temp = tempdir();
    let project = temp.path().join("claude-project");
    fs::create_dir_all(&project).unwrap();
    let session = project.join("manifest-retry.jsonl");
    let valid_user = r#"{"sessionId":"manifest-retry","timestamp":"2026-07-13T12:00:00Z","cwd":"/repo","version":"test","type":"user","message":{"role":"user","content":[{"type":"text","text":"manifest retry valid row"}]},"uuid":"manifest-retry-1"}"#;
    fs::write(&session, format!("{valid_user}\n{{\"type\":\n")).unwrap();
    let path = project.to_str().unwrap();

    let first = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "claude",
        "--path",
        path,
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(first["outcome"], "completed_with_rejections", "{first:#}");
    assert_eq!(first["totals"]["imported_events"], 1, "{first:#}");
    assert_eq!(first["totals"]["rejected_records"], 1, "{first:#}");
    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(status["failed_source_import_files"], 1, "{status:#}");
    assert_eq!(status["pending_source_import_files"], 1, "{status:#}");

    let valid_assistant = r#"{"sessionId":"manifest-retry","timestamp":"2026-07-13T12:00:01Z","cwd":"/repo","version":"test","type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"manifest retry corrected row"}]},"uuid":"manifest-retry-2"}"#;
    fs::write(&session, format!("{valid_user}\n{valid_assistant}\n")).unwrap();
    let corrected = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "claude",
        "--path",
        path,
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(corrected["outcome"], "success", "{corrected:#}");
    assert_eq!(corrected["totals"]["imported_events"], 1, "{corrected:#}");
    assert_eq!(corrected["totals"]["skipped_events"], 1, "{corrected:#}");
    assert_eq!(corrected["totals"]["rejected_records"], 0, "{corrected:#}");

    let search = json_output(ctx(&temp).args([
        "search",
        "manifest retry corrected row",
        "--provider",
        "claude",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(
        &search,
        "claude",
        "manifest retry corrected row",
        1,
        "message",
    );
}

#[test]
fn all_invalid_source_reports_failure_json_and_exits_nonzero() {
    let temp = tempdir();
    let brain = temp.path().join("brain");
    let bad_logs = brain.join("agy-bad").join(".system_generated").join("logs");
    fs::create_dir_all(&bad_logs).unwrap();
    fs::write(bad_logs.join("transcript_full.jsonl"), "{\"step_index\":\n").unwrap();

    let output = ctx(&temp)
        .args([
            "import",
            "--provider",
            "antigravity",
            "--path",
            brain.to_str().unwrap(),
            "--format=json",
            "--progress",
            "none",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "invalid import JSON ({error}); stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });

    assert_eq!(report["schema_version"], 2, "{report:#}");
    assert_eq!(report["outcome"], "failure", "{report:#}");
    assert_eq!(report["failure_scope"], "source", "{report:#}");
    assert_eq!(report["totals"]["imported_sources"], 0, "{report:#}");
    assert_eq!(report["totals"]["failed_sources"], 1, "{report:#}");
    assert_eq!(report["sources"][0]["status"], "failure", "{report:#}");
    assert_eq!(report["sources"][0]["rejected_records"], 1, "{report:#}");
    assert_eq!(
        report["sources"][0]["rejections"].as_array().unwrap().len(),
        1,
        "{report:#}"
    );
}

#[test]
fn complete_oversize_only_codex_session_replays_failure_without_import_scaffolding() {
    let temp = tempdir();
    let session = temp.path().join("codex-all-rejected.jsonl");
    let mut source = concat!(
        r#"{"timestamp":"2026-07-13T12:00:00Z","type":"session_meta","payload":{"id":"codex-all-rejected","timestamp":"2026-07-13T12:00:00Z","cwd":"/repo","originator":"codex-cli"}}"#,
        "\n",
        r#"{"timestamp":"2026-07-13T12:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":""#,
    )
    .to_owned();
    source.push_str(&"x".repeat(16 * 1024 * 1024));
    source.push_str("\"}]}}\n");
    fs::write(&session, source).unwrap();

    for resume in [false, true] {
        let mut command = ctx(&temp);
        command.args([
            "import",
            "--provider",
            "codex",
            "--path",
            session.to_str().unwrap(),
            "--format=json",
            "--progress",
            "none",
        ]);
        if resume {
            command.arg("--resume");
        }
        let output = command.assert().failure().get_output().clone();
        let report: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "invalid import JSON ({error}); stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            )
        });
        assert_eq!(report["outcome"], "failure", "resume={resume}: {report:#}");
        assert_eq!(report["totals"]["rejected_records"], 1, "{report:#}");
    }

    let conn = Connection::open(temp.path().join("work.sqlite")).unwrap();
    for table in ["history_records", "capture_sources", "sessions", "events"] {
        let count = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap();
        assert_eq!(count, 0, "unexpected rows in {table}");
    }
}

#[test]
fn missing_explicit_format_source_reports_failure_json() {
    let temp = tempdir();
    let missing = temp.path().join("missing-history.jsonl");
    let output = ctx(&temp)
        .args([
            "import",
            "--format",
            "ctx-history-jsonl-v1",
            "--path",
            missing.to_str().unwrap(),
            "--format=json",
            "--progress",
            "none",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();

    assert_eq!(report["schema_version"], 2, "{report:#}");
    assert_eq!(report["outcome"], "failure", "{report:#}");
    assert_eq!(report["failure_scope"], "source", "{report:#}");
    assert_eq!(report["failure_type"], "source_failure", "{report:#}");
    assert_eq!(report["totals"]["failed_sources"], 1, "{report:#}");
    assert_eq!(report["totals"]["rejected_records"], 0, "{report:#}");
    assert_eq!(report["sources"][0]["status"], "failure", "{report:#}");
    assert_eq!(
        report["sources"][0]["failure_type"], "not_found",
        "{report:#}"
    );
    assert_eq!(report["sources"][0]["imported_sessions"], 0, "{report:#}");
    assert_eq!(report["sources"][0]["rejections"], json!([]), "{report:#}");
}

fn failed_warp_report(temp: &TempDir, path: &Path) -> Value {
    let output = ctx(temp)
        .args([
            "import",
            "--provider",
            "warp",
            "--path",
            path.to_str().unwrap(),
            "--no-daemon",
            "--format=json",
            "--progress",
            "none",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "invalid Warp failure JSON ({error}); stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn warp_native_source_failures_keep_cli_classification() {
    let corrupt_temp = tempdir();
    let corrupt_path = corrupt_temp.path().join("corrupt-warp.sqlite");
    fs::write(&corrupt_path, b"not a SQLite database").unwrap();
    let corrupt = failed_warp_report(&corrupt_temp, &corrupt_path);
    assert_eq!(
        corrupt["sources"][0]["failure_type"], "source_database",
        "{corrupt:#}"
    );

    let schema_temp = tempdir();
    let schema_path = schema_temp.path().join("schema-warp.sqlite");
    drop(Connection::open(&schema_path).unwrap());
    let schema = failed_warp_report(&schema_temp, &schema_path);
    assert_eq!(
        schema["sources"][0]["failure_type"], "unsupported_schema",
        "{schema:#}"
    );
}

#[cfg(unix)]
#[test]
fn symlinked_default_source_is_not_admitted_beside_a_valid_source() {
    let temp = tempdir();
    write_codex_inventory_oracle(&temp);
    write_symlinked_claude_inventory_source(&temp);

    let report =
        json_output(ctx(&temp).args(["import", "--all", "--format=json", "--progress", "none"]));

    assert_eq!(report["outcome"], "success", "{report:#}");
    assert_eq!(report["totals"]["imported_sources"], 1, "{report:#}");
    assert_eq!(report["totals"]["failed_sources"], 0, "{report:#}");
    assert_eq!(report["totals"]["imported_events"], 1, "{report:#}");
    assert!(!report["sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|source| source["provider"] == "claude"));
}

#[cfg(unix)]
#[test]
fn symlinked_default_source_is_rejected_before_inventory() {
    let temp = tempdir();
    write_symlinked_claude_inventory_source(&temp);

    let error =
        failure_stderr(ctx(&temp).args(["import", "--all", "--format=json", "--progress", "none"]));
    assert!(
        error.contains("no importable provider history sources found"),
        "{error}"
    );

    let sources =
        json_output(ctx(&temp).args(["sources", "--provider", "claude", "--format=json"]));
    assert_eq!(sources["sources"], json!([]), "{sources:#}");
}

#[test]
fn mixed_import_analytics_reports_only_coarse_rejection_outcome() {
    let temp = tempdir();
    let brain = write_antigravity_valid_and_malformed_file_tree(&temp);
    let events_path = temp.path().join("analytics.jsonl");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let data_root = temp.path().join("data");
    fs::create_dir_all(&home).unwrap();

    let output = ctx(&temp)
        .args([
            "import",
            "--provider",
            "antigravity",
            "--path",
            brain.to_str().unwrap(),
            "--format=json",
            "--progress",
            "none",
        ])
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env("CTX_ANALYTICS_ENABLED", "1")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .assert()
        .success()
        .get_output()
        .clone();

    assert!(
        events_path.exists(),
        "analytics event was not written; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let event = read_analytics_events(&events_path).remove(0);
    let properties = analytics_event_properties(&event);
    assert_eq!(properties["import_outcome"], "completed_with_rejections");
    assert_eq!(properties["import_failure_scope"], "record");
    assert_eq!(properties["import_failure_type"], "record_rejection");
    assert_analytics_properties_are_allowlisted(properties);
    let encoded = serde_json::to_string(properties).unwrap();
    assert!(!encoded.contains(brain.to_str().unwrap()), "{encoded}");
    assert!(!encoded.contains("agy-bad"), "{encoded}");
}

fn write_antigravity_valid_and_malformed_file_tree(temp: &TempDir) -> PathBuf {
    let source_fixture = PathBuf::from(provider_history_fixture("antigravity/v1/brain"));
    let brain = temp.path().join("brain");
    let valid_logs = brain
        .join("agy-success")
        .join(".system_generated")
        .join("logs");
    fs::create_dir_all(&valid_logs).unwrap();
    fs::copy(
        source_fixture
            .join("agy-success")
            .join(".system_generated")
            .join("logs")
            .join("transcript_full.jsonl"),
        valid_logs.join("transcript_full.jsonl"),
    )
    .unwrap();

    let bad_logs = brain.join("agy-bad").join(".system_generated").join("logs");
    fs::create_dir_all(&bad_logs).unwrap();
    fs::write(bad_logs.join("transcript_full.jsonl"), "{\"step_index\":\n").unwrap();
    brain
}

#[cfg(unix)]
fn write_codex_inventory_oracle(temp: &TempDir) {
    let sessions = temp
        .path()
        .join(".codex")
        .join("sessions")
        .join("2026/07/13");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("rollout-inventory-oracle.jsonl"),
        concat!(
            r#"{"timestamp":"2026-07-13T12:00:00.000Z","type":"session_meta","payload":{"id":"inventory-oracle","timestamp":"2026-07-13T12:00:00.000Z","cwd":"/repo","originator":"codex-cli","cli_version":"0.200.0","source":"cli","model_provider":"openai"}}"#,
            "\n",
            r#"{"timestamp":"2026-07-13T12:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"inventory isolation oracle"}]}}"#,
            "\n",
        ),
    )
    .unwrap();
}

#[cfg(unix)]
fn write_symlinked_claude_inventory_source(temp: &TempDir) {
    let target = temp.path().join("claude-projects-target");
    fs::create_dir_all(&target).unwrap();
    fs::write(
        target.join("symlinked-session.jsonl"),
        r#"{"sessionId":"symlinked","type":"user","message":{"role":"user","content":"inventory failure"}}"#,
    )
    .unwrap();
    let claude = temp.path().join(".claude");
    fs::create_dir_all(&claude).unwrap();
    std::os::unix::fs::symlink(target, claude.join("projects")).unwrap();
}
