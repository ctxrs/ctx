mod support;

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
fn setup_partial_import_isolates_empty_codex_session_file() {
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
    assert_eq!(setup["import"]["totals"]["failed_sources"], 0, "{setup:#}");
    assert_eq!(
        setup["import"]["totals"]["imported_sessions"], 1,
        "{setup:#}"
    );
    assert_eq!(setup["import"]["totals"]["failed"], 1, "{setup:#}");
    assert!(setup["import"]["sources"][0]["failures"][0]["error"]
        .as_str()
        .unwrap()
        .contains("rollout-empty-codex-session.jsonl"));

    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(status["cataloged_sessions"], 2, "{status:#}");
    assert_eq!(status["indexed_catalog_sessions"], 1, "{status:#}");
    assert_eq!(status["failed_catalog_sessions"], 1, "{status:#}");
    assert_eq!(status["pending_catalog_sessions"], 1, "{status:#}");
    assert!(status["indexed_items"].as_u64().unwrap() > 0);

    let search = json_output(ctx(&temp).args([
        "search",
        "setup should import",
        "--provider",
        "codex",
        "--json",
    ]));
    assert_eq!(search["freshness"]["status"], "completed", "{search:#}");
    assert_eq!(search["freshness"]["totals"]["failed"], 1, "{search:#}");
    assert_eq!(
        search["freshness"]["totals"]["failed_sources"], 0,
        "{search:#}"
    );
    assert_search_provider_oracle(&search, "codex", "setup should import", 1, "message");
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
fn setup_skips_empty_codex_session_tree() {
    let temp = tempdir();
    fs::create_dir_all(temp.path().join(".codex").join("sessions")).unwrap();

    let setup = json_output(ctx(&temp).args(["setup", "--wait", "--json", "--progress", "none"]));
    assert_eq!(setup["catalog"]["cataloged_sessions"], 0);
    assert_eq!(setup["catalog"]["source_files"], 0);
    assert_eq!(setup["import"]["totals"]["imported_sources"], 0);

    let sources = json_output(ctx(&temp).args(["sources", "--json"]));
    let codex_sessions = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| {
            source["provider"] == "codex" && source["source_format"] == "codex_session_jsonl_tree"
        })
        .unwrap();
    assert_eq!(codex_sessions["status"], "empty");
    assert_eq!(codex_sessions["importable"], false);
}

#[test]
fn sources_default_hides_unsupported_missing_locations() {
    let temp = tempdir();

    let sources = json_output(ctx(&temp).args(["sources", "--json"]));
    assert_eq!(sources["scope"], "default");
    assert!(sources["hidden_missing_sources"].as_u64().unwrap() > 0);
    let visible = sources["sources"].as_array().unwrap();
    assert!(visible.iter().any(|source| source["provider"] == "codex"));
    assert!(visible.iter().any(|source| source["provider"] == "claude"));
    assert!(visible.iter().any(|source| source["provider"] == "cursor"));
    assert!(visible.iter().any(|source| source["provider"] == "pi"));
    assert!(visible
        .iter()
        .any(|source| source["provider"] == "opencode"));
    assert!(visible
        .iter()
        .any(|source| source["provider"] == "copilot_cli"));

    let text = ctx(&temp)
        .arg("sources")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(text).unwrap();
    assert!(text.contains("missing provider locations hidden"));
    assert!(text.contains("ctx sources --all"));

    let all_sources = json_output(ctx(&temp).args(["sources", "--json", "--all"]));
    assert_eq!(all_sources["scope"], "all");
    assert_eq!(all_sources["hidden_missing_sources"], 0);
    let all = all_sources["sources"].as_array().unwrap();
    assert!(all.len() > visible.len());
}

