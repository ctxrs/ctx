use super::{
    assert_daemon_process_running, assert_no_daemon_autostart_mutation, support::*,
    wait_for_daemon_status, write_active_daemon_upgrade_handoff, write_codex_setup_session,
};

use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Barrier,
    },
    thread,
};

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
fn setup_without_semantic_flag_preserves_explicit_search_setting() {
    for enabled in [false, true] {
        let temp = tempdir();
        let config_path = temp.path().join("config.toml");
        let original = format!("[search]\nsemantic = {enabled}\n");
        fs::write(&config_path, &original).unwrap();

        let setup = json_output(ctx(&temp).args(["setup", "--json", "--progress", "none"]));
        assert_eq!(
            setup["background_indexing"]["semantic_enabled"], enabled,
            "{setup:#}"
        );
        assert_eq!(fs::read_to_string(config_path).unwrap(), original);
        assert_no_daemon_autostart_mutation(&temp);
    }
}

#[test]
fn setup_semantic_persists_opt_in_and_machine_output_does_not_autostart() {
    let temp = tempdir();
    write_codex_setup_session(&temp);

    let setup =
        json_output(ctx(&temp).args(["setup", "--semantic", "--json", "--progress", "none"]));
    assert_eq!(setup["background_indexing"]["semantic_enabled"], true);
    assert_eq!(
        setup["background_indexing"]["daemon_autostart"]["reason"],
        "machine_readable_output"
    );
    assert_no_daemon_autostart_mutation(&temp);

    let config_path = temp.path().join("config.toml");
    let once = fs::read_to_string(&config_path).unwrap();
    assert!(once.contains("[search]\nsemantic = true\n"), "{once}");
    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(status["semantic"]["enabled"], true);
    assert_eq!(status["semantic"]["config_source"], "config");

    json_output(ctx(&temp).args(["setup", "--semantic", "--json", "--progress", "none"]));
    assert_eq!(fs::read_to_string(config_path).unwrap(), once);
    assert_no_daemon_autostart_mutation(&temp);
}

#[test]
fn setup_semantic_rejects_disabled_daemon_without_mutating_config_or_store() {
    let temp = tempdir();
    let config_path = temp.path().join("config.toml");
    let original = "[daemon]\nenabled = false\n";
    fs::write(&config_path, original).unwrap();

    let stderr =
        failure_stderr(ctx(&temp).args(["setup", "--semantic", "--json", "--progress", "none"]));
    assert!(stderr.contains("requires daemon maintenance"), "{stderr}");
    assert_eq!(fs::read_to_string(config_path).unwrap(), original);
    assert!(!temp.path().join("work.sqlite").exists());
    assert_no_daemon_autostart_mutation(&temp);

    let explicit_opt_out = tempdir();
    let stderr = failure_stderr(ctx(&explicit_opt_out).args([
        "setup",
        "--semantic",
        "--no-daemon",
        "--json",
        "--progress",
        "none",
    ]));
    assert!(stderr.contains("requires daemon maintenance"), "{stderr}");
    assert!(!explicit_opt_out.path().join("config.toml").exists());
    assert!(!explicit_opt_out.path().join("work.sqlite").exists());
    assert_no_daemon_autostart_mutation(&explicit_opt_out);
}

