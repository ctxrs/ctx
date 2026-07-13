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
        "--json",
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

    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(status["source_import_files"], 2, "{status:#}");
    assert_eq!(status["indexed_source_import_files"], 1, "{status:#}");
    assert_eq!(status["failed_source_import_files"], 0, "{status:#}");
    assert_eq!(status["pending_source_import_files"], 0, "{status:#}");
    assert_eq!(
        status["terminal_rejected_source_import_files"], 1,
        "{status:#}"
    );

    let search = json_output(ctx(&temp).args([
        "search",
        "write_to_file",
        "--provider",
        "antigravity",
        "--json",
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
        "--json",
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
        "--json",
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
            "--json",
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
        assert_eq!(report["totals"]["failed_sources"], 0, "{report:#}");
        assert_eq!(report["totals"]["rejected_records"], 1, "{report:#}");
    }

    let search = json_output(ctx(&temp).args([
        "search",
        "codex mixed replay oracle",
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--json",
    ]));
    assert_search_provider_oracle(&search, "codex", "codex mixed replay oracle", 1, "message");
}

#[test]
fn unchanged_codex_catalog_mixed_observation_is_not_selected_again() {
    let temp = tempdir();
    let sessions = temp.path().join("codex-sessions");
    fs::create_dir_all(&sessions).unwrap();
    let session = sessions.join("mixed.jsonl");
    let meta = r#"{"timestamp":"2026-07-13T12:00:00.000Z","type":"session_meta","payload":{"id":"catalog-mixed","timestamp":"2026-07-13T12:00:00.000Z","cwd":"/repo","originator":"codex-cli","source":"cli"}}"#;
    let user = r#"{"timestamp":"2026-07-13T12:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"catalog mixed searchable oracle"}]}}"#;
    let rejected = r#"{"timestamp":"2026-07-13T12:00:02.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":["#;
    fs::write(&session, format!("{meta}\n{user}\n{rejected}\n")).unwrap();

    let first = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        sessions.to_str().unwrap(),
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(first["outcome"], "completed_with_rejections", "{first:#}");
    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(status["pending_catalog_sessions"], 0, "{status:#}");
    assert_eq!(status["failed_catalog_sessions"], 0, "{status:#}");
    assert_eq!(
        status["completed_with_rejections_catalog_sessions"], 1,
        "{status:#}"
    );

    let unchanged = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        sessions.to_str().unwrap(),
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(unchanged["outcome"], "success", "{unchanged:#}");
    assert_eq!(unchanged["totals"]["rejected_records"], 0, "{unchanged:#}");
    assert_eq!(unchanged["totals"]["imported_events"], 0, "{unchanged:#}");

    let assistant = r#"{"timestamp":"2026-07-13T12:00:02.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"catalog mixed corrected oracle"}]}}"#;
    fs::write(&session, format!("{meta}\n{user}\n{assistant}\n")).unwrap();
    let corrected = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        sessions.to_str().unwrap(),
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(corrected["outcome"], "success", "{corrected:#}");
    let search = json_output(ctx(&temp).args([
        "search",
        "catalog mixed corrected oracle",
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--json",
    ]));
    assert_search_provider_oracle(
        &search,
        "codex",
        "catalog mixed corrected oracle",
        1,
        "message",
    );
}

#[test]
fn legacy_pr75_generic_catalog_failure_gets_one_tolerant_recovery_attempt() {
    let temp = tempdir();
    let sessions = temp.path().join("codex-sessions");
    fs::create_dir_all(&sessions).unwrap();
    let session = sessions.join("legacy-failed.jsonl");
    fs::write(
        &session,
        concat!(
            r#"{"timestamp":"2026-07-13T12:00:00.000Z","type":"session_meta","payload":{"id":"legacy-pr75","timestamp":"2026-07-13T12:00:00.000Z","cwd":"/repo","originator":"codex-cli","source":"cli"}}"#,
            "\n",
            r#"{"timestamp":"2026-07-13T12:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"legacy pr75 recovery oracle"}]}}"#,
            "\n",
        ),
    )
    .unwrap();
    let args = [
        "import",
        "--provider",
        "codex",
        "--path",
        sessions.to_str().unwrap(),
        "--json",
        "--progress",
        "none",
    ];
    json_output(ctx(&temp).args(args));

    let conn = Connection::open(temp.path().join("work.sqlite")).unwrap();
    conn.execute(
        "UPDATE catalog_sessions SET indexed_status = 'failed', indexed_error = 'full import failed for one or more sessions', indexed_import_revision = NULL WHERE source_path = ?1",
        [session.to_str().unwrap()],
    )
    .unwrap();
    drop(conn);

    let recovered = json_output(ctx(&temp).args(args));
    assert_eq!(recovered["outcome"], "success", "{recovered:#}");
    assert_eq!(recovered["totals"]["rejected_records"], 0, "{recovered:#}");
    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(status["failed_catalog_sessions"], 0, "{status:#}");
    assert_eq!(status["pending_catalog_sessions"], 0, "{status:#}");
    let search = json_output(ctx(&temp).args([
        "search",
        "legacy pr75 recovery oracle",
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--json",
    ]));
    assert_search_provider_oracle(
        &search,
        "codex",
        "legacy pr75 recovery oracle",
        1,
        "message",
    );
}

#[test]
fn terminal_codex_catalog_rejection_converges_then_retries_after_correction() {
    let temp = tempdir();
    let sessions = temp.path().join("codex-sessions");
    fs::create_dir_all(&sessions).unwrap();
    let session = sessions.join("rejected.jsonl");
    let meta = r#"{"timestamp":"2026-07-13T12:00:00.000Z","type":"session_meta","payload":{"id":"catalog-rejected","timestamp":"2026-07-13T12:00:00.000Z","cwd":"/repo","originator":"codex-cli","source":"cli"}}"#;
    let invalid = r#"{"timestamp":"not-rfc3339","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"catalog rejected corrected later"}]}}"#;
    fs::write(&session, format!("{meta}\n{invalid}\n")).unwrap();

    ctx(&temp)
        .args([
            "import",
            "--provider",
            "codex",
            "--path",
            sessions.to_str().unwrap(),
            "--json",
            "--progress",
            "none",
        ])
        .assert()
        .failure();
    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(
        status["terminal_rejected_catalog_sessions"], 1,
        "{status:#}"
    );
    assert_eq!(status["pending_catalog_sessions"], 0, "{status:#}");

    let unchanged = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        sessions.to_str().unwrap(),
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(unchanged["outcome"], "success", "{unchanged:#}");
    assert_eq!(unchanged["totals"]["rejected_records"], 0, "{unchanged:#}");

    let corrected = invalid.replace("not-rfc3339", "2026-07-13T12:00:01.000Z");
    fs::write(&session, format!("{meta}\n{corrected}\n")).unwrap();
    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        sessions.to_str().unwrap(),
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(imported["outcome"], "success", "{imported:#}");
    let search = json_output(ctx(&temp).args([
        "search",
        "catalog rejected corrected later",
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--json",
    ]));
    assert_search_provider_oracle(
        &search,
        "codex",
        "catalog rejected corrected later",
        1,
        "message",
    );
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
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(first["outcome"], "completed_with_rejections", "{first:#}");
    assert_eq!(first["totals"]["imported_events"], 1, "{first:#}");
    assert_eq!(first["totals"]["rejected_records"], 1, "{first:#}");
    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(status["failed_source_import_files"], 0, "{status:#}");
    assert_eq!(status["pending_source_import_files"], 0, "{status:#}");
    assert_eq!(
        status["completed_with_rejections_source_import_files"], 1,
        "{status:#}"
    );

    let valid_assistant = r#"{"sessionId":"manifest-retry","timestamp":"2026-07-13T12:00:01Z","cwd":"/repo","version":"test","type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"manifest retry corrected row"}]},"uuid":"manifest-retry-2"}"#;
    fs::write(&session, format!("{valid_user}\n{valid_assistant}\n")).unwrap();
    let corrected = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "claude",
        "--path",
        path,
        "--json",
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
        "--json",
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
fn unchanged_root_source_completed_with_rejections_is_not_retried() {
    let temp = tempdir();
    let root = PathBuf::from(write_native_openclaw_fixture(
        &temp,
        "openclaw root convergence oracle",
    ));
    let transcript = root.join("agents/personal-agent/sessions/openclaw-cli-native.jsonl");
    let mut source = fs::read_to_string(&transcript).unwrap();
    source.push_str("{\"type\":\n");
    fs::write(&transcript, source).unwrap();

    let first = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "openclaw",
        "--path",
        root.to_str().unwrap(),
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(first["outcome"], "completed_with_rejections", "{first:#}");
    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(status["pending_source_import_files"], 0, "{status:#}");
    assert_eq!(
        status["completed_with_rejections_source_import_files"], 1,
        "{status:#}"
    );

    let unchanged = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "openclaw",
        "--path",
        root.to_str().unwrap(),
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(unchanged["outcome"], "success", "{unchanged:#}");
    assert_eq!(unchanged["totals"]["rejected_records"], 0, "{unchanged:#}");
    let search = json_output(ctx(&temp).args([
        "search",
        "openclaw root convergence oracle",
        "--provider",
        "openclaw",
        "--refresh",
        "off",
        "--json",
    ]));
    assert_search_provider_oracle(
        &search,
        "openclaw",
        "openclaw root convergence oracle",
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
            "--json",
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

    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(
        status["terminal_rejected_source_import_files"], 1,
        "{status:#}"
    );
    assert_eq!(status["pending_source_import_files"], 0, "{status:#}");

    let unchanged = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "antigravity",
        "--path",
        brain.to_str().unwrap(),
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(unchanged["outcome"], "success", "{unchanged:#}");
    assert_eq!(unchanged["totals"]["rejected_records"], 0, "{unchanged:#}");

    let valid = PathBuf::from(provider_history_fixture("antigravity/v1/brain"))
        .join("agy-success/.system_generated/logs/transcript_full.jsonl");
    fs::copy(valid, bad_logs.join("transcript_full.jsonl")).unwrap();
    let corrected = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "antigravity",
        "--path",
        brain.to_str().unwrap(),
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(corrected["outcome"], "success", "{corrected:#}");
    let search = json_output(ctx(&temp).args([
        "search",
        "write_to_file",
        "--provider",
        "antigravity",
        "--refresh",
        "off",
        "--json",
    ]));
    assert_search_provider_oracle(&search, "antigravity", "write_to_file", 1, "tool_call");
}

#[test]
fn all_rejected_codex_session_leaves_no_import_scaffolding() {
    let temp = tempdir();
    let session = temp.path().join("codex-all-rejected.jsonl");
    fs::write(
        &session,
        concat!(
            r#"{"timestamp":"2026-07-13T12:00:00Z","type":"session_meta","payload":{"id":"codex-all-rejected","timestamp":"2026-07-13T12:00:00Z","cwd":"/repo","originator":"codex-cli"}}"#,
            "\n",
            r#"{"timestamp":"not-rfc3339","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"must not persist"}]}}"#,
            "\n",
        ),
    )
    .unwrap();

    let output = ctx(&temp)
        .args([
            "import",
            "--provider",
            "codex",
            "--path",
            session.to_str().unwrap(),
            "--json",
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
    assert_eq!(report["outcome"], "failure", "{report:#}");
    assert_eq!(report["totals"]["rejected_records"], 1, "{report:#}");

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
            "--json",
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

#[cfg(unix)]
#[test]
fn inventory_failure_isolated_from_independent_valid_source() {
    let temp = tempdir();
    write_codex_inventory_oracle(&temp);
    write_symlinked_claude_inventory_source(&temp);

    let report = json_output(ctx(&temp).args(["import", "--all", "--json", "--progress", "none"]));

    assert_eq!(
        report["outcome"], "completed_with_source_failures",
        "{report:#}"
    );
    assert_eq!(report["totals"]["imported_sources"], 1, "{report:#}");
    assert_eq!(report["totals"]["failed_sources"], 1, "{report:#}");
    assert_eq!(report["totals"]["imported_events"], 1, "{report:#}");
    assert!(report["sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|source| source["status"] == "failure"
            && source["error"]
                .as_str()
                .is_some_and(|error| error.contains("symlinked provider transcript roots"))));
}

#[cfg(unix)]
#[test]
fn all_inventory_failures_report_json_and_exit_nonzero() {
    let temp = tempdir();
    write_symlinked_claude_inventory_source(&temp);

    let output = ctx(&temp)
        .args(["import", "--all", "--json", "--progress", "none"])
        .assert()
        .failure()
        .get_output()
        .clone();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();

    assert_eq!(report["outcome"], "failure", "{report:#}");
    assert_eq!(report["failure_scope"], "source", "{report:#}");
    assert_eq!(report["totals"]["imported_sources"], 0, "{report:#}");
    assert_eq!(report["totals"]["failed_sources"], 1, "{report:#}");
    assert_eq!(report["sources"][0]["rejected_records"], 0, "{report:#}");
    assert_eq!(
        report["sources"][0]["rejections"].as_array().unwrap().len(),
        0,
        "{report:#}"
    );
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
            "--json",
            "--progress",
            "none",
        ])
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_OFF")
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
