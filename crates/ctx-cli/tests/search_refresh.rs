mod support;

use support::*;

#[test]
fn search_refreshes_discovered_codex_sessions_before_query() {
    let temp = tempdir();
    let fixture = PathBuf::from(provider_history_fixture("codex-sessions"));
    let discovered = temp.path().join(".codex").join("sessions");
    copy_dir_all(&fixture, &discovered);

    let search = json_output(ctx(&temp).args([
        "search",
        "onboarding",
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_search_provider_oracle(&search, "codex", "onboarding", 1, "message");
    assert_eq!(search["freshness"]["mode"], "wait");
    assert_eq!(search["freshness"]["status"], "completed");
    assert_eq!(search["freshness"]["source_count"], 1);
    assert_eq!(search["freshness"]["totals"]["imported_sessions"], 2);

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(status["cataloged_sessions"], 2);
    assert_eq!(status["indexed_catalog_sessions"], 2);
    assert_eq!(status["pending_catalog_sessions"], 0);
}

#[test]
fn search_refreshes_discovered_codex_prompt_history_before_query() {
    let temp = tempdir();
    let history = temp.path().join(".codex/history.jsonl");
    fs::create_dir_all(history.parent().unwrap()).unwrap();
    fs::write(
        &history,
        concat!(
            r#"{"session_id":"prompt-refresh-session","ts":1784371200,"text":"prompt history search refresh oracle"}"#,
            "\n"
        ),
    )
    .unwrap();

    let search = json_output(ctx(&temp).args([
        "search",
        "prompt history search refresh oracle",
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_search_provider_oracle(
        &search,
        "codex",
        "prompt history search refresh oracle",
        1,
        "message",
    );
    assert_eq!(search["freshness"]["status"], "completed");
    assert_eq!(search["freshness"]["source_count"], 1);
    assert_eq!(search["freshness"]["totals"]["imported_sessions"], 1);
    assert_eq!(search["freshness"]["totals"]["imported_events"], 1);
}

#[test]
fn machine_readable_default_search_does_not_autostart_daemon() {
    let temp = tempdir();
    let fixture = PathBuf::from(provider_history_fixture("codex-sessions"));
    let discovered = temp.path().join(".codex").join("sessions");
    copy_dir_all(&fixture, &discovered);
    let missing_exe = temp.path().join("missing-ctx-binary");

    let search = json_output(
        ctx(&temp)
            .args([
                "search",
                "onboarding",
                "--provider",
                "codex",
                "--format=json",
            ])
            .env("CTX_DAEMON_AUTOSTART_EXE", &missing_exe)
            .env_remove("CTX_DAEMON_AUTOSTART_OFF"),
    );
    assert_search_provider_oracle(&search, "codex", "onboarding", 1, "message");
    assert_eq!(search["freshness"]["status"], "completed");
    assert_eq!(search["retrieval"]["requested_mode"], "lexical");
    assert!(!temp.path().join("daemon/status.json").exists());

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert!(status["daemon"]["trigger_command"].is_null());
    assert_eq!(status["semantic"]["status"], "disabled");
    assert_eq!(status["semantic"]["reason"], "semantic_disabled");
}

#[test]
fn search_refresh_wait_skips_malformed_jsonl_rows() {
    let temp = tempdir();
    write_malformed_claude_session(&temp);

    let output = ctx(&temp)
        .args([
            "search",
            "rejected refresh search marker",
            "--provider",
            "claude",
            "--refresh",
            "wait",
            "--format=json",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let search: Value = serde_json::from_slice(&output.stdout).unwrap();

    assert_search_provider_oracle(
        &search,
        "claude",
        "rejected refresh search marker",
        1,
        "message",
    );
    assert_eq!(search["freshness"]["status"], "completed");
    assert!(
        search["freshness"]["totals"]["rejected_records"]
            .as_u64()
            .unwrap()
            >= 1,
        "{search:#}"
    );

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("refreshed claude with"), "{stderr}");
    assert!(stderr.contains("first failure at line 2"), "{stderr}");
    assert!(stderr.contains("malformed Claude JSONL record"), "{stderr}");
    assert!(
        stderr.contains("malformed or structurally unbounded Claude JSON record"),
        "{stderr}"
    );
}

#[test]
fn search_refresh_wait_warns_when_progress_is_not_interactive() {
    let temp = tempdir();
    write_malformed_claude_session(&temp);

    let output = ctx(&temp)
        .args([
            "search",
            "rejected refresh search marker",
            "--provider",
            "claude",
            "--refresh",
            "wait",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("rejected refresh search marker"),
        "{stdout}"
    );

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("refreshed claude with"), "{stderr}");
    assert!(stderr.contains("first failure at line 2"), "{stderr}");
    assert!(stderr.contains("malformed Claude JSONL record"), "{stderr}");
    assert!(
        stderr.contains("malformed or structurally unbounded Claude JSON record"),
        "{stderr}"
    );
}

fn write_malformed_claude_session(temp: &TempDir) {
    let project = temp.path().join(".claude").join("projects").join("-repo");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join("claude-session.jsonl"),
        concat!(
            r#"{"sessionId":"claude-session","timestamp":"2026-06-24T10:00:00Z","cwd":"/repo","version":"test","type":"user","message":{"role":"user","content":[{"type":"text","text":"rejected refresh search marker"}]},"uuid":"claude-user"}"#,
            "\n",
            "{malformed-jsonl-row\n",
            r#"{"sessionId":"claude-session","timestamp":"2026-06-24T10:00:01Z","cwd":"/repo","version":"test","type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"valid rows remain searchable"}]},"uuid":"claude-assistant"}"#,
            "\n"
        ),
    )
    .unwrap();
}

#[test]
fn search_refresh_off_serves_existing_index_without_importing() {
    let temp = tempdir();
    let indexed_fixture = provider_history_fixture("codex-sessions");
    json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &indexed_fixture,
        "--format=json",
    ]));
    let discovered_fixture = provider_history_fixture("codex-rich-sessions");
    let discovered = temp.path().join(".codex").join("sessions");
    copy_dir_all(&PathBuf::from(discovered_fixture), &discovered);

    let stale = json_output(ctx(&temp).args([
        "search",
        "diagnostic sample app",
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(stale["freshness"]["mode"], "off");
    assert_eq!(stale["freshness"]["status"], "skipped");
    assert!(stale["results"].as_array().unwrap().is_empty());

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(status["cataloged_sessions"], 2);
    assert_eq!(status["indexed_catalog_sessions"], 2);

    let fresh = json_output(ctx(&temp).args([
        "search",
        "onboarding",
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_search_provider_oracle(&fresh, "codex", "onboarding", 1, "message");
}

#[test]
fn search_refresh_wait_drains_native_failure_and_imports_later_good_source() {
    let temp = tempdir();
    let sessions = temp
        .path()
        .join(".codex")
        .join("sessions")
        .join("2026/07/12");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(sessions.join("rollout-empty-session.jsonl"), "").unwrap();
    let query = "pi-later-good-refresh-oracle";
    install_default_pi_fixture(&temp, query);

    let stderr =
        failure_stderr(ctx(&temp).args(["search", query, "--refresh", "wait", "--format=json"]));

    assert!(
        stderr.contains("1 search refresh source failure(s)"),
        "{stderr}"
    );
    assert!(stderr.contains("import codex source"), "{stderr}");
    assert!(
        stderr.contains("Codex NativePath source has no valid session owner"),
        "{stderr}"
    );

    let indexed = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "pi",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(&indexed, "pi", query, 1, "message");
}

#[test]
fn search_refresh_auto_all_malformed_native_history_fails_instead_of_serving_empty_index() {
    let temp = tempdir();
    ctx(&temp).args(["daemon", "disable"]).assert().success();
    let sessions = temp
        .path()
        .join(".codex")
        .join("sessions")
        .join("2026/07/12");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(sessions.join("rollout-empty-session.jsonl"), "").unwrap();

    let stderr = failure_stderr(ctx(&temp).args([
        "search",
        "anything",
        "--provider",
        "codex",
        "--refresh",
        "background",
        "--format=json",
    ]));

    assert!(
        stderr.contains("search refresh failed and no existing ctx index is available"),
        "{stderr}"
    );
    assert!(
        stderr.contains("all search refresh sources failed; first failure: import codex source"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Codex NativePath source has no valid session owner"),
        "{stderr}"
    );
}

#[test]
fn search_refresh_auto_imports_fresh_work_despite_large_existing_catalog() {
    let temp = tempdir();
    let fixture = PathBuf::from(provider_history_fixture("codex-sessions"));
    let _ = json_output(ctx(&temp).args(["setup", "--format=json"]));
    let discovered = temp.path().join(".codex").join("sessions");
    copy_dir_all(&fixture, &discovered);

    let mut conn = Connection::open(temp.path().join("work.sqlite")).unwrap();
    let tx = conn.transaction().unwrap();
    {
        let mut stmt = tx
            .prepare(
                "INSERT INTO catalog_sessions (
                    source_path, provider, source_format, source_root,
                    external_session_id, agent_type, file_size_bytes,
                    file_modified_at_ms, cataloged_at_ms, indexed_status,
                    indexed_at_ms, indexed_file_size_bytes,
                    indexed_file_modified_at_ms, metadata_json
                ) VALUES (?1, 'codex', 'codex_session_jsonl_tree', ?2, ?3,
                    'primary', 2, 1782259200000, 1782259200000, 'indexed',
                    1782259200000, 2, 1782259200000, '{}')",
            )
            .unwrap();
        for index in 0..10_000 {
            stmt.execute(params![
                format!("{}/seed-{index:05}.jsonl", discovered.display()),
                discovered.display().to_string(),
                format!("large-catalog-session-{index:05}"),
            ])
            .unwrap();
        }
    }
    tx.commit().unwrap();
    drop(conn);
    let search = json_output(ctx(&temp).args([
        "search",
        "onboarding",
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_eq!(search["freshness"]["mode"], "wait");
    assert_eq!(search["freshness"]["status"], "completed");
    assert_eq!(search["freshness"]["source_count"], 1);
    assert_eq!(search["freshness"]["totals"]["imported_sessions"], 2);
    assert_search_provider_oracle(&search, "codex", "onboarding", 1, "message");

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(status["pending_catalog_sessions"], 0);
}

#[test]
fn search_refresh_auto_tail_imports_appended_codex_session_event() {
    let temp = tempdir();
    let fixture = PathBuf::from(provider_history_fixture("codex-sessions"));
    let discovered = temp.path().join(".codex").join("sessions");
    copy_dir_all(&fixture, &discovered);
    let root_session = discovered.join("2026/06/23/root.jsonl");
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&root_session)
        .unwrap();
    for index in 0..250 {
        writeln!(
            file,
            "{}",
            json!({
                "timestamp": "2026-06-23T15:00:00.000Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": format!("tail-refresh-baseline-{index}")}]
                }
            })
        )
        .unwrap();
    }
    drop(file);

    let first = json_output(ctx(&temp).args([
        "search",
        "onboarding",
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_search_provider_oracle(&first, "codex", "onboarding", 1, "message");

    let appended_needle = "tail-refresh-append-oracle";
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&root_session)
        .unwrap();
    writeln!(
        file,
        "{}",
        json!({
            "timestamp": "2026-06-23T15:00:30.000Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": appended_needle}]
            }
        })
    )
    .unwrap();

    let started = Instant::now();
    let refreshed = json_output(ctx(&temp).args([
        "search",
        appended_needle,
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "tail refresh took {elapsed:?}"
    );
    assert_eq!(refreshed["freshness"]["status"], "completed");
    assert_eq!(refreshed["freshness"]["totals"]["imported_events"], 1);
    assert!(
        refreshed["freshness"]["totals"]["skipped"]
            .as_u64()
            .unwrap()
            < 20,
        "tail refresh unexpectedly reprocessed old events: {}",
        refreshed["freshness"]["totals"]
    );
    assert_search_provider_oracle(&refreshed, "codex", appended_needle, 1, "message");

    let second_append_needle = "tail-refresh-second-append-oracle";
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&root_session)
        .unwrap();
    writeln!(
        file,
        "{}",
        json!({
            "timestamp": "2026-06-23T15:00:31.000Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": second_append_needle}]
            }
        })
    )
    .unwrap();

    let second_refreshed = json_output(ctx(&temp).args([
        "search",
        second_append_needle,
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_eq!(second_refreshed["freshness"]["status"], "completed");
    assert_eq!(
        second_refreshed["freshness"]["totals"]["imported_events"],
        1
    );
    assert!(
        second_refreshed["freshness"]["totals"]["skipped"]
            .as_u64()
            .unwrap()
            < 20,
        "second tail refresh unexpectedly reprocessed old events: {}",
        second_refreshed["freshness"]["totals"]
    );
    assert_search_provider_oracle(
        &second_refreshed,
        "codex",
        second_append_needle,
        1,
        "message",
    );
}

#[test]
fn search_refresh_auto_imports_discovered_top_provider_sources() {
    for (cli_provider, stored_provider, install_fixture) in [
        (
            "claude",
            "claude",
            install_default_claude_fixture as fn(&TempDir, &str),
        ),
        ("pi", "pi", install_default_pi_fixture),
        ("cursor", "cursor", install_default_cursor_fixture),
        ("hermes", "hermes", install_default_hermes_fixture),
        ("kilo", "kilo", install_default_kilo_fixture),
        ("astrbot", "astrbot", install_default_astrbot_fixture),
        ("continue", "continue", install_default_continue_fixture),
        ("openhands", "openhands", install_default_openhands_fixture),
        ("rovodev", "rovodev", install_default_rovodev_fixture),
        ("lingma", "lingma", install_default_lingma_fixture),
        ("qoder", "qoder", install_default_qoder_fixture),
        ("junie", "junie", install_default_junie_fixture),
    ] {
        let temp = tempdir();
        let query = format!("{stored_provider}-default-refresh-oracle");
        install_fixture(&temp, &query);

        let search = json_output(ctx(&temp).args([
            "search",
            &query,
            "--provider",
            cli_provider,
            "--refresh",
            "wait",
            "--format=json",
        ]));
        assert_eq!(search["freshness"]["mode"], "wait");
        assert_eq!(search["freshness"]["status"], "completed");
        assert_eq!(search["freshness"]["source_count"], 1);
        assert!(
            search["freshness"]["totals"]["imported_sessions"]
                .as_u64()
                .unwrap()
                >= 1
        );
        assert_search_provider_oracle(&search, stored_provider, &query, 1, "message");

        let status = json_output(ctx(&temp).args(["status", "--format=json"]));
        assert!(
            status["inventory_units"].as_u64().unwrap() >= 1,
            "{cli_provider} did not record search-refresh inventory: {status:#}"
        );
        assert_eq!(
            status["pending_inventory_units"], 0,
            "{cli_provider} left inventory pending after search refresh: {status:#}"
        );

        let started = Instant::now();
        let refreshed = json_output(ctx(&temp).args([
            "search",
            &query,
            "--provider",
            cli_provider,
            "--refresh",
            "wait",
            "--format=json",
        ]));
        assert_eq!(refreshed["freshness"]["mode"], "wait");
        assert_eq!(refreshed["freshness"]["status"], "completed");
        assert_eq!(refreshed["freshness"]["totals"]["imported_events"], 0);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "second refresh should stay incremental for {cli_provider}"
        );
    }
}

#[test]
fn search_refresh_hermes_root_inventory_skips_unchanged_and_detects_wal_only_append() {
    let temp = tempdir();
    let initial = "hermes-root-inventory-initial-oracle";
    let appended = "hermes-root-inventory-appended-oracle";
    install_default_hermes_fixture(&temp, initial);
    let source = temp.path().join(".hermes/state.db");
    let writer = Connection::open(&source).unwrap();
    let journal_mode: String = writer
        .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(journal_mode, "wal");
    writer
        .execute_batch("PRAGMA wal_autocheckpoint = 0")
        .unwrap();

    let first = json_output(ctx(&temp).args([
        "search",
        initial,
        "--provider",
        "hermes",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_search_provider_oracle(&first, "hermes", initial, 1, "message");

    let unchanged = json_output(ctx(&temp).args([
        "search",
        initial,
        "--provider",
        "hermes",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_eq!(unchanged["freshness"]["totals"]["imported_events"], 0);

    let main_before = fs::metadata(&source).unwrap();
    writer
        .execute(
            "INSERT INTO messages (session_id, role, content, timestamp)
             VALUES (?1, 'user', ?2, 1782259203.0)",
            ["hermes-cli-native", appended],
        )
        .unwrap();
    assert!(source.with_extension("db-wal").is_file());
    let main_after = fs::metadata(&source).unwrap();
    assert_eq!(main_after.len(), main_before.len());
    assert_eq!(
        main_after.modified().unwrap(),
        main_before.modified().unwrap()
    );

    let refreshed = json_output(ctx(&temp).args([
        "search",
        appended,
        "--provider",
        "hermes",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_search_provider_oracle(&refreshed, "hermes", appended, 1, "message");
    assert!(
        refreshed["freshness"]["totals"]["imported_events"]
            .as_u64()
            .unwrap()
            >= 1
    );
    drop(writer);
}

#[test]
fn search_refresh_wait_json_emits_progress_on_stderr() {
    let temp = tempdir();
    let fixture = PathBuf::from(provider_history_fixture("codex-sessions"));
    copy_dir_all(&fixture, &temp.path().join(".codex").join("sessions"));

    let output = ctx(&temp)
        .args([
            "search",
            "onboarding",
            "--provider",
            "codex",
            "--refresh",
            "wait",
            "--format=json",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["freshness"]["status"], "completed");
    assert_search_provider_oracle(&stdout, "codex", "onboarding", 1, "message");

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains(r#""type":"ctx_progress""#), "{stderr}");
    assert!(
        stderr.contains(r#""operation":"search-refresh""#),
        "{stderr}"
    );
}

#[test]
fn search_refresh_wait_reports_no_sources_for_complete_empty_inventory() {
    let temp = tempdir();
    let search =
        json_output(ctx(&temp).args(["search", "anything", "--refresh", "wait", "--format=json"]));

    assert_eq!(search["freshness"]["status"], "no_sources");
    assert_eq!(search["freshness"]["source_count"], 0);
    assert_eq!(search["freshness"]["totals"]["failed_sources"], 0);
    assert!(search["results"].as_array().unwrap().is_empty());
}
