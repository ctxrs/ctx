use super::{
    assert_daemon_process_running, assert_no_daemon_autostart_mutation, ctx, support::*,
    wait_for_daemon_status, write_active_daemon_upgrade_handoff, write_codex_setup_session,
};
use rusqlite::OpenFlags;
use std::{
    io::Read,
    process::{Child, Command as StdCommand, Stdio},
};

struct SourceRefreshDaemon {
    child: Option<Child>,
}

impl Drop for SourceRefreshDaemon {
    fn drop(&mut self) {
        if let Err(error) =
            terminate_and_reap_test_child(&mut self.child, "setup source-refresh daemon")
        {
            if std::thread::panicking() {
                eprintln!("setup source-refresh daemon teardown also failed: {error}");
            } else {
                panic!("setup source-refresh daemon teardown failed: {error}");
            }
        }
    }
}

fn start_full_source_refresh_daemon(temp: &TempDir) -> SourceRefreshDaemon {
    bind_test_ctx_binary(temp);
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

fn wait_for_relational_projection(temp: &TempDir, generation: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = json_output(ctx(temp).args(["status", "--format=json"]));
        if status["relational"]["status"] == "ready"
            && status["relational"]["active_core_generation_id"] == generation
        {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for relational projection at generation {generation}: {status:#}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn published_generation(report: &Value) -> String {
    report["sources"][0]["published_generation"]
        .as_str()
        .expect("import report should identify its published generation")
        .to_owned()
}

#[test]
fn deprecated_partial_remains_a_noop_without_bypassing_daemon_only_writes() {
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
        .failure()
        .stderr(predicate::str::contains(
            "warning: --partial is deprecated and no longer changes import behavior; tolerant import is now unconditional",
        ))
        .stderr(predicate::str::contains(
            "no foreground writer was started",
        ));
    assert_no_daemon_autostart_mutation(&temp);
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
            "--format=json",
            "--progress",
            "json",
        ])
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["schema_version"], 2);
    assert!(stdout["totals"]["current_source_count"]
        .as_u64()
        .is_some_and(|count| count >= 1));
    assert_eq!(stdout["sources"][0]["status"], "published");
    assert!(stdout["sources"][0]["published_generation"].is_string());

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains(r#""type":"ctx_progress""#), "{stderr}");
    assert!(stderr.contains(r#""operation":"import""#), "{stderr}");
}

#[test]
fn human_import_is_outcome_first_without_internal_generation_fields() {
    let temp = tempdir();
    let _daemon = start_full_source_refresh_daemon(&temp);
    let fixture = provider_history_fixture("codex-sessions");
    let output = ctx(&temp)
        .args([
            "import",
            "--provider",
            "codex",
            "--path",
            &fixture,
            "--no-daemon",
            "--progress",
            "plain",
        ])
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.starts_with("✓ History import completed\n"),
        "{stdout}"
    );
    assert!(stdout.contains("\nCurrent index\n"), "{stdout}");
    for internal in [
        "failure_scope",
        "published_generation",
        "previous_generation",
        "generation_changed",
        "resume_mode",
    ] {
        assert!(!stdout.contains(internal), "{stdout}");
    }

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.ends_with("Published the source for indexing.\n"),
        "{stderr}"
    );
    assert!(!stderr.contains("generation"), "{stderr}");
}

#[test]
fn machine_readable_native_import_recovers_daemon_without_polluting_json() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");

    let import = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(import["schema_version"], 2);
    assert_eq!(import["sources"][0]["status"], "published");
    let running = wait_for_daemon_status(&temp, "running", true, "import");
    assert_eq!(running["daemon"]["start_mode"], "auto");
}

#[test]
fn progress_json_native_import_recovers_enabled_daemon() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");

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
    let running = wait_for_daemon_status(&temp, "running", true, "import");
    assert_eq!(running["daemon"]["start_mode"], "auto");
}

#[test]
fn machine_readable_native_import_bounds_upgrade_handoff_recovery() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");
    write_active_daemon_upgrade_handoff(&temp);

    let started = Instant::now();
    let output = ctx(&temp)
        .args([
            "import",
            "--provider",
            "codex",
            "--path",
            &fixture,
            "--format=json",
            "--progress",
            "none",
        ])
        .timeout(Duration::from_secs(15))
        .assert()
        .failure()
        .get_output()
        .clone();

    assert!(
        started.elapsed() < Duration::from_secs(20),
        "enabled-daemon handoff exceeded the bounded foreground recovery window"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("timed out waiting"), "{stderr}");
}