#[test]
fn sources_provider_filter_rejects_unsupported_providers() {
    let temp = tempdir();

    ctx(&temp)
        .args(["sources", "--provider", "not-a-real-provider", "--json"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown provider"));
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
    assert_eq!(stdout["schema_version"], 1);
    assert!(stdout["totals"]["imported_sessions"].as_u64().unwrap() > 0);

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains(r#""type":"ctx_progress""#), "{stderr}");
    assert!(stderr.contains(r#""operation":"import""#), "{stderr}");
}

#[test]
fn import_custom_history_jsonl_format_is_searchable_and_idempotent() {
    let temp = tempdir();
    let fixture = custom_history_fixture("basic.jsonl");

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
    assert_eq!(second["totals"]["skipped"], 6);
}

#[test]
fn import_custom_history_jsonl_format_rejects_malformed_atomically() {
    let temp = tempdir();
    let fixture = custom_history_fixture("malformed-partial.jsonl");

    let stderr = failure_stderr(ctx(&temp).args([
        "import",
        "--format",
        "ctx-history-jsonl-v1",
        "--path",
        &fixture,
        "--progress",
        "none",
    ]));
    assert!(
        stderr.contains("ctx-history-jsonl-v1 import failed"),
        "{stderr}"
    );

    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(status["indexed_items"], 0);
    let conn = Connection::open(temp.path().join("work.sqlite")).unwrap();
    assert_eq!(
        sqlite_count(&conn, "SELECT COUNT(*) FROM history_records"),
        0
    );
    assert_eq!(
        sqlite_count(&conn, "SELECT COUNT(*) FROM ctx_history_search"),
        0
    );
    assert_eq!(
        sqlite_count(&conn, "SELECT COUNT(*) FROM capture_sources"),
        0
    );
    assert_eq!(sqlite_count(&conn, "SELECT COUNT(*) FROM sessions"), 0);
    assert_eq!(sqlite_count(&conn, "SELECT COUNT(*) FROM events"), 0);
}

#[test]
fn import_custom_history_jsonl_format_partial_commits_valid_rows() {
    let temp = tempdir();
    let fixture = custom_history_fixture("malformed-partial.jsonl");

    let import = json_output(ctx(&temp).args([
        "import",
        "--format",
        "ctx-history-jsonl-v1",
        "--path",
        &fixture,
        "--partial",
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(import["totals"]["imported_sessions"], 1);
    assert_eq!(import["totals"]["imported_events"], 1);
    assert_eq!(import["totals"]["failed"], 1);
    assert_eq!(import["sources"][0]["failed"], 1);

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
        "partial custom import was not searchable: {search:#}"
    );
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
    fs::copy(
        provider_history_fixture("pi-session.jsonl"),
        pi_home.join("2026-06-24T12-00-00-000Z_pi-session-docs-1.jsonl"),
    )
    .unwrap();

    let output = ctx(&temp)
        .args(["import", "--all", "--json", "--progress", "json"])
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["schema_version"], 1);
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
fn sources_lists_supported_personal_agent_provider_defaults() {
    let temp = tempdir();
    install_default_openclaw_fixture(&temp, "openclaw-sources-oracle");
    install_default_hermes_fixture(&temp, "hermes-sources-oracle");
    install_default_kilo_fixture(&temp, "kilo-sources-oracle");
    install_default_kiro_fixture(&temp, "kiro-sources-oracle");
    install_default_astrbot_fixture(&temp, "astrbot-sources-oracle");
    install_default_shelley_fixture(&temp, "shelley-sources-oracle");
    install_default_continue_fixture(&temp, "continue-sources-oracle");
    install_default_forgecode_fixture(&temp, "forgecode-sources-oracle");
    install_default_mistral_vibe_fixture(&temp, "mistral-vibe-sources-oracle");
    install_default_mux_fixture(&temp, "mux-sources-oracle");
    install_default_lingma_fixture(&temp, "lingma-sources-oracle");
    install_default_qoder_fixture(&temp, "qoder-sources-oracle");
    install_default_auggie_fixture(&temp, "auggie-sources-oracle");
    install_default_junie_fixture(&temp, "junie-sources-oracle");
    install_default_warp_fixture(&temp);
    install_default_trae_fixture(&temp, "trae-sources-oracle");

    let sources = json_output(ctx(&temp).args(["sources", "--json"]));
    for (provider, source_format, import_support, native_import) in [
        ("openclaw", "openclaw_session_jsonl_tree", "native", true),
        ("hermes", "hermes_state_sqlite", "native", true),
        ("kilo", "kilo_sqlite", "native", true),
        ("kiro_cli", "kiro_cli_sqlite", "native", true),
        ("astrbot", "astrbot_data_v4_sqlite", "native", true),
        ("shelley", "shelley_sqlite", "native", true),
        ("continue", "continue_cli_sessions_json", "native", true),
        ("forgecode", "forgecode_sqlite", "native", true),
        (
            "mistral_vibe",
            "mistral_vibe_session_jsonl_tree",
            "native",
            true,
        ),
        ("mux", "mux_session_jsonl_tree", "native", true),
        ("lingma", "lingma_sqlite", "native", true),
        ("qoder", "qoder_transcript_jsonl_tree", "native", true),
        ("auggie", "auggie_session_json", "native", true),
        ("junie", "junie_session_events_jsonl_tree", "native", true),
        ("warp", "warp_sqlite", "native", true),
        ("trae", "trae_state_vscdb", "native", true),
    ] {
        let source = sources["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|source| {
                source["provider"] == provider && source["source_format"] == source_format
            })
            .unwrap_or_else(|| panic!("missing {provider} source in {sources:#}"));
        assert_eq!(source["status"], "available");
        assert_eq!(source["import_support"], import_support);
        assert_eq!(source["native_import"], native_import);
        assert_eq!(source["importable"], true);
        assert!(source["unsupported_reason"].is_null());
    }
}

#[test]
fn sources_discovers_shelley_db_env_override() {
    let temp = tempdir();
    let db_path = temp.path().join("custom-shelley.db");
    fs::write(&db_path, b"sqlite fixture marker").unwrap();

    let sources = json_output(
        ctx(&temp)
            .env("SHELLEY_DB", &db_path)
            .args(["sources", "--json"]),
    );
    let source = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| {
            source["provider"] == "shelley" && source["path"] == db_path.to_str().unwrap()
        })
        .unwrap_or_else(|| panic!("missing Shelley source in {sources:#}"));
    assert_eq!(source["source_format"], "shelley_sqlite");
    assert_eq!(source["status"], "available");
    assert_eq!(source["import_support"], "native");
    assert_eq!(source["path"], db_path.to_str().unwrap());
}

#[test]
fn sources_falls_back_to_userprofile_when_home_unset() {
    let temp = tempdir();
    copy_dir_all(
        Path::new(&provider_history_fixture("codex-sessions")),
        &temp.path().join(".codex").join("sessions"),
    );

    let sources = json_output(
        ctx(&temp)
            .env_remove("HOME")
            .env("USERPROFILE", temp.path())
            .args(["sources", "--json"]),
    );
    let codex = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["provider"] == "codex" && source["status"] == "available")
        .unwrap_or_else(|| panic!("missing codex source in {sources:#}"));
    assert!(Path::new(codex["path"].as_str().unwrap()).starts_with(temp.path()));
}

#[test]
fn sources_discovers_forgecode_env_and_legacy_db() {
    let temp = tempdir();
    let fixture = PathBuf::from(write_native_forgecode_fixture(
        &temp,
        "forgecode-env-sources-oracle",
    ));
    let env_root = temp.path().join("custom-forge");
    fs::create_dir_all(&env_root).unwrap();
    let env_db = env_root.join(".forge.db");
    fs::copy(&fixture, &env_db).unwrap();

    let sources = json_output(
        ctx(&temp)
            .env("FORGE_CONFIG", &env_root)
            .args(["sources", "--json"]),
    );
    let source = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["provider"] == "forgecode")
        .unwrap_or_else(|| panic!("missing ForgeCode env source in {sources:#}"));
    assert_eq!(source["status"], "available");
    assert_eq!(source["source_format"], "forgecode_sqlite");
    assert_eq!(source["path"], env_db.to_str().unwrap());

    let legacy_temp = tempdir();
    let legacy_fixture = PathBuf::from(write_native_forgecode_fixture(
        &legacy_temp,
        "forgecode-legacy-sources-oracle",
    ));
    let legacy_root = legacy_temp.path().join("forge");
    fs::create_dir_all(&legacy_root).unwrap();
    let legacy_db = legacy_root.join(".forge.db");
    fs::copy(legacy_fixture, &legacy_db).unwrap();

    let sources = json_output(ctx(&legacy_temp).args(["sources", "--json"]));
    let source = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["provider"] == "forgecode")
        .unwrap_or_else(|| panic!("missing ForgeCode legacy source in {sources:#}"));
    assert_eq!(source["status"], "available");
    assert_eq!(source["source_format"], "forgecode_sqlite");
    assert_eq!(source["path"], legacy_db.to_str().unwrap());
}
#[test]
fn explicit_native_sources_are_listed_but_not_auto_imported() {
    let temp = tempdir();
    ctx(&temp).args(["daemon", "disable"]).assert().success();
    let query = "nanoclaw-explicit-auto-refresh-oracle";
    let project = PathBuf::from(write_native_nanoclaw_fixture(&temp, query));

    let mut sources_command = ctx(&temp);
    sources_command.current_dir(&project);
    let sources = json_output(sources_command.args(["sources", "--json"]));
    let nanoclaw = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["provider"] == "nanoclaw")
        .unwrap();
    assert_eq!(nanoclaw["status"], "available");
    assert_eq!(nanoclaw["import_support"], "explicit");
    assert_eq!(nanoclaw["native_import"], false);
    assert_eq!(nanoclaw["importable"], true);
    assert!(nanoclaw["unsupported_reason"].is_null());

    let mut search_command = ctx(&temp);
    search_command.current_dir(&project);
    let search = json_output(search_command.args([
        "search",
        query,
        "--provider",
        "nanoclaw",
        "--refresh",
        "background",
        "--json",
    ]));
    assert_eq!(search["freshness"]["mode"], "background");
    assert_eq!(search["freshness"]["status"], "no_sources");
    assert_eq!(search["freshness"]["source_count"], 0);
    assert!(search["results"].as_array().unwrap().is_empty());

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "nanoclaw",
        "--path",
        project.to_str().unwrap(),
        "--json",
    ]));
    assert_eq!(imported["totals"]["failed"], 0);
    assert_eq!(imported["totals"]["imported_sources"], 1);

    let search_after_import =
        json_output(ctx(&temp).args(["search", query, "--provider", "nanoclaw", "--json"]));
    assert_search_provider_oracle(&search_after_import, "nanoclaw", query, 1, "message");
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
    assert_eq!(stdout["schema_version"], 1);
    assert_eq!(stdout["totals"]["imported_sources"], 1);
    assert_eq!(stdout["totals"]["failed_sources"], 1);
    assert!(stdout["totals"]["imported_sessions"].as_u64().unwrap() > 0);
    let sources = stdout["sources"].as_array().unwrap();
    assert!(sources
        .iter()
        .any(|source| source["provider"] == "codex" && source["status"] == "imported"));
    assert!(sources
        .iter()
        .any(|source| source["provider"] == "opencode" && source["status"] == "failed"));
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