#[test]
fn setup_semantic_clean_cache_queues_daemon_without_foreground_download() {
    let temp = tempdir();
    write_codex_setup_session(&temp);
    let semantic_cache = temp.path().join("clean-semantic-cache");

    let setup = json_output(
        ctx(&temp)
            .args(["setup", "--json", "--progress", "none"])
            .env("CTX_SEARCH_SEMANTIC", "true")
            .env("CTX_SEMANTIC_CACHE_DIR", &semantic_cache),
    );

    assert_eq!(setup["background_indexing"]["semantic_enabled"], true);
    assert_eq!(setup["background_indexing"]["semantic_supported"], true);
    assert_eq!(setup["network_required"], false);
    assert!(
        !semantic_cache.exists(),
        "foreground setup must leave clean semantic cache acquisition to the daemon"
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
        .create_scalar_function(
            "ctx_projection_writer_authorized_v1",
            0,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8
                | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC
                | rusqlite::functions::FunctionFlags::SQLITE_INNOCUOUS,
            |_| Ok(1_i64),
        )
        .unwrap();
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
        .env_remove("CTX_ANALYTICS_ENABLED")
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
fn status_existing_wal_mode_store_does_not_mutate_canonical_database() {
    let temp = tempdir();
    ctx(&temp).args(["setup", "--no-daemon"]).assert().success();
    let db_path = temp.path().join("work.sqlite");
    assert!(db_path.exists());
    let canonical_before = fs::read(&db_path).unwrap();

    let status = json_output(ctx(&temp).args(["status", "--json"]));

    assert_eq!(status["initialized"], true);
    assert_eq!(status["read_only"], true);
    assert_eq!(
        fs::read(&db_path).unwrap(),
        canonical_before,
        "status must not mutate canonical database pages"
    );
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
    assert_eq!(
        setup["background_indexing"]["daemon_autostart"]["status"],
        "not_needed"
    );
    assert_eq!(
        setup["background_indexing"]["daemon_autostart"]["reason"],
        "catalog_only"
    );

    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(status["inventory_units"], 1);
    assert_eq!(status["pending_inventory_units"], 1);
    assert_eq!(status["cataloged_sessions"], 1);
    assert_eq!(status["indexed_catalog_sessions"], 0);
    assert_eq!(
        status["inventory_source_bytes"],
        setup["background_indexing"]["source_bytes"]
    );
    let source_bytes = setup["background_indexing"]["source_bytes"]
        .as_u64()
        .unwrap();
    assert_eq!(
        status["lexical_index_estimate_seconds"],
        source_bytes.div_ceil(16 * 1024 * 1024).max(1)
    );
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
    assert!(human_setup.contains("Catalog-only setup does not autostart daemon maintenance."));
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
    assert!(status["inventory_source_bytes"].is_null());
    assert!(status["lexical_index_estimate_seconds"].is_null());
}

#[test]
fn setup_backgrounds_discovered_codex_sessions_by_default_and_wait_imports() {
    let temp = tempdir();
    write_codex_setup_session(&temp);

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
        "not_needed"
    );
    assert_eq!(
        setup["background_indexing"]["daemon_autostart"]["reason"],
        "machine_readable_output"
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
    assert_eq!(ready["import"]["totals"]["failed_sources"], 0);
    assert_eq!(ready["import"]["totals"]["imported_sessions"], 1);
    assert_eq!(
        ready["background_indexing"]["daemon_autostart"]["status"],
        "not_needed"
    );
    assert_eq!(
        ready["background_indexing"]["daemon_autostart"]["reason"],
        "machine_readable_output"
    );
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
    assert!(human_setup
        .contains("Daemon autostart is disabled for this process; setup ran in the foreground."));
    assert!(human_setup.contains("  ctx search \"test failure\""));
}

#[test]
fn setup_no_daemon_is_one_run_opt_out_and_keeps_semantic_disabled() {
    let temp = tempdir();
    write_codex_setup_session(&temp);

    let setup =
        json_output(ctx(&temp).args(["setup", "--no-daemon", "--json", "--progress", "none"]));
    assert_eq!(setup["mode"], "ready");
    assert_eq!(setup["import"]["ran"], true);
    assert_eq!(setup["background_indexing"]["enabled"], false);
    assert_eq!(
        setup["background_indexing"]["daemon_autostart"]["status"],
        "not_needed"
    );
    assert_eq!(
        setup["background_indexing"]["daemon_autostart"]["reason"],
        "explicit_opt_out"
    );

    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(status["daemon"]["enabled"], true);
    assert_eq!(status["semantic"]["status"], "disabled");
    assert_eq!(status["semantic"]["reason"], "semantic_disabled");
    assert!(!temp.path().join("config.toml").exists());

    let human_setup = ctx(&temp)
        .args(["setup", "--no-daemon", "--progress", "none"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let human_setup = String::from_utf8(human_setup).unwrap();
    assert!(human_setup
        .contains("Daemon autostart was skipped for this setup because --no-daemon was used."));
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

    let setup = json_output(ctx(&temp).args(["setup", "--wait", "--json", "--progress", "none"]));
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
    assert_eq!(
        setup["import"]["sources"][0]["rejections"][0],
        json!({
            "line": 0,
            "error": "Codex NativePath source has no valid session owner",
        }),
        "{setup:#}"
    );

    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(status["cataloged_sessions"], 2, "{status:#}");
    assert_eq!(status["indexed_catalog_sessions"], 1, "{status:#}");
    assert_eq!(status["failed_catalog_sessions"], 0, "{status:#}");
    assert_eq!(status["pending_catalog_sessions"], 1, "{status:#}");
    assert!(status["indexed_items"].as_u64().unwrap() > 0);

    let search = json_output(ctx(&temp).args([
        "search",
        "setup should import",
        "--provider",
        "codex",
        "--json",
    ]));
    assert_eq!(
        search["freshness"]["status"], "daemon_background",
        "{search:#}"
    );
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
    let missing_exe = temp.path().join("missing-ctx-binary");

    let output = ctx(&temp)
        .args(["--quiet", "setup", "--progress", "none"])
        .env("CTX_DAEMON_AUTOSTART_EXE", &missing_exe)
        .env_remove("CI")
        .env_remove("CTX_DAEMON_AUTOSTART_OFF")
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("ctx daemon did not start"), "{stderr}");
    assert!(stderr.contains("ctx daemon status --json"), "{stderr}");
    assert!(
        output.stdout.is_empty(),
        "failed quiet setup must not print success or queued output: {}",
        String::from_utf8_lossy(&output.stdout)
    );

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
fn machine_readable_setup_preserves_json_without_autostarting_daemon() {
    let temp = tempdir();
    let missing_exe = temp.path().join("missing-ctx-binary");
    write_active_daemon_upgrade_handoff(&temp);

    let setup = json_output(
        ctx(&temp)
            .args(["setup", "--json", "--progress", "none"])
            .env("CTX_DAEMON_AUTOSTART_EXE", &missing_exe)
            .env_remove("CI")
            .env_remove("CTX_DAEMON_AUTOSTART_OFF"),
    );
    assert_eq!(
        setup["background_indexing"]["daemon_autostart"]["status"],
        "not_needed"
    );
    assert_eq!(setup["mode"], "ready");
    assert_eq!(setup["background_indexing"]["enabled"], false);
    assert_eq!(
        setup["background_indexing"]["daemon_autostart"]["reason"],
        "machine_readable_output"
    );
    assert_no_daemon_autostart_mutation(&temp);
}

#[test]
fn progress_json_setup_does_not_autostart_or_nudge_daemon() {
    let temp = tempdir();
    write_active_daemon_upgrade_handoff(&temp);

    let output = ctx(&temp)
        .args(["setup", "--progress", "json"])
        .env_remove("CI")
        .env_remove("CTX_DAEMON_AUTOSTART_OFF")
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains(
            "Daemon autostart was skipped because machine-readable output was requested."
        ),
        "{stdout}"
    );
    assert_no_daemon_autostart_mutation(&temp);
}

#[test]
fn human_setup_starts_a_reported_daemon_process() {
    let temp = tempdir();
    let binary = copied_ctx_binary(&temp);

    let output = ctx_from_binary(&temp, &binary)
        .args(["setup", "--progress", "none"])
        .env("CTX_DAEMON_AUTOSTART_IDLE_EXIT_SECONDS", "2")
        .env("CTX_DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS", "1")
        .env("CTX_UPGRADE_AUTO", "off")
        .env_remove("CI")
        .env_remove("CTX_DAEMON_AUTOSTART_OFF")
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("background maintenance handoff is verified"),
        "{stdout}"
    );

    let running = json_output(ctx(&temp).args(["daemon", "status", "--json"]));
    assert_eq!(running["daemon"]["status"], "running", "{running:#}");
    assert_eq!(running["daemon"]["running"], true, "{running:#}");
    assert_eq!(running["daemon"]["trigger_command"], "setup", "{running:#}");
    assert_eq!(running["daemon"]["start_mode"], "auto");
    let pid = running["daemon"]["pid"].as_u64().unwrap() as u32;
    assert_daemon_process_running(pid);

    let completed = wait_for_daemon_status(&temp, "completed", false, "setup");
    assert_eq!(completed["daemon"]["pid"], pid);
    assert!(completed["daemon"]["finished_at_ms"].as_i64().unwrap() > 0);
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
    assert_eq!(
        status["inventory_source_bytes"],
        setup["background_indexing"]["source_bytes"]
    );
    let source_bytes = setup["background_indexing"]["source_bytes"]
        .as_u64()
        .unwrap();
    assert_eq!(
        status["lexical_index_estimate_seconds"],
        source_bytes.div_ceil(16 * 1024 * 1024).max(1)
    );
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
