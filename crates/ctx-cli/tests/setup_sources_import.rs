mod support;

use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Barrier,
    },
    thread,
};
use support::*;

fn write_codex_setup_session(temp: &TempDir) {
    let sessions = temp
        .path()
        .join(".codex")
        .join("sessions")
        .join("2026/06/24");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("rollout-2026-06-24T10-00-00-codex-session-setup.jsonl"),
        concat!(
            r#"{"timestamp":"2026-06-24T10:00:00.000Z","type":"session_meta","payload":{"id":"codex-session-setup","timestamp":"2026-06-24T10:00:00.000Z","cwd":"/repo/app","originator":"codex-cli","cli_version":"0.200.0","source":"cli","model_provider":"openai"}}"#,
            "\n",
            r#"{"timestamp":"2026-06-24T10:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"setup should import"}]}}"#,
            "\n"
        ),
    )
    .unwrap();
}

#[test]
fn setup_does_not_migrate_legacy_shim_directory() {
    let temp = tempdir();
    let legacy_shims = temp.path().join("legacy-history").join("shims");
    fs::create_dir_all(&legacy_shims).unwrap();
    fs::write(legacy_shims.join("git"), "#!/bin/sh\n").unwrap();

    ctx(&temp).arg("setup").assert().success();

    assert!(
        !temp.path().join("shims").exists(),
        "setup must not create or migrate shim directories"
    );
    assert!(
        legacy_shims.join("git").exists(),
        "legacy shim files should be left in place instead of installed"
    );
}

#[test]
fn setup_does_not_write_default_config_and_preserves_existing_config() {
    let temp = tempdir();
    let config_path = temp.path().join("config.toml");

    ctx(&temp).arg("setup").assert().success();
    assert!(
        !config_path.exists(),
        "setup must not write implicit default values to config.toml"
    );

    let user_config = "# user managed ctx config\n[analytics]\nenabled = false\n";
    fs::write(&config_path, user_config).unwrap();

    ctx(&temp).arg("setup").assert().success();
    assert_eq!(
        fs::read_to_string(&config_path).unwrap(),
        user_config,
        "setup must not overwrite an existing user config"
    );
}

#[test]
fn status_reads_committed_wal_content_from_an_active_store() {
    let temp = tempdir();
    write_codex_setup_session(&temp);
    ctx(&temp)
        .args(["setup", "--wait", "--progress", "none"])
        .assert()
        .success();

    let db_path = temp.path().join("work.sqlite");
    let writer = Connection::open(&db_path).unwrap();
    writer
        .execute_batch("PRAGMA journal_mode = WAL; PRAGMA wal_autocheckpoint = 0;")
        .unwrap();
    writer
        .execute(
            r#"
            INSERT INTO sessions
            (id, provider, external_session_id, agent_type, is_primary, status, fidelity,
             started_at_ms, created_at_ms, updated_at_ms)
            VALUES
            ('00000000-0000-0000-0000-000000000001', 'codex', 'wal-only-session',
             'primary', 1, 'imported', 'imported', 1, 1, 1)
            "#,
            [],
        )
        .unwrap();
    assert!(temp.path().join("work.sqlite-wal").exists());

    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(status["indexed_sessions"], 2, "{status:#}");
    drop(writer);
}

#[test]
fn malformed_present_config_fails_before_setup_and_analytics_side_effects() {
    let temp = tempdir();
    let state = temp.path().join("state");
    let events_path = temp.path().join("analytics.jsonl");
    fs::write(
        temp.path().join("config.toml"),
        "[analytics]\nenabled = flase\n",
    )
    .unwrap();

    ctx(&temp)
        .arg("setup")
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_OFF")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("analytics.enabled").and(predicate::str::contains("boolean")),
        );

    assert!(
        !temp.path().join("work.sqlite").exists(),
        "setup must not create the store after config load fails"
    );
    assert!(
        !events_path.exists(),
        "analytics endpoint should not be touched after config load fails"
    );
    assert!(
        !temp.path().join("install.json").exists(),
        "analytics install identity should not be created after config load fails"
    );
    assert!(
        !expected_device_path(temp.path(), &state).exists(),
        "analytics device identity should not be created after config load fails"
    );
}