#[test]
fn human_native_import_starts_a_reported_daemon_process() {
    let temp = finite_daemon_test_root();
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
        .env("CTX_DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS", "60")
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
        "--input-format",
        "ctx-history-jsonl-v1",
        "--path",
        &fixture,
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(first["totals"]["current_indexed_documents"], 2);
    assert_eq!(first["totals"]["current_rejected_records"], 0);
    assert_eq!(first["sources"][0]["provider"], "custom");
    assert_eq!(first["sources"][0]["source_format"], "ctx_history_jsonl_v1");

    let search = json_output(ctx(&temp).args([
        "search",
        "parser test",
        "--provider",
        "custom",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert!(
        !search["results"].as_array().unwrap().is_empty(),
        "custom import was not searchable: {search:#}"
    );

    let second = json_output(ctx(&temp).args([
        "import",
        "--input-format",
        "ctx-history-jsonl-v1",
        "--path",
        &fixture,
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(second["totals"]["current_indexed_documents"], 2);
    assert_eq!(second["totals"]["current_rejected_records"], 0);
    assert_eq!(second["totals"]["change"], "no_op", "{second:#}");
}

#[test]
fn one_event_native_and_explicit_imports_publish_tantivy_and_relational_projections() {
    let native = tempdir();
    let _native_daemon = start_full_source_refresh_daemon(&native);
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
                "content": "one event must publish through a Tantivy generation"
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
        "--no-daemon",
        "--format=json",
        "--progress",
        "none",
    ]));
    let native_generation = published_generation(&native_import);
    assert_eq!(native_import["sources"][0]["status"], "published");
    assert_eq!(
        native_import["sources"][0]["daemon_request_metadata"]["owner"],
        "daemon"
    );
    let native_status = wait_for_relational_projection(&native, &native_generation);
    assert_eq!(native_status["lexical"]["indexed_documents"], 1);
    assert_eq!(native_status["relational"]["event_count"], 1);
    assert!(data_root(&native)
        .join("search/lexical/meta.json")
        .is_file());
    assert!(data_root(&native).join("relational.sqlite").is_file());
    let native_search = json_output(ctx(&native).args([
        "search",
        "one event must publish",
        "--provider",
        "openhands",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(native_search["retrieval"]["index"], "source_backed");
    assert_eq!(
        native_search["retrieval"]["generation_id"],
        native_generation
    );
    assert_eq!(native_search["results"].as_array().unwrap().len(), 1);

    let explicit = tempdir();
    let _explicit_daemon = start_full_source_refresh_daemon(&explicit);
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
            "payload": {"text": "explicit one event in a Tantivy generation"},
            "preview": "explicit one event in a Tantivy generation"
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
        "--input-format",
        "ctx-history-jsonl-v1",
        "--path",
        fixture.to_str().unwrap(),
        "--no-daemon",
        "--format=json",
        "--progress",
        "none",
    ]));
    let explicit_generation = published_generation(&explicit_import);
    assert_eq!(explicit_import["sources"][0]["status"], "published");
    assert_eq!(explicit_import["sources"][0]["catalog_changed"], true);
    assert_eq!(
        explicit_import["sources"][0]["daemon_request_metadata"]["owner"],
        "daemon"
    );
    let explicit_status = wait_for_relational_projection(&explicit, &explicit_generation);
    assert_eq!(explicit_status["lexical"]["indexed_documents"], 1);
    assert_eq!(explicit_status["relational"]["event_count"], 1);
    assert!(data_root(&explicit)
        .join("search/lexical/meta.json")
        .is_file());
    assert!(data_root(&explicit).join("relational.sqlite").is_file());
    let explicit_search = json_output(ctx(&explicit).args([
        "search",
        "explicit one event",
        "--provider",
        "custom",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(explicit_search["retrieval"]["index"], "source_backed");
    assert_eq!(
        explicit_search["retrieval"]["generation_id"],
        explicit_generation
    );
    assert_eq!(explicit_search["results"].as_array().unwrap().len(), 1);
}

#[test]
fn import_custom_history_jsonl_format_imports_valid_rows_and_reports_rejections() {
    let temp = tempdir();
    let fixture = custom_history_fixture("malformed-mixed.jsonl");

    let import = json_output(ctx(&temp).args([
        "import",
        "--input-format",
        "ctx-history-jsonl-v1",
        "--path",
        &fixture,
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(import["totals"]["current_indexed_documents"], 1);
    assert_eq!(import["totals"]["current_rejected_records"], 1);
    assert_eq!(import["sources"][0]["current_rejected_records"], 1);

    let search = json_output(ctx(&temp).args([
        "search",
        "Valid event before malformed record.",
        "--provider",
        "custom",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert!(
        !search["results"].as_array().unwrap().is_empty(),
        "custom import with rejections was not searchable: {search:#}"
    );
}

#[test]
fn custom_history_structural_manifest_failures_fail_closed_and_recover() {
    let temp = tempdir();
    let _daemon = start_full_source_refresh_daemon(&temp);
    let fixture = temp.path().join("structural-manifest.jsonl");
    let fixture_arg = fixture.to_str().unwrap();

    let cases: [(&str, &[u8], &str, &str); 3] = [
        (
            "missing",
            b"",
            "invalid_source",
            "invalid capture payload",
        ),
        (
            "unsupported",
            b"{\"record_type\":\"manifest\",\"schema_version\":\"ctx-history-jsonl-v999\"}\n",
            "schema_incompatible",
            "unsupported provider schema",
        ),
        (
            "duplicate",
            b"{\"record_type\":\"manifest\",\"schema_version\":\"ctx-history-jsonl-v1\"}\n{\"record_type\":\"manifest\",\"schema_version\":\"ctx-history-jsonl-v1\"}\n",
            "invalid_source",
            "invalid capture payload",
        ),
    ];
    for (name, bytes, failure_kind, cli_classification) in cases {
        fs::write(&fixture, bytes).unwrap();
        let stderr = failure_stderr(ctx(&temp).args([
            "import",
            "--input-format",
            "ctx-history-jsonl-v1",
            "--path",
            fixture_arg,
            "--no-daemon",
            "--format=json",
            "--progress",
            "none",
        ]));
        assert!(stderr.contains(failure_kind), "{name}: {stderr}");
        assert!(stderr.contains(cli_classification), "{name}: {stderr}");
        assert!(
            stderr.contains("retained generation"),
            "{name} did not report fail-closed retention: {stderr}"
        );
    }

    fs::write(
        &fixture,
        concat!(
            "{\"record_type\":\"manifest\",\"schema_version\":\"ctx-history-jsonl-v1\"}\n",
            "{\"record_type\":\"source\",\"source_id\":\"recovered-source\",\"provider_key\":\"recovered-agent\",\"source_format\":\"recovered-jsonl\"}\n",
            "{\"record_type\":\"session\",\"source_id\":\"recovered-source\",\"session_id\":\"recovered-session\",\"started_at\":\"2026-07-31T12:00:00Z\",\"agent_type\":\"primary\",\"is_primary\":true}\n",
            "{malformed-json}\n",
            "{\"record_type\":\"event\",\"source_id\":\"recovered-source\",\"session_id\":\"recovered-session\",\"event_index\":0,\"event_type\":\"message\",\"role\":\"user\",\"occurred_at\":\"2026-07-31T12:00:01Z\",\"payload\":{\"text\":\"structural manifest recovery oracle\"}}\n",
        ),
    )
    .unwrap();
    let recovered = json_output(ctx(&temp).args([
        "import",
        "--input-format",
        "ctx-history-jsonl-v1",
        "--path",
        fixture_arg,
        "--no-daemon",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(recovered["outcome"], "success", "{recovered:#}");
    assert_eq!(
        recovered["totals"]["current_indexed_documents"], 1,
        "{recovered:#}"
    );
    assert_eq!(
        recovered["totals"]["current_rejected_records"], 1,
        "{recovered:#}"
    );
}

#[test]
fn all_invalid_custom_source_publishes_empty_then_refreshes_after_fix() {
    let temp = tempdir();
    let _daemon = start_full_source_refresh_daemon(&temp);
    let fixture = temp.path().join("custom-retry.jsonl");
    let records = |event_index: &str| {
        r#"{"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"}
{"record_type":"source","source_id":"retry-source","provider_key":"retry-agent","source_format":"retry-jsonl","cursor":{"after":{"stream":"retry-agent:retry-source","cursor":"1","observed_at":"2026-07-13T12:00:00Z"}}}
{"record_type":"session","source_id":"retry-source","session_id":"retry-session","started_at":"2026-07-13T12:00:00Z","agent_type":"primary","is_primary":true,"status":"completed"}
{"record_type":"event","source_id":"retry-source","session_id":"retry-session","event_index":EVENT_INDEX,"event_id":"retry-event","event_type":"message","role":"user","occurred_at":"2026-07-13T12:00:01Z","payload":{"text":"retry oracle"},"preview":"retry oracle"}
"#
        .replace("EVENT_INDEX", event_index)
    };
    fs::write(&fixture, records(r#""invalid""#)).unwrap();

    let empty = json_output(ctx(&temp).args([
        "import",
        "--input-format",
        "ctx-history-jsonl-v1",
        "--path",
        fixture.to_str().unwrap(),
        "--no-daemon",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(empty["outcome"], "success", "{empty:#}");
    assert_eq!(empty["sources"][0]["catalog_changed"], true, "{empty:#}");
    assert_eq!(
        empty["sources"][0]["current_indexed_documents"], 0,
        "{empty:#}"
    );
    let empty_generation = published_generation(&empty);
    let empty_status = wait_for_relational_projection(&temp, &empty_generation);
    assert_eq!(
        empty_status["lexical"]["indexed_documents"], 0,
        "{empty_status:#}"
    );
    assert_eq!(
        empty_status["relational"]["event_count"], 0,
        "{empty_status:#}"
    );

    fs::write(&fixture, records("0")).unwrap();
    let retry = json_output(ctx(&temp).args([
        "import",
        "--input-format",
        "ctx-history-jsonl-v1",
        "--path",
        fixture.to_str().unwrap(),
        "--no-daemon",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(retry["outcome"], "success", "{retry:#}");
    assert_eq!(retry["sources"][0]["catalog_changed"], false, "{retry:#}");
    let generation = published_generation(&retry);
    assert_ne!(generation, empty_generation);
    let status = wait_for_relational_projection(&temp, &generation);
    assert_eq!(status["lexical"]["indexed_documents"], 1, "{status:#}");
    assert_eq!(status["relational"]["event_count"], 1, "{status:#}");
    let search = json_output(ctx(&temp).args([
        "search",
        "retry oracle",
        "--provider",
        "custom",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(search["retrieval"]["generation_id"], generation);
    assert_eq!(search["results"].as_array().unwrap().len(), 1, "{search:#}");
}

#[test]
fn import_custom_history_format_is_not_a_native_provider_importer() {
    let temp = tempdir();
    let stderr = failure_stderr(ctx(&temp).args(["import", "--provider", "custom"]));
    assert!(stderr.contains("invalid value 'custom'"), "{stderr}");

    let fixture = custom_history_fixture("basic.jsonl");
    let stderr = failure_stderr(ctx(&temp).args([
        "import",
        "--input-format",
        "ctx-history-jsonl-v1",
        "--path",
        &fixture,
        "--all",
    ]));
    assert!(stderr.contains("--input-format"), "{stderr}");
    assert!(stderr.contains("--all"), "{stderr}");
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
        .args(["import", "--all", "--format=json", "--progress", "json"])
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["schema_version"], 2);
    assert!(
        stdout["totals"]["current_indexed_documents"]
            .as_u64()
            .is_some_and(|count| count >= 3),
        "{stdout:#}"
    );
    assert!(
        stdout["totals"]["current_source_count"]
            .as_u64()
            .is_some_and(|count| count >= 2),
        "{stdout:#}"
    );
    assert_eq!(
        stdout["sources"][0]["source_format"],
        "provider_authoritative_all"
    );

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains(r#""type":"ctx_progress""#), "{stderr}");
    assert!(stderr.contains(r#""phase":"published""#), "{stderr}");
}

#[test]
fn import_all_without_sources_does_not_report_missing_explicit_path() {
    let temp = tempdir();
    let report = json_output(ctx(&temp).args(["import", "--all", "--format=json"]));
    assert_eq!(report["outcome"], "success", "{report:#}");
    assert!(
        matches!(
            report["totals"]["change"].as_str(),
            Some("changed" | "no_op")
        ),
        "{report:#}"
    );
    assert_eq!(report["totals"]["current_source_count"], 0, "{report:#}");
    assert_eq!(
        report["totals"]["current_indexed_documents"], 0,
        "{report:#}"
    );
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
            .args(["import", "--all", "--format=json", "--progress", "none"]),
    );
    assert!(imported["totals"]["current_source_count"]
        .as_u64()
        .is_some_and(|count| count >= 1));
    assert!(imported["totals"].get("failed_sources").is_none());
    assert_eq!(
        imported["sources"][0]["source_format"],
        "provider_authoritative_all"
    );
}

#[test]
fn import_all_skips_empty_gemini_source() {
    let temp = tempdir();
    copy_dir_all(
        Path::new(&provider_history_fixture("codex-sessions")),
        &temp.path().join(".codex").join("sessions"),
    );
    fs::create_dir_all(temp.path().join(".gemini")).unwrap();

    let sources = json_output(ctx(&temp).args(["sources", "--format=json"]));
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
        json_output(ctx(&temp).args(["import", "--all", "--format=json", "--progress", "none"]));
    assert!(imported["totals"]["current_source_count"]
        .as_u64()
        .is_some_and(|count| count >= 1));
    assert!(imported["totals"].get("failed_sources").is_none());
}

#[test]
fn import_all_fails_atomically_when_one_source_is_invalid() {
    let temp = finite_daemon_test_root();
    copy_dir_all(
        Path::new(&provider_history_fixture("codex-sessions")),
        &temp.path().join(".codex").join("sessions"),
    );
    let opencode_dir = temp.path().join(".local/share/opencode");
    fs::create_dir_all(&opencode_dir).unwrap();
    fs::write(opencode_dir.join("opencode.db"), b"not sqlite").unwrap();

    let output = ctx(&temp)
        .args(["import", "--all", "--format=json", "--progress", "none"])
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("source-backed scan failed for opencode")
            && stderr.contains("not a database"),
        "{stderr}"
    );
}

#[test]
fn failed_import_attempt_does_not_count_as_indexed_history() {
    let temp = tempdir();
    let opencode_dir = temp.path().join(".local/share/opencode");
    fs::create_dir_all(&opencode_dir).unwrap();
    fs::write(opencode_dir.join("opencode.db"), b"not sqlite").unwrap();

    ctx(&temp)
        .args(["import", "--all", "--format=json", "--progress", "none"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "source-backed scan failed for opencode",
        ));

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert!(
        matches!(
            status["lexical"]["status"].as_str(),
            Some("unavailable" | "pending")
        ),
        "{status:#}"
    );
    assert!(
        matches!(
            status["history_epoch"]["status"].as_str(),
            Some("unavailable" | "pending")
        ),
        "{status:#}"
    );
    assert_ne!(status["lexical"]["status"], "ready", "{status:#}");
    assert_ne!(status["history_epoch"]["status"], "ready", "{status:#}");
    assert_eq!(status["initialized"], false, "{status:#}");
    assert!(
        status
            .get("indexed_events")
            .and_then(serde_json::Value::as_u64)
            .is_none_or(|count| count == 0),
        "{status:#}"
    );
}

#[derive(Debug, PartialEq, Eq)]
struct ProviderProjectionSnapshot {
    generation: String,
    sessions: Vec<String>,
    events: Vec<String>,
    sources: Vec<String>,
}

fn relational_rows(conn: &Connection, sql: &str, selector: &str) -> Vec<String> {
    conn.prepare(sql)
        .unwrap()
        .query_map(params![selector], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

fn provider_projection_snapshot(temp: &TempDir, provider: &str) -> ProviderProjectionSnapshot {
    let status = json_output(ctx(temp).args(["status", "--format=json"]));
    let generation = status["lexical"]["generation_id"]
        .as_str()
        .expect("source-backed lexical generation")
        .to_owned();
    let status = wait_for_relational_projection(temp, &generation);
    assert_eq!(
        status["relational"]["active_core_generation_id"],
        generation
    );
    let relational_path = data_root(temp).join("relational.sqlite");
    let conn =
        Connection::open_with_flags(&relational_path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    ProviderProjectionSnapshot {
        generation,
        sessions: relational_rows(
            &conn,
            "SELECT ctx_session_id || ':' || COALESCE(provider_session_id, '')
             FROM ctx_sessions WHERE provider = ?1 ORDER BY ctx_session_id",
            provider,
        ),
        events: relational_rows(
            &conn,
            "SELECT ctx_event_id || ':' || ctx_session_id || ':' || event_type
             FROM ctx_events WHERE provider = ?1 ORDER BY ctx_event_id",
            provider,
        ),
        sources: relational_rows(
            &conn,
            "SELECT source_id || ':' || source_format || ':' || content_digest_hex
             FROM source_backed_sources
             WHERE provider = ?1
             ORDER BY source_id",
            provider,
        ),
    }
}

fn ready_setup(temp: &TempDir) -> Value {
    json_output(ctx(temp).args(["setup", "--wait", "--format=json", "--progress", "none"]))
}

fn generation_manifest(temp: &TempDir, generation: &str) -> Value {
    let path = data_root(temp)
        .join("search/lexical/ctx-generations")
        .join(format!("{generation}.json"));
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn manifest_providers(temp: &TempDir, generation: &str) -> Vec<String> {
    let manifest = generation_manifest(temp, generation);
    let mut providers = manifest["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|source| {
            source["observation"]["source"]["provider"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect::<Vec<_>>();
    providers.sort();
    providers
}

fn assert_searchable_and_showable(temp: &TempDir, provider: &str, query: &str) -> (String, String) {
    let search = json_output(ctx(temp).args([
        "search",
        query,
        "--provider",
        provider,
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(search["retrieval"]["index"], "source_backed", "{search:#}");
    assert_eq!(search["filters"]["provider"], provider, "{search:#}");
    assert_eq!(search["results"].as_array().unwrap().len(), 1, "{search:#}");
    let result = &search["results"][0];
    assert_eq!(result["provider"], provider, "{result:#}");
    assert!(
        result["snippet"]
            .as_str()
            .is_some_and(|snippet| snippet.contains(query)),
        "{result:#}"
    );
    let event_id = result["ctx_event_id"].as_str().unwrap().to_owned();
    let session_id = result["ctx_session_id"].as_str().unwrap().to_owned();

    let shown_event = json_output(ctx(temp).args([
        "show", "event", &event_id, "--window", "1", "--format", "json",
    ]));
    assert_eq!(shown_event["payload_type"], "event_window");
    assert_eq!(shown_event["ctx_event_id"], event_id);
    assert_eq!(shown_event["ctx_session_id"], session_id);
    assert_eq!(shown_event["event"]["content"]["origin"], "provider_source");
    assert_eq!(shown_event["event"]["content"]["source_verified"], true);

    let shown_session =
        json_output(ctx(temp).args(["show", "session", &session_id, "--format", "json"]));
    assert_eq!(shown_session["payload_type"], "session_transcript");
    assert_eq!(shown_session["ctx_session_id"], session_id);
    assert_eq!(shown_session["provider"], provider);
    (session_id, event_id)
}

#[test]
fn fresh_setup_publishes_provider_sources_to_tantivy_and_relational() {
    let temp = tempdir();
    write_codex_setup_session(&temp);
    let _daemon = start_full_source_refresh_daemon(&temp);

    let setup = ready_setup(&temp);

    assert_eq!(setup["schema_version"], 2, "{setup:#}");
    assert_eq!(setup["mode"], "ready", "{setup:#}");
    assert_eq!(setup["history_epoch"]["status"], "ready", "{setup:#}");
    assert_eq!(setup["lexical"]["status"], "ready", "{setup:#}");
    assert_eq!(setup["refresh_request"]["status"], "published", "{setup:#}");
    assert_eq!(setup["refresh_request"]["source_count"], 1, "{setup:#}");
    let generation = setup["lexical"]["generation_id"].as_str().unwrap();
    assert_eq!(
        manifest_providers(&temp, generation),
        vec!["codex".to_owned()]
    );
    let status = wait_for_relational_projection(&temp, generation);
    assert_eq!(status["relational"]["source_count"], 1, "{status:#}");
    assert_eq!(status["relational"]["session_count"], 1, "{status:#}");
    assert_eq!(status["relational"]["event_count"], 1, "{status:#}");
    assert!(data_root(&temp).join("search/lexical/meta.json").is_file());
    assert!(data_root(&temp).join("relational.sqlite").is_file());

    let projection = provider_projection_snapshot(&temp, "codex");
    assert_eq!(projection.generation, generation);
    assert_eq!(projection.sessions.len(), 1, "{projection:#?}");
    assert_eq!(projection.events.len(), 1, "{projection:#?}");
    assert_eq!(projection.sources.len(), 1, "{projection:#?}");
    assert_searchable_and_showable(&temp, "codex", "setup should import");
}

#[test]
fn mixed_setup_publishes_each_provider_once() {
    let temp = tempdir();
    write_codex_setup_session(&temp);
    let claude_query = "mixed setup claude source authority";
    install_default_claude_fixture(&temp, claude_query);
    let _daemon = start_full_source_refresh_daemon(&temp);

    let setup = ready_setup(&temp);
    assert_eq!(setup["schema_version"], 2, "{setup:#}");
    assert_eq!(setup["mode"], "ready", "{setup:#}");
    assert_eq!(setup["refresh_request"]["status"], "published", "{setup:#}");
    assert_eq!(setup["refresh_request"]["source_count"], 2, "{setup:#}");
    let generation = setup["lexical"]["generation_id"].as_str().unwrap();
    assert_eq!(
        manifest_providers(&temp, generation),
        vec!["claude".to_owned(), "codex".to_owned()]
    );
    let status = wait_for_relational_projection(&temp, generation);
    assert_eq!(status["relational"]["source_count"], 2, "{status:#}");

    let codex = provider_projection_snapshot(&temp, "codex");
    let claude = provider_projection_snapshot(&temp, "claude");
    assert_eq!(codex.sessions.len(), 1, "{codex:#?}");
    assert_eq!(claude.sessions.len(), 1, "{claude:#?}");
    assert_eq!(codex.sources.len(), 1, "{codex:#?}");
    assert_eq!(claude.sources.len(), 1, "{claude:#?}");
    assert_eq!(codex.generation, claude.generation);
    assert_eq!(codex.generation, generation);

    assert_searchable_and_showable(&temp, "codex", "setup should import");
    assert_searchable_and_showable(&temp, "claude", claude_query);
}

#[test]
fn setup_adds_provider_without_changing_unchanged_source_ids() {
    let temp = tempdir();
    let pi_query = "pi authority retained across provider addition";
    install_default_pi_fixture(&temp, pi_query);
    let _daemon = start_full_source_refresh_daemon(&temp);
    ready_setup(&temp);
    let pi_before = provider_projection_snapshot(&temp, "pi");
    let pi_ids_before = assert_searchable_and_showable(&temp, "pi", pi_query);

    write_codex_setup_session(&temp);
    let setup = ready_setup(&temp);
    let generation = setup["lexical"]["generation_id"].as_str().unwrap();
    assert_eq!(
        manifest_providers(&temp, generation),
        vec!["codex".to_owned(), "pi".to_owned()]
    );
    let pi_after = provider_projection_snapshot(&temp, "pi");
    assert_ne!(pi_after.generation, pi_before.generation);
    assert_eq!(pi_after.sessions, pi_before.sessions);
    assert_eq!(pi_after.events, pi_before.events);
    assert_eq!(pi_after.sources, pi_before.sources);
    assert_eq!(
        assert_searchable_and_showable(&temp, "pi", pi_query),
        pi_ids_before
    );
    assert_searchable_and_showable(&temp, "codex", "setup should import");
}

#[test]
fn repeated_setup_and_import_preserve_generation_and_source_backed_ids() {
    let temp = tempdir();
    write_codex_setup_session(&temp);
    let _daemon = start_full_source_refresh_daemon(&temp);

    let first = ready_setup(&temp);
    let first_generation = first["lexical"]["generation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let projection = provider_projection_snapshot(&temp, "codex");
    let ids = assert_searchable_and_showable(&temp, "codex", "setup should import");

    let second = ready_setup(&temp);
    assert_eq!(
        second["lexical"]["generation_id"], first_generation,
        "{second:#}"
    );
    assert_eq!(
        second["refresh_request"]["published_generation"], first_generation,
        "{second:#}"
    );
    assert_eq!(provider_projection_snapshot(&temp, "codex"), projection);

    let imported = json_output(ctx(&temp).args([
        "import",
        "--all",
        "--no-daemon",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(imported["sources"].as_array().unwrap().len(), 1);
    assert_eq!(
        imported["sources"][0]["source_format"],
        "provider_authoritative_all"
    );
    assert_eq!(
        imported["sources"][0]["published_generation"],
        first_generation
    );
    assert_eq!(provider_projection_snapshot(&temp, "codex"), projection);
    assert_eq!(
        assert_searchable_and_showable(&temp, "codex", "setup should import"),
        ids
    );
}
