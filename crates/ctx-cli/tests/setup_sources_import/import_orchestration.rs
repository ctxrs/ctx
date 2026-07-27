use super::{
    assert_daemon_process_running, assert_no_daemon_autostart_mutation, support::*,
    wait_for_daemon_status, write_active_daemon_upgrade_handoff, write_codex_setup_session,
};

#[test]
fn import_accepts_deprecated_partial_as_a_compatibility_noop() {
    let temp = tempdir();
    write_codex_setup_session(&temp);
    let source_root = temp.path().join(".codex").join("sessions");

    ctx(&temp)
        .args([
            "import",
            "--partial",
            "--quiet",
            "--provider",
            "codex",
            "--path",
            source_root.to_str().unwrap(),
            "--no-daemon",
            "--progress",
            "none",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "warning: --partial is deprecated and no longer changes import behavior; tolerant import is now unconditional",
        ));
}

#[test]
fn import_progress_json_goes_to_stderr_without_polluting_stdout() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");
    let output = ctx(&temp)
        .args([
            "import",
            "--provider",
            "codex",
            "--path",
            &fixture,
            "--json",
            "--progress",
            "json",
        ])
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["schema_version"], 2);
    assert!(stdout["totals"]["imported_sessions"].as_u64().unwrap() > 0);

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains(r#""type":"ctx_progress""#), "{stderr}");
    assert!(stderr.contains(r#""operation":"import""#), "{stderr}");
}

#[test]
fn machine_readable_native_import_preserves_json_without_autostarting_daemon() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");
    let missing_exe = temp.path().join("missing-ctx-binary");
    write_active_daemon_upgrade_handoff(&temp);

    let import = json_output(
        ctx(&temp)
            .args([
                "import",
                "--provider",
                "codex",
                "--path",
                &fixture,
                "--json",
                "--progress",
                "none",
            ])
            .env("CTX_DAEMON_AUTOSTART_EXE", &missing_exe)
            .env_remove("CI")
            .env_remove("CTX_DAEMON_AUTOSTART_OFF"),
    );
    assert_eq!(import["schema_version"], 2);
    assert!(import["totals"]["imported_sessions"].as_u64().unwrap() > 0);
    assert_no_daemon_autostart_mutation(&temp);
}

#[test]
fn progress_json_native_import_does_not_autostart_or_nudge_daemon() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");
    write_active_daemon_upgrade_handoff(&temp);

    let output = ctx(&temp)
        .args([
            "import",
            "--provider",
            "codex",
            "--path",
            &fixture,
            "--progress",
            "json",
        ])
        .env_remove("CI")
        .env_remove("CTX_DAEMON_AUTOSTART_OFF")
        .assert()
        .success()
        .get_output()
        .clone();

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains(r#""type":"ctx_progress""#), "{stderr}");
    assert_no_daemon_autostart_mutation(&temp);
}

#[test]
fn human_native_import_starts_a_reported_daemon_process() {
    let temp = tempdir();
    let binary = copied_ctx_binary(&temp);
    let fixture = provider_history_fixture("codex-sessions");

    ctx_from_binary(&temp, &binary)
        .args([
            "import",
            "--provider",
            "codex",
            "--path",
            &fixture,
            "--progress",
            "none",
        ])
        .env("CTX_DAEMON_AUTOSTART_IDLE_EXIT_SECONDS", "2")
        .env("CTX_DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS", "1")
        .env("CTX_UPGRADE_AUTO", "off")
        .env_remove("CI")
        .env_remove("CTX_DAEMON_AUTOSTART_OFF")
        .assert()
        .success();

    let running = wait_for_daemon_status(&temp, "running", true, "import");
    assert_eq!(running["daemon"]["start_mode"], "auto");
    let pid = running["daemon"]["pid"].as_u64().unwrap() as u32;
    assert_daemon_process_running(pid);

    let completed = wait_for_daemon_status(&temp, "completed", false, "import");
    assert_eq!(completed["daemon"]["pid"], pid);
    assert!(completed["daemon"]["finished_at_ms"].as_i64().unwrap() > 0);
}

#[test]
fn import_custom_history_jsonl_format_is_searchable_and_idempotent() {
    let temp = tempdir();
    let fixture = temp.path().join("basic.jsonl");
    fs::write(
        &fixture,
        fs::read(custom_history_fixture("basic.jsonl")).unwrap(),
    )
    .unwrap();
    let fixture = fixture.to_str().unwrap().to_owned();

    let first = json_output(ctx(&temp).args([
        "import",
        "--format",
        "ctx-history-jsonl-v1",
        "--path",
        &fixture,
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(first["totals"]["imported_sessions"], 2);
    assert_eq!(first["totals"]["imported_events"], 2);
    assert_eq!(first["totals"]["imported_edges"], 2);
    assert_eq!(first["sources"][0]["provider"], "custom");
    assert_eq!(first["sources"][0]["format"], "ctx-history-jsonl-v1");

    let search = json_output(ctx(&temp).args([
        "search",
        "parser test",
        "--provider",
        "custom",
        "--refresh",
        "off",
        "--json",
    ]));
    assert!(
        !search["results"].as_array().unwrap().is_empty(),
        "custom import was not searchable: {search:#}"
    );

    let second = json_output(ctx(&temp).args([
        "import",
        "--format",
        "ctx-history-jsonl-v1",
        "--path",
        &fixture,
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(second["totals"]["imported_sessions"], 0);
    assert_eq!(second["totals"]["imported_events"], 0);
    assert_eq!(second["totals"]["imported_edges"], 0);
    assert_eq!(second["totals"]["skipped"], 0, "{second:#}");
    assert_eq!(second["totals"]["change"], "no_op", "{second:#}");
}

#[test]
fn one_event_native_and_explicit_imports_defer_full_fts_merge() {
    let native = tempdir();
    let source_root = native.path().join("openhands-user");
    let conversation = source_root
        .join("v1_conversations")
        .join("one-event-maintenance");
    fs::create_dir_all(&conversation).unwrap();
    fs::write(
        conversation.join("0001-message.json"),
        json!({
            "id": "one-event-maintenance",
            "timestamp": "2026-07-26T12:00:00Z",
            "source": "user",
            "llm_message": {
                "role": "user",
                "content": "one event must stay searchable without a full merge"
            }
        })
        .to_string(),
    )
    .unwrap();
    let native_import = json_output(ctx(&native).args([
        "import",
        "--provider",
        "openhands",
        "--path",
        source_root.to_str().unwrap(),
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(native_import["totals"]["imported_events"], 1);
    assert_deferred_search_maintenance(&native);

    let explicit = tempdir();
    let fixture = explicit.path().join("one-event.jsonl");
    let records = [
        json!({
            "record_type": "manifest",
            "schema_version": "ctx-history-jsonl-v1"
        }),
        json!({
            "record_type": "source",
            "source_id": "one-event-source",
            "provider_key": "one-event-agent",
            "source_format": "one-event-jsonl",
            "raw_source_path": "/tmp/one-event.jsonl",
            "fingerprint": "sha256:one-event",
            "importer_version": "1.0.0",
            "observed_at": "2026-07-26T12:00:00Z",
            "machine_id": "fixture-host"
        }),
        json!({
            "record_type": "session",
            "source_id": "one-event-source",
            "session_id": "one-event-session",
            "started_at": "2026-07-26T12:00:00Z",
            "agent_type": "primary",
            "is_primary": true,
            "status": "completed"
        }),
        json!({
            "record_type": "event",
            "source_id": "one-event-source",
            "session_id": "one-event-session",
            "event_index": 0,
            "event_id": "one-event",
            "event_type": "message",
            "role": "user",
            "occurred_at": "2026-07-26T12:00:01Z",
            "payload": {"text": "explicit one event without a full merge"},
            "preview": "explicit one event without a full merge"
        }),
    ];
    fs::write(
        &fixture,
        records
            .into_iter()
            .map(|record| record.to_string())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
    let explicit_import = json_output(ctx(&explicit).args([
        "import",
        "--format",
        "ctx-history-jsonl-v1",
        "--path",
        fixture.to_str().unwrap(),
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(explicit_import["totals"]["imported_events"], 1);
    assert_deferred_search_maintenance(&explicit);
}

fn assert_deferred_search_maintenance(temp: &TempDir) {
    let connection = Connection::open(temp.path().join("work.sqlite")).unwrap();
    let pending: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM search_projection_stats
             WHERE key = 'event_search_maintenance_v1' AND value = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        pending, 1,
        "bounded bulk finish must leave maintenance debt instead of running a full merge"
    );
    let active_bulk: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM search_projection_stats
             WHERE key LIKE 'event_search_bulk_mode_v1%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(active_bulk, 0, "outer bulk guard must finish cleanly");
}

#[test]
fn import_custom_history_jsonl_format_imports_valid_rows_and_reports_rejections() {
    let temp = tempdir();
    let fixture = custom_history_fixture("malformed-mixed.jsonl");

    let import = json_output(ctx(&temp).args([
        "import",
        "--format",
        "ctx-history-jsonl-v1",
        "--path",
        &fixture,
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(import["totals"]["imported_sessions"], 1);
    assert_eq!(import["totals"]["imported_events"], 1);
    assert_eq!(import["totals"]["rejected_records"], 1);
    assert_eq!(import["sources"][0]["rejected_records"], 1);

    let search = json_output(ctx(&temp).args([
        "search",
        "Valid event before malformed record.",
        "--provider",
        "custom",
        "--refresh",
        "off",
        "--json",
    ]));
    assert!(
        !search["results"].as_array().unwrap().is_empty(),
        "custom import with rejections was not searchable: {search:#}"
    );
}

#[test]
fn all_invalid_custom_import_cleans_up_and_retries_after_source_is_fixed() {
    let temp = tempdir();
    let fixture = temp.path().join("custom-retry.jsonl");
    let records = |event_index: &str| {
        r#"{"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"}
{"record_type":"source","source_id":"retry-source","provider_key":"retry-agent","source_format":"retry-jsonl","cursor":{"after":{"stream":"retry-agent:retry-source","cursor":"1","observed_at":"2026-07-13T12:00:00Z"}}}
{"record_type":"session","source_id":"retry-source","session_id":"retry-session","started_at":"2026-07-13T12:00:00Z"}
{"record_type":"event","source_id":"retry-source","session_id":"retry-session","event_index":EVENT_INDEX,"event_type":"message","role":"user","occurred_at":"2026-07-13T12:00:01Z","payload":{"text":"retry oracle"}}
"#
        .replace("EVENT_INDEX", event_index)
    };
    fs::write(&fixture, records(r#""invalid""#)).unwrap();

    let failed = ctx(&temp)
        .args([
            "import",
            "--format",
            "ctx-history-jsonl-v1",
            "--path",
            fixture.to_str().unwrap(),
            "--json",
            "--progress",
            "none",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    let report: Value = serde_json::from_slice(&failed.stdout).unwrap();
    assert_eq!(report["outcome"], "failure", "{report:#}");
    assert_eq!(report["totals"]["imported_sessions"], 0, "{report:#}");
    assert_eq!(report["totals"]["imported_events"], 0, "{report:#}");
    assert_eq!(report["totals"]["rejected_records"], 1, "{report:#}");
    assert_eq!(report["totals"]["failed_sources"], 1, "{report:#}");
    let conn = Connection::open(temp.path().join("work.sqlite")).unwrap();
    for table in ["history_records", "sessions", "events", "sync_cursors"] {
        assert_eq!(
            sqlite_count(&conn, &format!("SELECT COUNT(*) FROM {table}")),
            0,
            "unexpected rejection-only rows in {table}: {report:#}"
        );
    }
    drop(conn);

    fs::write(&fixture, records("0")).unwrap();
    let retry = json_output(ctx(&temp).args([
        "import",
        "--format",
        "ctx-history-jsonl-v1",
        "--path",
        fixture.to_str().unwrap(),
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(retry["outcome"], "success", "{retry:#}");
    assert_eq!(retry["totals"]["imported_events"], 1, "{retry:#}");
}

#[test]
fn import_custom_history_format_is_not_a_native_provider_importer() {
    let temp = tempdir();
    let stderr = failure_stderr(ctx(&temp).args(["import", "--provider", "custom"]));
    assert!(stderr.contains("invalid value 'custom'"), "{stderr}");

    let fixture = custom_history_fixture("basic.jsonl");
    let stderr = failure_stderr(ctx(&temp).args([
        "import",
        "--format",
        "ctx-history-jsonl-v1",
        "--path",
        &fixture,
        "--all",
    ]));
    assert!(stderr.contains("--format"), "{stderr}");
    assert!(stderr.contains("--all"), "{stderr}");
}

#[test]
fn import_all_runs_enabled_history_source_plugins_for_external_shapes() {
    let temp = tempdir();
    let plugin_root = temp.path().join("history-plugins");
    let providers = ["dorkos", "openclaw", "hermes", "nanoclaw"];
    for provider in providers {
        write_history_source_plugin_at(&plugin_root, provider, true, None);
    }
    write_history_source_plugin_at(&plugin_root, "disabled-dorkos", false, None);

    let imported = json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin_root)
            .args(["import", "--all", "--json", "--progress", "none"]),
    );
    assert_eq!(imported["totals"]["imported_sources"], 4);
    assert_eq!(imported["totals"]["imported_sessions"], 4);
    assert_eq!(imported["totals"]["imported_events"], 4);
    let sources = imported["sources"].as_array().unwrap();
    for provider in providers {
        assert!(
            sources
                .iter()
                .any(|source| source["history_source"] == format!("{provider}/default")),
            "missing import source for {provider}: {sources:#?}"
        );
        let search = json_output(ctx(&temp).args([
            "search",
            &format!("{provider} plugin initial marker"),
            "--provider",
            "custom",
            "--refresh",
            "off",
            "--json",
        ]));
        assert!(
            !search["results"].as_array().unwrap().is_empty(),
            "{provider} plugin result was not searchable: {search:#}"
        );
    }
    assert!(!sources
        .iter()
        .any(|source| source["history_source"] == "disabled-dorkos/default"));
}

#[test]
fn import_all_discovers_and_imports_providers_together() {
    let temp = tempdir();
    copy_dir_all(
        Path::new(&provider_history_fixture("codex-sessions")),
        &temp.path().join(".codex").join("sessions"),
    );
    let pi_home = temp.path().join(".pi/agent/sessions/--workspace-example--");
    fs::create_dir_all(&pi_home).unwrap();
    write_pi_session_jsonl(
        &pi_home.join("2026-06-24T12-00-00-000Z_pi-session-docs-1.jsonl"),
        "pi-session-docs-1",
        "Inspect the provider metadata rows.",
    );

    let output = ctx(&temp)
        .args(["import", "--all", "--json", "--progress", "json"])
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["schema_version"], 2);
    assert!(stdout["totals"]["imported_sessions"].as_u64().unwrap() >= 3);
    let sources = stdout["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 2);
    assert!(sources.iter().any(|source| source["provider"] == "codex"));
    assert!(sources.iter().any(|source| source["provider"] == "pi"));

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains(r#""type":"ctx_progress""#), "{stderr}");
    assert!(stderr.contains(r#""phase":"finalizing""#), "{stderr}");
}

#[test]
fn import_all_without_sources_does_not_report_missing_explicit_path() {
    let temp = tempdir();
    let stderr = failure_stderr(ctx(&temp).args(["import", "--all", "--json"]));

    assert!(stderr.contains("no importable provider history sources found"));
    assert!(!stderr.contains("import path does not exist"), "{stderr}");
}

#[test]
fn import_all_discovers_sources_when_home_unset_and_userprofile_set() {
    let temp = tempdir();
    copy_dir_all(
        Path::new(&provider_history_fixture("codex-sessions")),
        &temp.path().join(".codex").join("sessions"),
    );

    let imported = json_output(
        ctx(&temp)
            .env_remove("HOME")
            .env("USERPROFILE", temp.path())
            .args(["import", "--all", "--json", "--progress", "none"]),
    );
    assert_eq!(imported["totals"]["imported_sources"], 1);
    assert_eq!(imported["totals"]["failed_sources"], 0);
    assert!(imported["sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|source| source["provider"] == "codex"));
}

#[test]
fn import_all_skips_empty_gemini_source() {
    let temp = tempdir();
    copy_dir_all(
        Path::new(&provider_history_fixture("codex-sessions")),
        &temp.path().join(".codex").join("sessions"),
    );
    fs::create_dir_all(temp.path().join(".gemini")).unwrap();

    let sources = json_output(ctx(&temp).args(["sources", "--json"]));
    let gemini = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["provider"] == "gemini")
        .unwrap();
    assert_eq!(gemini["status"], "empty");
    assert_eq!(gemini["native_import"], true);
    assert_eq!(gemini["importable"], false);

    let imported =
        json_output(ctx(&temp).args(["import", "--all", "--json", "--progress", "none"]));
    assert_eq!(imported["totals"]["imported_sources"], 1);
    assert_eq!(imported["totals"]["failed_sources"], 0);
    assert!(imported["sources"]
        .as_array()
        .unwrap()
        .iter()
        .all(|source| source["provider"] != "gemini"));
}

#[test]
fn import_all_reports_source_failure_without_losing_successes() {
    let temp = tempdir();
    copy_dir_all(
        Path::new(&provider_history_fixture("codex-sessions")),
        &temp.path().join(".codex").join("sessions"),
    );
    let opencode_dir = temp.path().join(".local/share/opencode");
    fs::create_dir_all(&opencode_dir).unwrap();
    fs::write(opencode_dir.join("opencode.db"), b"not sqlite").unwrap();

    let output = ctx(&temp)
        .args(["import", "--all", "--json", "--progress", "none"])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["schema_version"], 2);
    assert_eq!(stdout["totals"]["imported_sources"], 1);
    assert_eq!(stdout["totals"]["failed_sources"], 1);
    assert!(stdout["totals"]["imported_sessions"].as_u64().unwrap() > 0);
    let sources = stdout["sources"].as_array().unwrap();
    assert!(sources
        .iter()
        .any(|source| source["provider"] == "codex" && source["status"] == "success"));
    assert!(sources
        .iter()
        .any(|source| source["provider"] == "opencode" && source["status"] == "failure"));
    let opencode_failure = sources
        .iter()
        .find(|source| source["provider"] == "opencode")
        .unwrap();
    assert!(
        opencode_failure["error"]
            .as_str()
            .unwrap()
            .contains("not a database"),
        "{opencode_failure}"
    );
}

#[test]
fn failed_import_attempt_does_not_count_as_indexed_history() {
    let temp = tempdir();
    let opencode_dir = temp.path().join(".local/share/opencode");
    fs::create_dir_all(&opencode_dir).unwrap();
    fs::write(opencode_dir.join("opencode.db"), b"not sqlite").unwrap();

    ctx(&temp)
        .args(["import", "--all", "--json", "--progress", "none"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("all import sources failed"));

    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(status["indexed_items"], 0);
    assert_eq!(status["indexed_sources"], 0);
}

#[derive(Debug, PartialEq, Eq)]
struct ProviderAuthoritySnapshot {
    sessions: Vec<String>,
    events: Vec<String>,
    capture_sources: Vec<String>,
    source_routes: Vec<String>,
    cursors: Vec<String>,
    history_records: Vec<String>,
}

fn authority_rows(conn: &Connection, sql: &str, selector: &str) -> Vec<String> {
    conn.prepare(sql)
        .unwrap()
        .query_map(params![selector], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

fn provider_authority_snapshot(temp: &TempDir, provider: &str) -> ProviderAuthoritySnapshot {
    let conn = Connection::open(temp.path().join("work.sqlite")).unwrap();
    ProviderAuthoritySnapshot {
        sessions: authority_rows(
            &conn,
            "SELECT id || ':' || COALESCE(external_session_id, '')
             FROM sessions WHERE provider = ?1 ORDER BY id",
            provider,
        ),
        events: authority_rows(
            &conn,
            "SELECT e.id || ':' || COALESCE(e.session_id, '') || ':' || e.event_type
             FROM events e
             JOIN sessions s ON s.id = e.session_id
             WHERE s.provider = ?1
             ORDER BY e.id",
            provider,
        ),
        capture_sources: authority_rows(
            &conn,
            "SELECT id || ':' || source_format || ':' || COALESCE(external_session_id, '')
             FROM capture_sources WHERE provider = ?1 ORDER BY id",
            provider,
        ),
        source_routes: authority_rows(
            &conn,
            "SELECT capture_source_id || ':' || source_format || ':' || alias_group_identity
             FROM capture_source_provider_routes
             WHERE provider = ?1
             ORDER BY capture_source_id",
            provider,
        ),
        cursors: authority_rows(
            &conn,
            "SELECT stream || ':' || cursor
             FROM sync_cursors
             WHERE stream LIKE ?1
             ORDER BY stream",
            &format!("provider:{provider}:%"),
        ),
        history_records: authority_rows(
            &conn,
            "SELECT DISTINCT h.id || ':' || h.kind
             FROM history_records h
             JOIN sessions s ON s.history_record_id = h.id
             WHERE s.provider = ?1
             ORDER BY h.id",
            provider,
        ),
    }
}

fn foreground_setup(temp: &TempDir) -> Value {
    json_output(ctx(temp).args([
        "setup",
        "--wait",
        "--no-daemon",
        "--json",
        "--progress",
        "none",
    ]))
}

fn setup_source_report<'a>(setup: &'a Value, provider: &str) -> &'a Value {
    let sources = setup["import"]["sources"].as_array().unwrap();
    assert_eq!(
        sources
            .iter()
            .filter(|source| source["provider"] == provider)
            .count(),
        1,
        "{provider} must appear exactly once in {setup:#}"
    );
    sources
        .iter()
        .find(|source| source["provider"] == provider)
        .unwrap()
}

fn assert_searchable_and_showable(temp: &TempDir, provider: &str, query: &str) -> (String, String) {
    let search = json_output(ctx(temp).args([
        "search",
        query,
        "--provider",
        provider,
        "--refresh",
        "off",
        "--json",
    ]));
    assert_search_provider_oracle(&search, provider, query, 1, "message");
    let result = &search["results"][0];
    let event_id = result["ctx_event_id"].as_str().unwrap().to_owned();
    let session_id = result["ctx_session_id"].as_str().unwrap().to_owned();

    let shown_event = json_output(ctx(temp).args([
        "show", "event", &event_id, "--window", "1", "--format", "json",
    ]));
    assert_eq!(shown_event["payload_type"], "event_window");
    assert_eq!(shown_event["event"]["ctx_event_id"], event_id);
    assert_eq!(shown_event["event"]["ctx_session_id"], session_id);

    let shown_session =
        json_output(ctx(temp).args(["show", "session", &session_id, "--format", "json"]));
    assert_eq!(shown_session["payload_type"], "session_transcript");
    assert_eq!(shown_session["session"]["item_id"], session_id);
    (session_id, event_id)
}

#[test]
fn cold_cutover_fresh_foreground_setup_is_canonical_searchable_and_showable() {
    let temp = tempdir();
    write_codex_setup_session(&temp);
    let db_path = temp.path().join("work.sqlite");
    assert!(!db_path.exists());

    let setup = foreground_setup(&temp);

    assert!(db_path.exists());
    assert_eq!(setup["mode"], "ready", "{setup:#}");
    assert_eq!(setup["import"]["ran"], true, "{setup:#}");
    assert_eq!(setup["import"]["totals"]["failed_sources"], 0, "{setup:#}");
    assert_eq!(
        setup["import"]["totals"]["imported_sources"], 1,
        "{setup:#}"
    );
    assert_eq!(
        setup["import"]["totals"]["imported_sessions"], 1,
        "{setup:#}"
    );
    assert_eq!(
        setup_source_report(&setup, "codex")["change"],
        "changed",
        "{setup:#}"
    );

    let authority = provider_authority_snapshot(&temp, "codex");
    assert_eq!(authority.sessions.len(), 1, "{authority:#?}");
    assert_eq!(
        authority.events.len() as u64,
        setup["import"]["totals"]["imported_events"]
            .as_u64()
            .unwrap(),
        "{authority:#?}"
    );
    assert_eq!(authority.capture_sources.len(), 1, "{authority:#?}");
    assert_eq!(authority.source_routes.len(), 1, "{authority:#?}");
    assert_eq!(authority.cursors.len(), 1, "{authority:#?}");
    assert_eq!(authority.history_records.len(), 1, "{authority:#?}");
    assert_searchable_and_showable(&temp, "codex", "setup should import");
}

#[test]
fn cold_cutover_mixed_setup_seeds_codex_once_and_imports_claude_nativepath() {
    let temp = tempdir();
    write_codex_setup_session(&temp);
    let claude_query = "mixed setup claude nativepath authority";
    install_default_claude_fixture(&temp, claude_query);
    assert!(!temp.path().join("work.sqlite").exists());

    let setup = foreground_setup(&temp);

    assert_eq!(setup["import"]["totals"]["failed_sources"], 0, "{setup:#}");
    assert_eq!(
        setup["import"]["totals"]["imported_sources"], 2,
        "{setup:#}"
    );
    assert_eq!(
        setup["import"]["totals"]["imported_sessions"], 2,
        "{setup:#}"
    );
    assert_eq!(
        setup["import"]["sources"].as_array().unwrap().len(),
        2,
        "{setup:#}"
    );
    assert_eq!(setup_source_report(&setup, "codex")["change"], "changed");
    assert_eq!(setup_source_report(&setup, "claude")["change"], "changed");

    let codex = provider_authority_snapshot(&temp, "codex");
    let claude = provider_authority_snapshot(&temp, "claude");
    assert_eq!(codex.sessions.len(), 1, "{codex:#?}");
    assert_eq!(claude.sessions.len(), 1, "{claude:#?}");
    assert_eq!(codex.capture_sources.len(), 1, "{codex:#?}");
    assert_eq!(claude.capture_sources.len(), 1, "{claude:#?}");
    assert_eq!(codex.source_routes.len(), 1, "{codex:#?}");
    assert_eq!(claude.source_routes.len(), 1, "{claude:#?}");
    assert_eq!(codex.cursors.len(), 1, "{codex:#?}");
    assert_eq!(claude.cursors.len(), 1, "{claude:#?}");
    assert_eq!(codex.history_records.len(), 1, "{codex:#?}");
    assert_eq!(claude.history_records.len(), 1, "{claude:#?}");
    assert_eq!(
        (codex.events.len() + claude.events.len()) as u64,
        setup["import"]["totals"]["imported_events"]
            .as_u64()
            .unwrap(),
        "combined report must equal committed provider authority"
    );

    assert_searchable_and_showable(&temp, "codex", "setup should import");
    assert_searchable_and_showable(&temp, "claude", claude_query);
}

#[test]
fn cold_cutover_existing_store_setup_preserves_prior_provider_authority() {
    let temp = tempdir();
    let pi_query = "existing store pi authority survives setup";
    install_default_pi_fixture(&temp, pi_query);
    let initial = json_output(ctx(&temp).args([
        "import",
        "--all",
        "--no-daemon",
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(initial["totals"]["failed_sources"], 0, "{initial:#}");
    assert_eq!(initial["totals"]["imported_sources"], 1, "{initial:#}");
    let pi_before = provider_authority_snapshot(&temp, "pi");
    let pi_ids_before = assert_searchable_and_showable(&temp, "pi", pi_query);

    write_codex_setup_session(&temp);
    let setup = foreground_setup(&temp);

    assert_eq!(setup["import"]["totals"]["failed_sources"], 0, "{setup:#}");
    assert_eq!(setup_source_report(&setup, "codex")["change"], "changed");
    assert_eq!(setup_source_report(&setup, "pi")["change"], "no_op");
    assert_eq!(provider_authority_snapshot(&temp, "pi"), pi_before);
    assert_eq!(
        assert_searchable_and_showable(&temp, "pi", pi_query),
        pi_ids_before
    );
    assert_searchable_and_showable(&temp, "codex", "setup should import");
}

#[test]
fn cold_cutover_repeated_setup_and_import_are_authority_level_noops() {
    let temp = tempdir();
    write_codex_setup_session(&temp);

    let first = foreground_setup(&temp);
    assert_eq!(setup_source_report(&first, "codex")["change"], "changed");
    let authority = provider_authority_snapshot(&temp, "codex");
    let ids = assert_searchable_and_showable(&temp, "codex", "setup should import");

    let second = foreground_setup(&temp);
    assert_eq!(second["import"]["totals"]["change"], "no_op", "{second:#}");
    assert_eq!(
        second["import"]["totals"]["imported_sessions"], 0,
        "{second:#}"
    );
    assert_eq!(
        second["import"]["totals"]["imported_events"], 0,
        "{second:#}"
    );
    assert_eq!(
        setup_source_report(&second, "codex")["change"],
        "no_op",
        "{second:#}"
    );
    assert_eq!(provider_authority_snapshot(&temp, "codex"), authority);

    let imported = json_output(ctx(&temp).args([
        "import",
        "--all",
        "--no-daemon",
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(imported["totals"]["change"], "no_op", "{imported:#}");
    assert_eq!(imported["totals"]["imported_sessions"], 0, "{imported:#}");
    assert_eq!(imported["totals"]["imported_events"], 0, "{imported:#}");
    assert_eq!(imported["sources"].as_array().unwrap().len(), 1);
    assert_eq!(imported["sources"][0]["provider"], "codex");
    assert_eq!(imported["sources"][0]["change"], "no_op");
    assert_eq!(provider_authority_snapshot(&temp, "codex"), authority);
    assert_eq!(
        assert_searchable_and_showable(&temp, "codex", "setup should import"),
        ids
    );
}
