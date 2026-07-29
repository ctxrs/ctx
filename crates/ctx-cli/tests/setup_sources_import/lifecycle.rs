use super::{
    assert_daemon_process_running, assert_no_daemon_autostart_mutation, support::*,
    wait_for_daemon_status, write_active_daemon_upgrade_handoff, write_codex_setup_session,
};
use rusqlite::OpenFlags;

use std::{
    io::Read,
    process::{Child, Command as StdCommand, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Barrier,
    },
    thread,
};

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

fn start_full_source_refresh_daemon(temp: &TempDir) -> SourceRefreshDaemon {
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

fn ready_setup(temp: &TempDir) -> Value {
    json_output(ctx(temp).args(["setup", "--wait", "--format=json", "--progress", "none"]))
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
fn setup_without_semantic_flag_preserves_explicit_search_setting() {
    for enabled in [false, true] {
        let temp = tempdir();
        let config_path = temp.path().join("config.toml");
        let original = format!("[search]\nsemantic = {enabled}\n");
        fs::write(&config_path, &original).unwrap();

        let setup = json_output(ctx(&temp).args(["setup", "--format=json", "--progress", "none"]));
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

    let setup = json_output(ctx(&temp).args([
        "setup",
        "--semantic",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(setup["background_indexing"]["semantic_enabled"], true);
    assert_eq!(
        setup["background_indexing"]["daemon_autostart"]["reason"],
        "machine_readable_output"
    );
    assert_no_daemon_autostart_mutation(&temp);

    let config_path = temp.path().join("config.toml");
    let once = fs::read_to_string(&config_path).unwrap();
    assert!(once.contains("[search]\nsemantic = true\n"), "{once}");
    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(status["semantic"]["enabled"], true);
    assert_eq!(status["semantic"]["config_source"], "config");

    json_output(ctx(&temp).args(["setup", "--semantic", "--format=json", "--progress", "none"]));
    assert_eq!(fs::read_to_string(config_path).unwrap(), once);
    assert_no_daemon_autostart_mutation(&temp);
}

#[test]
fn setup_semantic_rejects_disabled_daemon_without_mutating_source_epoch() {
    let temp = tempdir();
    let config_path = temp.path().join("config.toml");
    let original = "[daemon]\nenabled = false\n";
    fs::write(&config_path, original).unwrap();

    let stderr = failure_stderr(ctx(&temp).args([
        "setup",
        "--semantic",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert!(stderr.contains("requires daemon maintenance"), "{stderr}");
    assert_eq!(fs::read_to_string(config_path).unwrap(), original);
    assert!(!temp.path().join("search").exists());
    assert!(!temp.path().join("relational.sqlite").exists());
    assert!(!temp.path().join("catalogs").exists());
    assert_no_daemon_autostart_mutation(&temp);

    let explicit_opt_out = tempdir();
    let stderr = failure_stderr(ctx(&explicit_opt_out).args([
        "setup",
        "--semantic",
        "--no-daemon",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert!(stderr.contains("requires daemon maintenance"), "{stderr}");
    assert!(!explicit_opt_out.path().join("config.toml").exists());
    assert!(!explicit_opt_out.path().join("search").exists());
    assert!(!explicit_opt_out.path().join("relational.sqlite").exists());
    assert!(!explicit_opt_out.path().join("catalogs").exists());
    assert_no_daemon_autostart_mutation(&explicit_opt_out);
}

#[test]
fn setup_semantic_clean_cache_queues_daemon_without_foreground_download() {
    let temp = tempdir();
    write_codex_setup_session(&temp);
    let semantic_cache = temp.path().join("clean-semantic-cache");

    let setup = json_output(
        ctx(&temp)
            .args(["setup", "--format=json", "--progress", "none"])
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
fn setup_wait_indexes_committed_provider_sqlite_wal_content() {
    let temp = tempdir();
    install_default_hermes_fixture(&temp, "provider sqlite canonical content");
    let provider_db = temp.path().join(".hermes/state.db");
    let writer = Connection::open(&provider_db).unwrap();
    let journal_mode: String = writer
        .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(journal_mode, "wal");
    writer
        .execute_batch("PRAGMA wal_autocheckpoint = 0")
        .unwrap();
    let main_before = fs::metadata(&provider_db).unwrap();
    writer
        .execute(
            "INSERT INTO messages (session_id, role, content, timestamp)
             VALUES ('hermes-cli-native', 'user', ?1, 1782259203.0)",
            ["provider sqlite committed wal lifecycle oracle"],
        )
        .unwrap();
    assert!(provider_db.with_extension("db-wal").is_file());
    let main_after = fs::metadata(&provider_db).unwrap();
    assert_eq!(main_after.len(), main_before.len());
    assert_eq!(
        main_after.modified().unwrap(),
        main_before.modified().unwrap()
    );

    let _daemon = start_full_source_refresh_daemon(&temp);
    let setup = ready_setup(&temp);
    assert_eq!(setup["schema_version"], 2, "{setup:#}");
    assert_eq!(setup["mode"], "ready", "{setup:#}");
    let generation = setup["lexical"]["generation_id"].as_str().unwrap();
    let status = wait_for_relational_projection(&temp, generation);
    assert_eq!(status["relational"]["source_count"], 1, "{status:#}");
    assert!(
        status["relational"]["event_count"]
            .as_u64()
            .is_some_and(|count| count >= 3),
        "{status:#}"
    );

    let search = json_output(ctx(&temp).args([
        "search",
        "provider sqlite committed wal lifecycle oracle",
        "--provider",
        "hermes",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(search["retrieval"]["index"], "source_backed", "{search:#}");
    assert_eq!(search["retrieval"]["generation_id"], generation);
    assert_eq!(search["results"].as_array().unwrap().len(), 1, "{search:#}");
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
        !temp.path().join("search").exists(),
        "setup must not create search projections after config load fails"
    );
    assert!(
        !temp.path().join("relational.sqlite").exists(),
        "setup must not create the relational projection after config load fails"
    );
    assert!(
        !temp.path().join("catalogs").exists(),
        "setup must not create source catalogs after config load fails"
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
fn status_missing_source_epoch_is_read_only_and_does_not_initialize_files() {
    let temp = tempdir();
    let data_root = temp.path().join("ctx-data");

    let status = json_output(
        ctx(&temp)
            .args(["status", "--format=json"])
            .env("CTX_DATA_ROOT", &data_root),
    );
    assert_eq!(status["schema_version"], 2);
    assert_eq!(status["initialized"], false);
    assert_eq!(status["local_only"], true);
    assert_eq!(status["read_only"], true);
    assert_eq!(status["history_epoch"]["status"], "unavailable");
    assert_eq!(status["history_epoch"]["reason"], "epoch_not_initialized");
    assert!(status["indexed_items"].is_null());
    assert!(status["indexed_sources"].is_null());
    assert_eq!(
        status["lexical"]["path"],
        json!(data_root.join("search/lexical"))
    );
    assert_eq!(
        status["relational"]["path"],
        json!(data_root.join("relational.sqlite"))
    );
    assert_eq!(status["prior_epoch"]["status"], "absent");

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
        output.contains("history_epoch_status: unavailable"),
        "{output}"
    );

    assert!(
        !data_root.exists(),
        "status must not create the missing data root"
    );
    assert!(!data_root.join("config.toml").exists());
    assert!(!data_root.join("search").exists());
    assert!(!data_root.join("relational.sqlite").exists());
    assert!(!data_root.join("catalogs").exists());
}

#[test]
fn status_existing_relational_projection_does_not_mutate_database() {
    let temp = tempdir();
    write_codex_setup_session(&temp);
    let generation = {
        let _daemon = start_full_source_refresh_daemon(&temp);
        let setup = ready_setup(&temp);
        let generation = setup["lexical"]["generation_id"]
            .as_str()
            .unwrap()
            .to_owned();
        wait_for_relational_projection(&temp, &generation);
        generation
    };
    let relational_path = temp.path().join("relational.sqlite");
    let relational_before = fs::read(&relational_path).unwrap();

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));

    assert_eq!(status["initialized"], true);
    assert_eq!(status["read_only"], true);
    assert_eq!(status["relational"]["status"], "ready", "{status:#}");
    assert_eq!(
        status["relational"]["active_core_generation_id"], generation,
        "{status:#}"
    );
    assert_eq!(
        fs::read(&relational_path).unwrap(),
        relational_before,
        "status must not mutate relational projection pages"
    );
}

#[test]
fn status_reports_unsupported_relational_schema_without_migrating_it() {
    let temp = tempdir();
    let db_path = temp.path().join("relational.sqlite");
    let conn = Connection::open(&db_path).unwrap();
    conn.pragma_update(None, "user_version", 1).unwrap();
    drop(conn);
    let before = fs::read(&db_path).unwrap();

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(status["relational"]["status"], "unavailable", "{status:#}");
    assert_eq!(
        status["relational"]["reason"], "projection_open_failed",
        "{status:#}"
    );
    assert!(
        status["relational"]["last_error"]
            .as_str()
            .is_some_and(|error| !error.is_empty()),
        "{status:#}"
    );

    let conn = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let user_version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(user_version, 1);
    drop(conn);
    assert_eq!(fs::read(&db_path).unwrap(), before);
    assert!(!temp.path().join("config.toml").exists());
    assert!(!temp.path().join("search").exists());
    assert!(!temp.path().join("catalogs").exists());
}

#[test]
fn status_does_not_repair_missing_tantivy_publication_pointer() {
    let temp = tempdir();
    write_codex_setup_session(&temp);
    let generation = {
        let _daemon = start_full_source_refresh_daemon(&temp);
        let setup = ready_setup(&temp);
        let generation = setup["lexical"]["generation_id"]
            .as_str()
            .unwrap()
            .to_owned();
        wait_for_relational_projection(&temp, &generation);
        generation
    };
    let lexical_root = temp.path().join("search/lexical");
    let publication_pointer = lexical_root.join("meta.json");
    assert!(publication_pointer.is_file());
    let manifest_path = lexical_root
        .join("ctx-generations")
        .join(format!("{generation}.json"));
    let manifest_before = fs::read(&manifest_path).unwrap();
    fs::remove_file(&publication_pointer).unwrap();

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(status["initialized"], true);
    assert_eq!(status["read_only"], true);
    assert_eq!(status["lexical"]["status"], "unavailable", "{status:#}");
    assert!(!publication_pointer.exists());
    assert_eq!(fs::read(manifest_path).unwrap(), manifest_before);
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

    let setup = json_output(ctx(&temp).args(["setup", "--catalog-only", "--format=json"]));
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

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
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

    let setup = json_output(ctx(&temp).args(["setup", "--catalog-only", "--format=json"]));
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
        "--format=json",
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

    let status = json_output(ctx(&temp).args(["--quiet", "status", "--format=json"]));
    assert_eq!(status["schema_version"], 1);
    assert_eq!(status["initialized"], false);
    assert!(status["inventory_source_bytes"].is_null());
    assert!(status["lexical_index_estimate_seconds"].is_null());
}

#[test]
fn setup_backgrounds_discovered_codex_sessions_by_default_and_wait_imports() {
    let temp = tempdir();
    write_codex_setup_session(&temp);

    let setup = json_output(ctx(&temp).args(["setup", "--format=json", "--progress", "none"]));
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

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(status["inventory_units"], 1);
    assert_eq!(status["pending_inventory_units"], 1);
    assert_eq!(status["cataloged_sessions"], 1);
    assert_eq!(status["indexed_catalog_sessions"], 0);
    assert_eq!(status["pending_catalog_sessions"], 1);
    assert_eq!(status["daemon"]["status"], "unknown");
    assert!(status["daemon"]["reason"].is_null());
    assert!(status["daemon"]["start_mode"].is_null());
    assert!(status["daemon"]["trigger_command"].is_null());

    let ready =
        json_output(ctx(&temp).args(["setup", "--wait", "--format=json", "--progress", "none"]));
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

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
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
fn setup_wait_imports_discovered_codex_prompt_history() {
    let temp = tempdir();
    let history = temp.path().join(".codex/history.jsonl");
    fs::create_dir_all(history.parent().unwrap()).unwrap();
    fs::write(
        &history,
        concat!(
            r#"{"session_id":"prompt-setup-session","ts":1784371200,"text":"prompt history setup refresh oracle"}"#,
            "\n"
        ),
    )
    .unwrap();

    let setup =
        json_output(ctx(&temp).args(["setup", "--wait", "--format=json", "--progress", "none"]));
    assert_eq!(setup["mode"], "ready");
    assert_eq!(setup["inventory"]["sources"], 1);
    assert_eq!(setup["inventory"]["units"], 1);
    assert_eq!(setup["import"]["totals"]["failed_sources"], 0);
    assert_eq!(setup["import"]["totals"]["imported_sessions"], 1);
    assert_eq!(setup["import"]["totals"]["imported_events"], 1);

    let search = json_output(ctx(&temp).args([
        "search",
        "prompt history setup refresh oracle",
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(
        &search,
        "codex",
        "prompt history setup refresh oracle",
        1,
        "message",
    );
}

#[test]
fn setup_no_daemon_is_one_run_opt_out_and_keeps_semantic_disabled() {
    let temp = tempdir();
    write_codex_setup_session(&temp);

    let setup = json_output(ctx(&temp).args([
        "setup",
        "--no-daemon",
        "--format=json",
        "--progress",
        "none",
    ]));
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

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
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

    let setup =
        json_output(ctx(&temp).args(["setup", "--wait", "--format=json", "--progress", "none"]));
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

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
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
        "--format=json",
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
        .args(["setup", "--wait", "--format=json", "--progress", "none"])
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
    assert!(
        stderr.contains("ctx daemon status --format json"),
        "{stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "failed quiet setup must not print success or queued output: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let status = json_output(ctx(&temp).args(["daemon", "status", "--format=json"]));
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
            .args(["setup", "--format=json", "--progress", "none"])
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
fn human_setup_without_sources_starts_daemon_without_claiming_background_indexing() {
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
        stdout.contains("ctx is initialized; no local history was indexed"),
        "{stdout}"
    );
    assert!(
        stdout.contains("background maintenance handoff is verified"),
        "{stdout}"
    );
    assert!(!stdout.contains("indexing is queued"), "{stdout}");
    assert!(
        !stdout.contains("queued your local agent history"),
        "{stdout}"
    );
    assert!(stdout.contains("  ctx sources"), "{stdout}");
    assert!(stdout.contains("  ctx import --all"), "{stdout}");
    assert!(
        !stdout.contains("  ctx search \"test failure\""),
        "{stdout}"
    );

    let running = json_output(ctx(&temp).args(["daemon", "status", "--format=json"]));
    assert_eq!(running["daemon"]["status"], "running", "{running:#}");
    assert_eq!(running["daemon"]["running"], true, "{running:#}");
    assert_eq!(running["daemon"]["trigger_command"], "setup", "{running:#}");
    assert_eq!(running["daemon"]["start_mode"], "auto", "{running:#}");
    let pid = running["daemon"]["pid"].as_u64().unwrap() as u32;
    assert_daemon_process_running(pid);

    let completed = wait_for_daemon_status(&temp, "completed", false, "setup");
    assert_eq!(completed["daemon"]["pid"], pid, "{completed:#}");
    assert!(completed["daemon"]["last_error"].is_null(), "{completed:#}");
    let lock: Value =
        serde_json::from_slice(&fs::read(temp.path().join("daemon/daemon.lock")).unwrap()).unwrap();
    assert_eq!(lock["pid"], pid, "{lock:#}");
    assert_eq!(lock["released"], true, "{lock:#}");
}

#[test]
fn daemon_once_rejections_complete_and_preserve_diagnostics() {
    let temp = tempdir();
    let binary = copied_ctx_binary(&temp);
    let sessions = temp
        .path()
        .join(".codex")
        .join("sessions")
        .join("2026/06/24");
    fs::create_dir_all(&sessions).unwrap();
    fs::copy(
        provider_history_fixture("codex-malformed-session.jsonl"),
        sessions.join("rollout-malformed.jsonl"),
    )
    .unwrap();
    ctx_from_binary(&temp, &binary)
        .args([
            "setup",
            "--catalog-only",
            "--no-daemon",
            "--progress",
            "none",
        ])
        .assert()
        .success();

    let output = ctx_from_binary(&temp, &binary)
        .args(["daemon", "run", "--once", "--force", "--format=json"])
        .env("CTX_UPGRADE_AUTO", "off")
        .assert()
        .success()
        .get_output()
        .clone();
    let daemon: Value = serde_json::from_slice(&output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();

    assert_eq!(daemon["status"], "completed", "{daemon:#}");
    assert_eq!(
        daemon["jobs"]["history_refresh"]["status"], "completed",
        "{daemon:#}"
    );
    assert_eq!(
        daemon["jobs"]["history_refresh"]["totals"]["rejected_records"], 1,
        "{daemon:#}"
    );
    assert_eq!(
        daemon["jobs"]["history_refresh"]["totals"]["failed_sources"], 0,
        "{daemon:#}"
    );
    assert!(daemon["last_error"].is_null(), "{daemon:#}");
    assert!(stderr.is_empty(), "{stderr}");

    let index =
        json_output(ctx_from_binary(&temp, &binary).args(["index", "status", "--format=json"]));
    assert_eq!(index["lexical"]["status"], "ready", "{index:#}");
    assert_eq!(index["lexical"]["pending_inventory_units"], 0, "{index:#}");
    ctx_from_binary(&temp, &binary)
        .args([
            "index",
            "watch",
            "--format=jsonl",
            "--interval-seconds",
            "1",
        ])
        .timeout(Duration::from_secs(3))
        .assert()
        .success();

    let lock: Value =
        serde_json::from_slice(&fs::read(temp.path().join("daemon/daemon.lock")).unwrap()).unwrap();
    assert_eq!(lock["released"], true, "{lock:#}");
}

#[test]
fn daemon_rejection_diagnostics_survive_a_later_healthy_source_cycle() {
    let temp = tempdir();
    let binary = copied_ctx_binary(&temp);
    let sessions = temp
        .path()
        .join(".codex")
        .join("sessions")
        .join("2026/06/24");
    fs::create_dir_all(&sessions).unwrap();
    fs::copy(
        provider_history_fixture("codex-malformed-session.jsonl"),
        sessions.join("rollout-malformed.jsonl"),
    )
    .unwrap();
    fs::write(
        temp.path().join(".codex/history.jsonl"),
        concat!(
            r#"{"session_id":"prompt-daemon-session","ts":1784371200,"text":"healthy prompt source"}"#,
            "\n"
        ),
    )
    .unwrap();
    ctx_from_binary(&temp, &binary)
        .args([
            "setup",
            "--catalog-only",
            "--no-daemon",
            "--progress",
            "none",
        ])
        .assert()
        .success();

    let mut saw_rejection = false;
    let mut saw_later_healthy_cycle = false;
    for _ in 0..4 {
        let report = json_output(
            ctx_from_binary(&temp, &binary)
                .args(["daemon", "run", "--once", "--force", "--format=json"])
                .env("CTX_UPGRADE_AUTO", "off"),
        );
        let rejected = report["jobs"]["history_refresh"]["totals"]["rejected_records"]
            .as_u64()
            .unwrap_or(0);
        let preserved = report["jobs"]["history_refresh"]["rejection_diagnostics"]
            ["rejected_records"]
            .as_u64()
            .unwrap_or(0);
        if rejected > 0 {
            assert_eq!(rejected, 1, "{report:#}");
            assert_eq!(preserved, 1, "{report:#}");
            saw_rejection = true;
        } else if saw_rejection {
            assert_eq!(preserved, 1, "{report:#}");
            saw_later_healthy_cycle = true;
            break;
        }
    }
    assert!(saw_rejection, "malformed source was never selected");
    assert!(
        saw_later_healthy_cycle,
        "healthy source was not selected after the malformed source"
    );

    let doctor = json_output(ctx_from_binary(&temp, &binary).args([
        "doctor",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(
        doctor["daemon"]["jobs"]["history_refresh"]["rejection_diagnostics"]["rejected_records"], 1,
        "{doctor:#}"
    );
}

#[test]
fn daemon_once_refreshes_discovered_codex_prompt_history() {
    let temp = tempdir();
    let binary = copied_ctx_binary(&temp);
    let history = temp.path().join(".codex/history.jsonl");
    fs::create_dir_all(history.parent().unwrap()).unwrap();
    fs::write(
        &history,
        concat!(
            r#"{"session_id":"prompt-daemon-session","ts":1784371200,"text":"prompt history daemon refresh oracle"}"#,
            "\n"
        ),
    )
    .unwrap();
    ctx_from_binary(&temp, &binary)
        .args([
            "setup",
            "--catalog-only",
            "--no-daemon",
            "--progress",
            "none",
        ])
        .assert()
        .success();

    let daemon = json_output(
        ctx_from_binary(&temp, &binary)
            .args(["daemon", "run", "--once", "--force", "--format=json"])
            .env("CTX_UPGRADE_AUTO", "off"),
    );
    assert_eq!(daemon["status"], "completed", "{daemon:#}");
    assert_eq!(
        daemon["jobs"]["history_refresh"]["status"], "completed",
        "{daemon:#}"
    );
    assert_eq!(
        daemon["jobs"]["history_refresh"]["totals"]["imported_sessions"], 1,
        "{daemon:#}"
    );
    assert_eq!(
        daemon["jobs"]["history_refresh"]["totals"]["imported_events"], 1,
        "{daemon:#}"
    );

    let search = json_output(ctx_from_binary(&temp, &binary).args([
        "search",
        "prompt history daemon refresh oracle",
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(
        &search,
        "codex",
        "prompt history daemon refresh oracle",
        1,
        "message",
    );
}

#[test]
fn human_wait_setup_starts_daemon_after_foreground_import() {
    let temp = tempdir();
    write_codex_setup_session(&temp);
    let binary = copied_ctx_binary(&temp);

    let output = ctx_from_binary(&temp, &binary)
        .args(["setup", "--wait", "--progress", "none"])
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
        stdout.contains("ctx local agent history search is ready"),
        "{stdout}"
    );
    assert!(
        stdout.contains("background maintenance handoff is verified"),
        "{stdout}"
    );
    assert!(!stdout.contains("indexing is queued"), "{stdout}");
    assert!(
        !stdout.contains("queued your local agent history"),
        "{stdout}"
    );

    let running = json_output(ctx(&temp).args(["daemon", "status", "--format=json"]));
    assert_eq!(running["daemon"]["status"], "running", "{running:#}");
    assert_eq!(running["daemon"]["running"], true, "{running:#}");
    assert_eq!(running["daemon"]["trigger_command"], "setup", "{running:#}");
    assert_eq!(running["daemon"]["start_mode"], "auto");
    let pid = running["daemon"]["pid"].as_u64().unwrap() as u32;
    assert_daemon_process_running(pid);

    let completed = wait_for_daemon_status(&temp, "completed", false, "setup");
    assert_eq!(completed["daemon"]["pid"], pid);
    assert!(completed["daemon"]["finished_at_ms"].as_i64().unwrap() > 0);
    assert!(completed["daemon"]["last_error"].is_null(), "{completed:#}");
    let lock: Value =
        serde_json::from_slice(&fs::read(temp.path().join("daemon/daemon.lock")).unwrap()).unwrap();
    assert_eq!(lock["pid"], pid, "{lock:#}");
    assert_eq!(lock["released"], true, "{lock:#}");
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

    let setup =
        json_output(ctx(&temp).args(["setup", "--wait", "--format=json", "--progress", "none"]));
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

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
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

    let setup =
        json_output(ctx(&temp).args(["setup", "--wait", "--format=json", "--progress", "none"]));
    assert_eq!(setup["inventory"]["sources"], 1);
    assert_eq!(setup["inventory"]["units"], 1);
    assert_eq!(setup["inventory"]["source_import_files"], 1);
    assert_eq!(setup["inventory"]["indexed_source_import_files"], 1);
    assert_eq!(setup["inventory"]["pending_source_import_files"], 0);
    assert_eq!(setup["catalog"]["cataloged_sessions"], 0);
    assert_eq!(setup["import"]["totals"]["imported_sources"], 1);
    assert_eq!(setup["import"]["totals"]["failed_sources"], 0);

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(status["inventory_units"], 1);
    assert_eq!(status["source_import_files"], 1);
    assert_eq!(status["indexed_source_import_files"], 1);
    assert_eq!(status["pending_inventory_units"], 0);
}

#[test]
fn clean_multisource_setup_bounds_relational_wal_and_preserves_projection_identity() {
    let temp = tempdir();
    write_large_codex_setup_sessions(&temp, 40, 4, 4 * 1024);
    write_large_hermes_setup_db(&temp, 130, 8 * 1024);
    let _daemon = start_full_source_refresh_daemon(&temp);
    let db_path = temp.path().join("relational.sqlite");
    let wal_path = temp.path().join("relational.sqlite-wal");

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
    let setup = ready_setup(&temp);
    let generation = setup["lexical"]["generation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let status = wait_for_relational_projection(&temp, &generation);
    running.store(false, Ordering::Release);
    sampler.join().unwrap();

    assert_eq!(setup["schema_version"], 2, "{setup:#}");
    assert_eq!(setup["mode"], "ready", "{setup:#}");
    assert_eq!(status["relational"]["status"], "ready", "{status:#}");
    assert_eq!(status["relational"]["source_count"], 41, "{status:#}");
    assert!(
        peak_wal_bytes.load(Ordering::Acquire) <= 32 * 1024 * 1024,
        "clean multi-source setup grew relational WAL to {} bytes",
        peak_wal_bytes.load(Ordering::Acquire)
    );
    assert!(
        fs::metadata(temp.path().join("relational.sqlite-wal"))
            .map(|metadata| metadata.len())
            .unwrap_or(0)
            <= 4 * 1024 * 1024,
        "setup left a large final relational WAL"
    );

    let conn = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(
        conn.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert!(
        sqlite_count(
            &conn,
            "SELECT COUNT(*) FROM source_backed_events
             WHERE source_id IN (
                 SELECT source_id FROM source_backed_sources WHERE provider = 'codex'
             )"
        ) > 0
    );
    assert!(
        sqlite_count(
            &conn,
            "SELECT COUNT(*) FROM source_backed_events
             WHERE source_id IN (
                 SELECT source_id FROM source_backed_sources WHERE provider = 'hermes'
             )"
        ) > 0
    );
    let event_count = sqlite_count(&conn, "SELECT COUNT(*) FROM source_backed_events");
    drop(conn);

    let codex_search = json_output(ctx(&temp).args([
        "search",
        "codex setup history",
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(codex_search["retrieval"]["generation_id"], generation);
    assert!(!codex_search["results"].as_array().unwrap().is_empty());
    let hermes_search = json_output(ctx(&temp).args([
        "search",
        "hermes setup current",
        "--provider",
        "hermes",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(hermes_search["retrieval"]["generation_id"], generation);
    assert!(!hermes_search["results"].as_array().unwrap().is_empty());

    let replay = ready_setup(&temp);
    assert_eq!(replay["lexical"]["generation_id"], generation, "{replay:#}");
    wait_for_relational_projection(&temp, &generation);
    let conn = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(
        sqlite_count(&conn, "SELECT COUNT(*) FROM source_backed_events"),
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
    let payload = "provider source checkpoint bounded lexical generation "
        .repeat(payload_bytes / "provider source checkpoint bounded lexical generation ".len() + 1);
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
    let payload = "provider import relational recovery bounded checkpoint ".repeat(
        payload_bytes / "provider import relational recovery bounded checkpoint ".len() + 1,
    );
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