#[test]
fn status_missing_store_is_read_only_and_does_not_initialize_files() {
    let temp = tempdir();
    let data_root = temp.path().join("ctx-data");

    let status = json_output(
        ctx(&temp)
            .args(["status", "--json"])
            .env("CTX_DATA_ROOT", &data_root),
    );
    assert_eq!(status["schema_version"], 1);
    assert_eq!(status["initialized"], false);
    assert_eq!(status["local_only"], true);
    assert_eq!(status["read_only"], true);
    assert_eq!(status["indexed_items"], 0);
    assert_eq!(status["indexed_sources"], 0);
    assert_eq!(status["cataloged_sessions"], 0);

    let output = ctx(&temp)
        .arg("status")
        .env("CTX_DATA_ROOT", &data_root)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("initialized: false"), "{output}");
    assert!(output.contains("local_only: true"), "{output}");
    assert!(output.contains("read_only: true"), "{output}");

    assert!(
        !data_root.exists(),
        "status must not create the missing data root"
    );
    assert!(!data_root.join("work.sqlite").exists());
    assert!(!data_root.join("config.toml").exists());
    assert!(!data_root.join("objects").exists());
    assert!(!data_root.join("spool").exists());
}

#[test]
fn status_existing_wal_mode_store_does_not_create_sqlite_sidecars() {
    let temp = tempdir();
    ctx(&temp).args(["setup", "--no-daemon"]).assert().success();
    let db_path = temp.path().join("work.sqlite");
    let wal_path = sqlite_sidecar_path(&db_path, "-wal");
    let shm_path = sqlite_sidecar_path(&db_path, "-shm");
    assert!(db_path.exists());
    assert!(
        !wal_path.exists(),
        "setup should close a clean checkpointed store"
    );
    assert!(
        !shm_path.exists(),
        "setup should close a clean checkpointed store"
    );

    let status = json_output(ctx(&temp).args(["status", "--json"]));

    assert_eq!(status["initialized"], true);
    assert_eq!(status["read_only"], true);
    assert!(
        !wal_path.exists(),
        "status must not create a SQLite WAL sidecar"
    );
    assert!(
        !shm_path.exists(),
        "status must not create a SQLite SHM sidecar"
    );
}

fn sqlite_sidecar_path(db_path: &Path, suffix: &str) -> PathBuf {
    let mut path = db_path.as_os_str().to_os_string();
    path.push(suffix);
    PathBuf::from(path)
}

#[test]
fn status_rejects_unsupported_schema_without_migrating_or_creating_side_dirs() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let conn = Connection::open(&db_path).unwrap();
    conn.pragma_update(None, "user_version", 1).unwrap();
    drop(conn);

    let stderr = failure_stderr(ctx(&temp).args(["status", "--json"]));
    assert!(stderr.contains("schema version 1"), "{stderr}");
    assert!(stderr.contains("writable command"), "{stderr}");
    assert!(stderr.contains("ctx status"), "{stderr}");

    let conn = Connection::open(&db_path).unwrap();
    let user_version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(user_version, 1);
    assert!(!temp.path().join("config.toml").exists());
    assert!(!temp.path().join("objects").exists());
    assert!(!temp.path().join("spool").exists());
}

#[test]
fn status_does_not_repair_empty_search_projection() {
    let temp = tempdir();
    let fixture = custom_history_fixture("basic.jsonl");

    let imported = json_output(ctx(&temp).args([
        "import",
        "--format",
        "ctx-history-jsonl-v1",
        "--path",
        &fixture,
        "--json",
        "--progress",
        "none",
    ]));
    assert!(imported["totals"]["imported_events"].as_u64().unwrap() > 0);

    let db_path = temp.path().join("work.sqlite");
    let conn = Connection::open(&db_path).unwrap();
    assert!(
        sqlite_count(&conn, "SELECT COUNT(*) FROM event_search") > 0,
        "fixture import should create searchable event projections"
    );
    conn.execute_batch(
        "DELETE FROM ctx_history_search;\
         DELETE FROM event_search;\
         DELETE FROM artifact_search;",
    )
    .unwrap();
    drop(conn);

    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(status["initialized"], true);
    assert_eq!(status["read_only"], true);
    assert!(status["indexed_items"].as_u64().unwrap() > 0);

    let conn = Connection::open(&db_path).unwrap();
    assert_eq!(
        sqlite_count(&conn, "SELECT COUNT(*) FROM ctx_history_search"),
        0
    );
    assert_eq!(sqlite_count(&conn, "SELECT COUNT(*) FROM event_search"), 0);
    assert_eq!(
        sqlite_count(&conn, "SELECT COUNT(*) FROM artifact_search"),
        0
    );
}

#[test]
fn setup_catalog_only_catalogs_codex_sessions_without_import() {
    let temp = tempdir();
    let sessions = temp
        .path()
        .join(".codex")
        .join("sessions")
        .join("2026/06/24");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("rollout-2026-06-24T10-00-00-codex-session-setup.jsonl"),
        r#"{"timestamp":"2026-06-24T10:00:00.000Z","type":"session_meta","payload":{"id":"codex-session-setup","timestamp":"2026-06-24T10:00:00.000Z","cwd":"/repo/app","originator":"codex-cli","cli_version":"0.200.0","source":"cli","model_provider":"openai"}}"#,
    )
    .unwrap();

    let setup = json_output(ctx(&temp).args(["setup", "--catalog-only", "--json"]));
    assert_eq!(setup["inventory"]["sources"], 1);
    assert_eq!(setup["inventory"]["units"], 1);
    assert_eq!(setup["inventory"]["codex_catalog_sessions"], 1);
    assert_eq!(setup["catalog"]["cataloged_sessions"], 1);
    assert_eq!(setup["catalog"]["source_files"], 1);
    assert_eq!(setup["catalog"]["failed_sessions"], 0);
    assert_eq!(setup["import"]["ran"], false);

    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(status["inventory_units"], 1);
    assert_eq!(status["pending_inventory_units"], 1);
    assert_eq!(status["cataloged_sessions"], 1);
    assert_eq!(status["indexed_catalog_sessions"], 0);
    assert_eq!(status["indexed_items"], 0);
    assert_eq!(status["read_only"], true);

    let human_setup = ctx(&temp)
        .args(["setup", "--catalog-only", "--progress", "none"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let human_setup = String::from_utf8(human_setup).unwrap();
    assert!(human_setup.contains("ctx local history inventory is ready; import is still pending"));
    assert!(human_setup.contains("  ctx import --all"));
    assert!(!human_setup.contains("ctx search \"test failure\""));
}

#[test]
fn setup_catalog_only_reports_pending_non_codex_inventory() {
    let temp = tempdir();
    install_default_claude_fixture(&temp, "catalog-only claude inventory");

    let setup = json_output(ctx(&temp).args(["setup", "--catalog-only", "--json"]));
    assert_eq!(setup["inventory"]["sources"], 1);
    assert_eq!(setup["inventory"]["source_import_files"], 1);
    assert_eq!(setup["inventory"]["pending_source_import_files"], 1);
    assert_eq!(setup["catalog"]["cataloged_sessions"], 0);
    assert_eq!(setup["import"]["ran"], false);

    let human_setup = ctx(&temp)
        .args(["setup", "--catalog-only", "--progress", "none"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let human_setup = String::from_utf8(human_setup).unwrap();
    assert!(human_setup.contains("ctx local history inventory is ready; import is still pending"));
    assert!(human_setup.contains("  ctx import --all"));
}

#[test]
fn quiet_setup_suppresses_success_output_but_not_json() {
    let temp = tempdir();
    ctx(&temp)
        .args(["--quiet", "setup", "--catalog-only"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());

    let temp = tempdir();
    ctx(&temp)
        .args(["setup", "--quiet", "--catalog-only", "--progress", "none"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());

    let temp = tempdir();
    ctx(&temp)
        .args(["setup", "--catalog-only", "--progress", "none"])
        .env("CTX_QUIET", "1")
        .assert()
        .success()
        .stdout(predicate::str::is_empty());

    let temp = tempdir();
    let setup = json_output(ctx(&temp).args([
        "--quiet",
        "setup",
        "--catalog-only",
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(setup["schema_version"], 1);
    assert_eq!(setup["mode"], "catalog_only");
}

#[test]
fn quiet_status_suppresses_success_output_but_not_json() {
    let temp = tempdir();
    ctx(&temp)
        .args(["--quiet", "status"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());

    ctx(&temp)
        .args(["status", "--quiet"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());

    ctx(&temp)
        .arg("status")
        .env("CTX_QUIET", "1")
        .assert()
        .success()
        .stdout(predicate::str::is_empty());

    ctx(&temp)
        .arg("status")
        .env("CTX_QUIET", "0")
        .assert()
        .success()
        .stdout(predicate::str::contains("initialized: false"));

    let status = json_output(ctx(&temp).args(["--quiet", "status", "--json"]));
    assert_eq!(status["schema_version"], 1);
    assert_eq!(status["initialized"], false);
}

#[test]
fn setup_backgrounds_discovered_codex_sessions_when_daemon_is_enabled_and_wait_imports() {
    let temp = tempdir();
    write_codex_setup_session(&temp);
    fs::write(
        temp.path().join("config.toml"),
        "[daemon]\nenabled = true\n",
    )
    .unwrap();

    let setup = json_output(ctx(&temp).args(["setup", "--json", "--progress", "none"]));
    assert_eq!(setup["mode"], "background");
    assert_eq!(setup["inventory"]["sources"], 1);
    assert_eq!(setup["inventory"]["units"], 1);
    assert_eq!(setup["inventory"]["codex_catalog_sessions"], 1);
    assert_eq!(setup["catalog"]["cataloged_sessions"], 1);
    assert_eq!(setup["import"]["ran"], false);
    assert_eq!(setup["import"]["reason"], "background");
    assert_eq!(setup["background_indexing"]["enabled"], true);
    assert_eq!(setup["background_indexing"]["units"], 1);
    assert_eq!(
        setup["background_indexing"]["daemon_autostart"]["status"],
        "skipped"
    );
    assert_eq!(
        setup["background_indexing"]["daemon_autostart"]["reason"],
        "json_output"
    );

    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(status["inventory_units"], 1);
    assert_eq!(status["pending_inventory_units"], 1);
    assert_eq!(status["cataloged_sessions"], 1);
    assert_eq!(status["indexed_catalog_sessions"], 0);
    assert_eq!(status["pending_catalog_sessions"], 1);
    assert_eq!(status["daemon"]["status"], "unknown");
    assert!(status["daemon"]["reason"].is_null());
    assert!(status["daemon"]["start_mode"].is_null());
    assert!(status["daemon"]["trigger_command"].is_null());

    let ready = json_output(ctx(&temp).args(["setup", "--wait", "--json", "--progress", "none"]));
    assert_eq!(ready["mode"], "ready");
    assert_eq!(ready["inventory"]["sources"], 1);
    assert_eq!(ready["inventory"]["units"], 1);
    assert_eq!(ready["inventory"]["codex_catalog_sessions"], 1);
    assert_eq!(ready["catalog"]["cataloged_sessions"], 1);
    assert_eq!(ready["import"]["ran"], true);
    assert_eq!(ready["import"]["totals"]["durable_progress"], true);
    assert_eq!(ready["import"]["totals"]["fresh_units_pending_exact"], true);
    assert_eq!(
        ready["import"]["totals"]["recovery_units_pending_exact"],
        true
    );
    assert_eq!(ready["import"]["totals"]["failed_sources"], 0);
    assert_eq!(ready["import"]["totals"]["imported_sessions"], 1);
    assert!(
        ready["import"]["totals"]["imported_events"]
            .as_u64()
            .unwrap()
            >= 1
    );

    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(status["inventory_units"], 1);
    assert_eq!(status["pending_inventory_units"], 0);
    assert_eq!(status["cataloged_sessions"], 1);
    assert_eq!(status["indexed_catalog_sessions"], 1);
    assert_eq!(status["pending_catalog_sessions"], 0);
    assert!(status["indexed_items"].as_u64().unwrap() > 0);
    assert_eq!(status["read_only"], true);

    let human_temp = tempdir();
    write_codex_setup_session(&human_temp);
    let human_setup = ctx(&human_temp)
        .args(["setup", "--wait", "--progress", "none"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let human_setup = String::from_utf8(human_setup).unwrap();
    assert!(human_setup.contains("ctx local agent history search is ready"));
    assert!(human_setup.contains("from 1 source."));
    assert!(human_setup.contains("  ctx search \"test failure\""));
}

#[test]
fn setup_json_reports_partial_when_foreground_import_leaves_incomplete_history() {
    let temp = tempdir();
    install_default_pi_fixture(&temp, "pi baseline setup content");

    let baseline =
        json_output(ctx(&temp).args(["setup", "--wait", "--json", "--progress", "none"]));
    assert_eq!(baseline["mode"], "ready", "{baseline:#}");

    let source_root = temp.path().join(".pi/agent/sessions/--workspace--");
    let incomplete_path = source_root.join("2026-06-24T12-00-00-000Z_pi-default-refresh.jsonl");
    fs::OpenOptions::new()
        .append(true)
        .open(incomplete_path)
        .unwrap()
        .write_all(br#"{"type":"message","id":"partial""#)
        .unwrap();
    write_pi_session_jsonl(
        &source_root.join("2026-06-25T12-00-00-000Z_pi-complete-later.jsonl"),
        "pi-complete-later",
        "pi complete later setup content",
    );

    let setup = json_output(ctx(&temp).args(["setup", "--wait", "--json", "--progress", "none"]));
    assert_eq!(setup["mode"], "partial", "{setup:#}");
    assert_eq!(setup["import"]["ran"], true, "{setup:#}");
    assert_eq!(setup["import"]["totals"]["durable_progress"], true);
    assert_eq!(setup["import"]["totals"]["fresh_units_processed"], 1);
    assert_eq!(setup["import"]["totals"]["fresh_units_pending"], 1);
    assert_eq!(setup["import"]["totals"]["fresh_units_pending_exact"], true);
    assert_eq!(setup["import"]["totals"]["recovery_units_pending"], 0);
    assert_eq!(
        setup["import"]["totals"]["recovery_units_pending_exact"],
        true
    );

    let search = json_output(ctx(&temp).args([
        "search",
        "pi complete later setup content",
        "--provider",
        "pi",
        "--refresh",
        "off",
        "--json",
    ]));
    assert_search_provider_oracle(
        &search,
        "pi",
        "pi complete later setup content",
        1,
        "message",
    );
}

#[test]
fn setup_import_isolates_empty_codex_session_file() {
    let temp = tempdir();
    write_codex_setup_session(&temp);
    let sessions = temp
        .path()
        .join(".codex")
        .join("sessions")
        .join("2026/06/24");
    fs::write(sessions.join("rollout-empty-codex-session.jsonl"), "").unwrap();

    let setup = json_output(ctx(&temp).args(["setup", "--json", "--progress", "none"]));
    assert_eq!(setup["inventory"]["sources"], 1, "{setup:#}");
    assert_eq!(setup["inventory"]["units"], 2, "{setup:#}");
    assert_eq!(setup["catalog"]["cataloged_sessions"], 2, "{setup:#}");
    assert_eq!(
        setup["import"]["outcome"], "completed_with_rejections",
        "{setup:#}"
    );
    assert_eq!(setup["import"]["failure_scope"], "record", "{setup:#}");
    assert_eq!(
        setup["import"]["failure_type"], "record_rejection",
        "{setup:#}"
    );
    assert_eq!(setup["import"]["totals"]["failed_sources"], 0, "{setup:#}");
    assert_eq!(
        setup["import"]["totals"]["imported_sessions"], 1,
        "{setup:#}"
    );
    assert_eq!(
        setup["import"]["totals"]["rejected_records"], 1,
        "{setup:#}"
    );
    assert!(setup["import"]["sources"][0]["rejections"][0]["error"]
        .as_str()
        .unwrap()
        .contains("rollout-empty-codex-session.jsonl"));

    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(status["cataloged_sessions"], 2, "{status:#}");
    assert_eq!(status["indexed_catalog_sessions"], 1, "{status:#}");
    assert_eq!(status["failed_catalog_sessions"], 0, "{status:#}");
    assert_eq!(status["pending_catalog_sessions"], 0, "{status:#}");
    assert_eq!(
        status["terminal_rejected_catalog_sessions"], 1,
        "{status:#}"
    );
    assert!(status["indexed_items"].as_u64().unwrap() > 0);

    let search = json_output(ctx(&temp).args([
        "search",
        "setup should import",
        "--provider",
        "codex",
        "--json",
    ]));
    assert_eq!(search["freshness"]["status"], "completed", "{search:#}");
    assert_eq!(
        search["freshness"]["totals"]["rejected_records"], 0,
        "{search:#}"
    );
    assert_eq!(
        search["freshness"]["totals"]["failed_sources"], 0,
        "{search:#}"
    );
    assert_search_provider_oracle(&search, "codex", "setup should import", 1, "message");
}

#[test]
fn setup_all_failed_foreground_import_prints_json_and_exits_nonzero() {
    let temp = tempdir();
    let sessions = temp
        .path()
        .join(".codex")
        .join("sessions")
        .join("2026/06/24");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(sessions.join("rollout-empty-only.jsonl"), "").unwrap();

    let output = ctx(&temp)
        .args(["setup", "--wait", "--json", "--progress", "none"])
        .assert()
        .failure()
        .get_output()
        .clone();
    let setup: Value = serde_json::from_slice(&output.stdout).unwrap();

    assert_eq!(setup["schema_version"], 1, "{setup:#}");
    assert_eq!(setup["import"]["ran"], true, "{setup:#}");
    assert_eq!(setup["import"]["outcome"], "failure", "{setup:#}");
    assert_eq!(setup["import"]["failure_scope"], "source", "{setup:#}");
    assert_eq!(
        setup["import"]["totals"]["imported_sources"], 0,
        "{setup:#}"
    );
    assert_eq!(setup["import"]["totals"]["failed_sources"], 1, "{setup:#}");
}

#[test]
fn setup_autostart_records_spawn_failure_status() {
    let temp = tempdir();
    write_codex_setup_session(&temp);
    fs::write(
        temp.path().join("config.toml"),
        "[daemon]\nenabled = true\n",
    )
    .unwrap();
    let missing_exe = temp.path().join("missing-ctx-binary");

    ctx(&temp)
        .args(["setup", "--progress", "none"])
        .env("CTX_DAEMON_AUTOSTART_EXE", &missing_exe)
        .env_remove("CI")
        .env_remove("CTX_DAEMON_AUTOSTART_OFF")
        .assert()
        .success();

    let status = json_output(ctx(&temp).args(["daemon", "status", "--json"]));
    assert_eq!(status["daemon"]["status"], "failed");
    assert_eq!(status["daemon"]["reason"], "spawn_failed");
    assert_eq!(status["daemon"]["start_mode"], "auto");
    assert_eq!(status["daemon"]["trigger_command"], "setup");
    assert!(status["daemon"]["last_error"]
        .as_str()
        .is_some_and(|error| !error.is_empty()));
}

#[test]
fn setup_inventories_and_imports_claude_sources_by_default() {
    let temp = tempdir();
    let project = temp.path().join(".claude").join("projects").join("-repo");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join("claude-session-setup.jsonl"),
        concat!(
            r#"{"sessionId":"claude-session-setup","timestamp":"2026-06-24T10:00:00Z","cwd":"/repo","version":"test","type":"user","message":{"role":"user","content":[{"type":"text","text":"setup should import claude"}]},"uuid":"claude-setup-1"}"#,
            "\n",
            r#"{"sessionId":"claude-session-setup","timestamp":"2026-06-24T10:00:01Z","cwd":"/repo","version":"test","type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"imported"}]},"uuid":"claude-setup-2"}"#,
            "\n"
        ),
    )
    .unwrap();

    let setup = json_output(ctx(&temp).args(["setup", "--wait", "--json", "--progress", "none"]));
    assert_eq!(setup["inventory"]["sources"], 1);
    assert_eq!(setup["inventory"]["units"], 1);
    assert_eq!(setup["inventory"]["source_import_files"], 1);
    assert_eq!(setup["inventory"]["indexed_source_import_files"], 1);
    assert_eq!(setup["inventory"]["pending_source_import_files"], 0);
    assert_eq!(setup["catalog"]["cataloged_sessions"], 0);
    assert_eq!(setup["import"]["outcome"], "success");
    assert_eq!(setup["import"]["failure_scope"], "none");
    assert_eq!(setup["import"]["failure_type"], "none");
    assert_eq!(setup["import"]["totals"]["imported_sources"], 1);
    assert_eq!(setup["import"]["totals"]["imported_sessions"], 1);
    assert_eq!(setup["import"]["totals"]["failed_sources"], 0);

    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(status["inventory_units"], 1);
    assert_eq!(status["source_import_files"], 1);
    assert_eq!(status["indexed_source_import_files"], 1);
    assert_eq!(status["pending_inventory_units"], 0);
    assert_eq!(status["indexed_catalog_sessions"], 0);
    assert!(status["indexed_items"].as_u64().unwrap() > 0);
}

#[test]
fn setup_inventories_whole_source_sqlite_providers() {
    let temp = tempdir();
    install_default_hermes_fixture(&temp, "setup should inventory hermes");

    let setup = json_output(ctx(&temp).args(["setup", "--wait", "--json", "--progress", "none"]));
    assert_eq!(setup["inventory"]["sources"], 1);
    assert_eq!(setup["inventory"]["units"], 1);
    assert_eq!(setup["inventory"]["source_import_files"], 1);
    assert_eq!(setup["inventory"]["indexed_source_import_files"], 1);
    assert_eq!(setup["inventory"]["pending_source_import_files"], 0);
    assert_eq!(setup["catalog"]["cataloged_sessions"], 0);
    assert_eq!(setup["import"]["totals"]["imported_sources"], 1);
    assert_eq!(setup["import"]["totals"]["failed_sources"], 0);

    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(status["inventory_units"], 1);
    assert_eq!(status["source_import_files"], 1);
    assert_eq!(status["indexed_source_import_files"], 1);
    assert_eq!(status["pending_inventory_units"], 0);
}

#[test]
fn clean_multisource_setup_with_hermes_bounds_wal_through_final_optimization() {
    let temp = tempdir();
    write_large_codex_setup_sessions(&temp, 40, 4, 4 * 1024);
    write_large_hermes_setup_db(&temp, 130, 8 * 1024);
    let db_path = temp.path().join("work.sqlite");
    let wal_path = temp.path().join("work.sqlite-wal");

    let running = Arc::new(AtomicBool::new(true));
    let peak_wal_bytes = Arc::new(AtomicU64::new(0));
    let sampler_ready = Arc::new(Barrier::new(2));
    let sampler = {
        let running = Arc::clone(&running);
        let peak_wal_bytes = Arc::clone(&peak_wal_bytes);
        let sampler_ready = Arc::clone(&sampler_ready);
        thread::spawn(move || {
            sampler_ready.wait();
            loop {
                if let Ok(metadata) = fs::metadata(&wal_path) {
                    peak_wal_bytes.fetch_max(metadata.len(), Ordering::AcqRel);
                }
                if !running.load(Ordering::Acquire) {
                    break;
                }
                thread::sleep(Duration::from_millis(1));
            }
        })
    };
    sampler_ready.wait();
    let mut setup_command = ctx(&temp);
    setup_command.args(["setup", "--wait", "--json", "--progress", "none"]);
    let setup_output = setup_command.output().unwrap();
    running.store(false, Ordering::Release);
    sampler.join().unwrap();

    assert!(
        setup_output.status.success(),
        "setup failed: {}",
        String::from_utf8_lossy(&setup_output.stderr)
    );
    let setup: Value = serde_json::from_slice(&setup_output.stdout).unwrap();
    assert_eq!(setup["import"]["totals"]["failed_sources"], 0);
    assert!(
        peak_wal_bytes.load(Ordering::Acquire) <= 32 * 1024 * 1024,
        "clean multi-source setup grew WAL to {} bytes",
        peak_wal_bytes.load(Ordering::Acquire)
    );
    assert!(
        fs::metadata(temp.path().join("work.sqlite-wal"))
            .map(|metadata| metadata.len())
            .unwrap_or(0)
            <= 4 * 1024 * 1024,
        "setup left a large final WAL"
    );

    let conn = Connection::open(&db_path).unwrap();
    assert_eq!(
        conn.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert_eq!(
        sqlite_count(
            &conn,
            "SELECT COUNT(*) FROM search_projection_stats WHERE key LIKE 'event_search_bulk_mode_v1%'"
        ),
        0
    );
    assert!(
        sqlite_count(
            &conn,
            "SELECT COUNT(*) FROM event_search WHERE event_search MATCH 'codex AND setup AND history'"
        ) > 0
    );
    assert!(
        sqlite_count(
            &conn,
            "SELECT COUNT(*) FROM event_search WHERE event_search MATCH 'hermes AND setup AND current'"
        ) > 0
    );
    let event_count = sqlite_count(&conn, "SELECT COUNT(*) FROM events");
    drop(conn);

    let replay = json_output(ctx(&temp).args(["setup", "--wait", "--json", "--progress", "none"]));
    assert_eq!(replay["import"]["totals"]["failed_sources"], 0);
    let conn = Connection::open(&db_path).unwrap();
    assert_eq!(
        sqlite_count(&conn, "SELECT COUNT(*) FROM events"),
        event_count
    );
}

fn write_large_codex_setup_sessions(
    temp: &TempDir,
    sessions: usize,
    messages_per_session: usize,
    payload_bytes: usize,
) {
    let sessions_dir = temp.path().join(".codex/sessions/2026/07/12");
    fs::create_dir_all(&sessions_dir).unwrap();
    let payload = "database migration checkpoint bounded wal search index ".repeat(
        payload_bytes / "database migration checkpoint bounded wal search index ".len() + 1,
    );
    for session_index in 0..sessions {
        let session_id = format!("codex-setup-history-{session_index}");
        let path = sessions_dir.join(format!("rollout-{session_id}.jsonl"));
        let mut file = fs::File::create(path).unwrap();
        writeln!(
            file,
            "{}",
            json!({
                "timestamp": "2026-07-12T10:00:00.000Z",
                "type": "session_meta",
                "payload": {
                    "id": session_id,
                    "timestamp": "2026-07-12T10:00:00.000Z",
                    "cwd": "/repo/setup",
                    "originator": "codex-cli",
                    "cli_version": "0.200.0",
                    "source": "cli",
                    "model_provider": "openai"
                }
            })
        )
        .unwrap();
        for message_index in 0..messages_per_session {
            writeln!(
                file,
                "{}",
                json!({
                    "timestamp": "2026-07-12T10:00:01.000Z",
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "role": "user",
                        "content": [{
                            "type": "input_text",
                            "text": format!(
                                "codex-setup-history session {session_index} message {message_index} {payload}"
                            )
                        }]
                    }
                })
            )
            .unwrap();
        }
    }
}

fn write_large_hermes_setup_db(temp: &TempDir, messages: usize, payload_bytes: usize) {
    let hermes_dir = temp.path().join(".hermes");
    fs::create_dir_all(&hermes_dir).unwrap();
    let mut conn = Connection::open(hermes_dir.join("state.db")).unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (
            id TEXT PRIMARY KEY,
            source TEXT NOT NULL,
            started_at REAL NOT NULL
        );
        CREATE TABLE messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            role TEXT NOT NULL,
            content TEXT,
            timestamp REAL NOT NULL,
            active INTEGER NOT NULL DEFAULT 1,
            compacted INTEGER NOT NULL DEFAULT 0
        );
        INSERT INTO sessions VALUES ('hermes-setup-current', 'acp', 1782259200.0);",
    )
    .unwrap();
    let payload = "provider import fts merge recovery bounded checkpoint "
        .repeat(payload_bytes / "provider import fts merge recovery bounded checkpoint ".len() + 1);
    let transaction = conn.transaction().unwrap();
    for index in 0..messages {
        transaction
            .execute(
                "INSERT INTO messages (session_id, role, content, timestamp)
                 VALUES ('hermes-setup-current', ?1, ?2, ?3)",
                params![
                    if index % 2 == 0 { "user" } else { "assistant" },
                    format!("hermes-setup-current message {index} {payload}"),
                    1782259201.0 + index as f64,
                ],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
}

include!("setup_sources_import/source_and_import_tests.rs");
