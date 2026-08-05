use super::{
    assert_daemon_process_running, assert_no_daemon_autostart_mutation, ctx, support, support::*,
    wait_for_daemon_status, write_codex_setup_session,
};

#[path = "../support/setup_sources_import/lifecycle_helpers.rs"]
mod lifecycle_helpers;
use lifecycle_helpers::*;

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
    let config_path = data_root(&temp).join("config.toml");
    fs::create_dir_all(data_root(&temp)).unwrap();

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
        let config_path = data_root(&temp).join("config.toml");
        fs::create_dir_all(data_root(&temp)).unwrap();
        let original = format!("[search]\nsemantic = {enabled}\n");
        fs::write(&config_path, &original).unwrap();

        let setup =
            json_output(support::ctx(&temp).args(["setup", "--format=json", "--progress", "none"]));
        assert_eq!(setup["semantic"]["enabled"], enabled, "{setup:#}");
        assert_eq!(fs::read_to_string(config_path).unwrap(), original);
        assert_no_daemon_autostart_mutation(&temp);
    }
}

#[test]
fn setup_semantic_persists_opt_in_when_autostart_is_explicitly_disabled() {
    let temp = tempdir();
    write_codex_setup_session(&temp);

    let setup = json_output(support::ctx(&temp).args([
        "setup",
        "--semantic",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(setup["semantic"]["enabled"], true);
    assert_eq!(setup["daemon_autostart"]["reason"], "autostart_disabled");
    assert_no_daemon_autostart_mutation(&temp);

    let config_path = data_root(&temp).join("config.toml");
    let once = fs::read_to_string(&config_path).unwrap();
    assert!(once.contains("[search]\nsemantic = true\n"), "{once}");
    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(status["semantic"]["enabled"], true);
    assert_eq!(status["semantic"]["config_source"], "config");

    json_output(support::ctx(&temp).args([
        "setup",
        "--semantic",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(fs::read_to_string(config_path).unwrap(), once);
    assert_no_daemon_autostart_mutation(&temp);
}

#[test]
fn setup_semantic_rejects_disabled_daemon_without_mutating_source_epoch() {
    let temp = tempdir();
    let config_path = data_root(&temp).join("config.toml");
    fs::create_dir_all(data_root(&temp)).unwrap();
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
    assert!(!data_root(&temp).join("search").exists());
    assert!(!data_root(&temp).join("relational.sqlite").exists());
    assert!(!data_root(&temp).join("catalogs").exists());
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
    assert!(!data_root(&explicit_opt_out).join("config.toml").exists());
    assert!(!data_root(&explicit_opt_out).join("search").exists());
    assert!(!data_root(&explicit_opt_out)
        .join("relational.sqlite")
        .exists());
    assert!(!data_root(&explicit_opt_out).join("catalogs").exists());
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

    assert_eq!(setup["semantic"]["enabled"], true);
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
    let _status = wait_for_core_generation(&temp, generation);
    assert!(
        provider_core_counts(&data_root(&temp), "hermes").1 >= 3,
        "committed provider WAL records must be included in Core"
    );
    assert!(!data_root(&temp).join("relational.sqlite").exists());

    let search = json_output(ctx(&temp).args([
        "search",
        "provider sqlite committed wal lifecycle oracle",
        "--provider",
        "hermes",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(search["retrieval"]["index"], "core", "{search:#}");
    assert_eq!(search["retrieval"]["generation_id"], generation);
    assert_eq!(search["results"].as_array().unwrap().len(), 1, "{search:#}");
    drop(writer);
}

#[test]
fn malformed_present_config_fails_before_setup_and_analytics_side_effects() {
    let temp = tempdir();
    let state = temp.path().join("state");
    let events_path = temp.path().join("analytics.jsonl");
    fs::create_dir_all(data_root(&temp)).unwrap();
    fs::write(
        data_root(&temp).join("config.toml"),
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
        !data_root(&temp).join("search").exists(),
        "setup must not create search projections after config load fails"
    );
    assert!(
        !data_root(&temp).join("relational.sqlite").exists(),
        "setup must not create the relational projection after config load fails"
    );
    assert!(
        !data_root(&temp).join("catalogs").exists(),
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
    assert_eq!(
        status["history_epoch"]["reason"],
        "generation_not_published"
    );
    assert!(status["indexed_items"].is_null());
    assert!(status["indexed_sources"].is_null());
    assert_eq!(
        status["lexical"]["path"],
        json!(data_root.join("search/lexical"))
    );
    assert!(status.get("relational").is_none(), "{status:#}");
    assert!(status.get("prior_epoch").is_none());

    let output = ctx(&temp)
        .arg("status")
        .env("CTX_DATA_ROOT", &data_root)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("History status: failed"), "{output}");
    assert!(
        output.contains("history has not been indexed yet"),
        "{output}"
    );
    assert!(output.contains("ctx setup"), "{output}");

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
        wait_for_core_generation(&temp, &generation);
        generation
    };
    let lexical_root = data_root(&temp).join("search/lexical");
    let publication_pointer = lexical_root.join("active-generation.json");
    assert!(publication_pointer.is_file());
    let manifest_path = lexical_root
        .join("ctx-generations")
        .join(format!("{generation}.json"));
    let manifest_before = fs::read(&manifest_path).unwrap();
    fs::remove_file(&publication_pointer).unwrap();

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(status["initialized"], false);
    assert_eq!(status["read_only"], true);
    assert!(
        matches!(
            status["lexical"]["status"].as_str(),
            Some("pending" | "unavailable")
        ),
        "{status:#}"
    );
    assert!(!publication_pointer.exists());
    assert_eq!(fs::read(manifest_path).unwrap(), manifest_before);
}

#[test]
fn deprecated_catalog_only_is_ignored_and_wait_publishes_setup_status_contract() {
    let temp = tempdir();
    write_codex_setup_session(&temp);

    let setup = json_output(ctx(&temp).args([
        "setup",
        "--catalog-only",
        "--wait",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(setup["schema_version"], 2, "{setup:#}");
    assert_eq!(setup["deprecated_catalog_only_ignored"], true, "{setup:#}");
    assert!(setup.get("read_only").is_none(), "{setup:#}");
    assert_eq!(setup["mode"], "ready", "{setup:#}");
    assert_eq!(setup["lexical"]["certified_sources"], 1, "{setup:#}");
    assert!(
        setup["lexical"]["indexed_documents"]
            .as_u64()
            .is_some_and(|count| count >= 1),
        "{setup:#}"
    );
    for counter in [
        "indexed_items",
        "indexed_sessions",
        "indexed_events",
        "indexed_sources",
    ] {
        assert!(
            setup[counter].as_u64().is_some(),
            "setup omitted the {counter} status counter: {setup:#}"
        );
    }
    assert_eq!(
        setup["indexed_items"], setup["lexical"]["indexed_documents"],
        "{setup:#}"
    );
    assert_eq!(setup["indexed_events"], setup["indexed_items"], "{setup:#}");
    assert_eq!(
        setup["indexed_sources"], setup["lexical"]["certified_sources"],
        "{setup:#}"
    );
}

#[test]
fn deprecated_catalog_only_is_ignored_for_non_codex_sources() {
    let temp = tempdir();
    install_default_claude_fixture(&temp, "catalog-only claude inventory");

    let setup = json_output(ctx(&temp).args([
        "setup",
        "--catalog-only",
        "--wait",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(setup["deprecated_catalog_only_ignored"], true, "{setup:#}");
    assert_eq!(setup["mode"], "ready", "{setup:#}");
    assert_eq!(setup["lexical"]["certified_sources"], 1, "{setup:#}");
    assert!(
        setup["lexical"]["indexed_documents"]
            .as_u64()
            .is_some_and(|count| count >= 1),
        "{setup:#}"
    );
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
    assert_eq!(setup["schema_version"], 2);
    assert_eq!(setup["deprecated_catalog_only_ignored"], true);
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
        .stdout(predicate::str::contains("History status: failed"));

    let status = json_output(ctx(&temp).args(["--quiet", "status", "--format=json"]));
    assert_eq!(status["schema_version"], 2);
    assert_eq!(status["initialized"], false);
    assert!(status["inventory_source_bytes"].is_null());
    assert!(status["lexical_index_estimate_seconds"].is_null());
}

#[test]
fn setup_background_refresh_and_wait_publish_the_same_codex_source() {
    let temp = tempdir();
    write_codex_setup_session(&temp);

    let setup = json_output(ctx(&temp).args(["setup", "--format=json", "--progress", "none"]));
    assert_eq!(setup["schema_version"], 2, "{setup:#}");
    assert_eq!(setup["daemon_autostart"]["requested"], true, "{setup:#}");
    assert!(
        matches!(
            setup["refresh_request"]["status"].as_str(),
            Some("queued" | "pending" | "running" | "published")
        ),
        "{setup:#}"
    );

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(status["daemon"]["running"], true, "{status:#}");

    let ready =
        json_output(ctx(&temp).args(["setup", "--wait", "--format=json", "--progress", "none"]));
    assert_eq!(ready["mode"], "ready");
    assert!(
        ready["lexical"]["indexed_documents"]
            .as_u64()
            .is_some_and(|count| count >= 1),
        "{ready:#}"
    );

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(status["lexical"]["status"], "ready", "{status:#}");
    assert!(status["lexical"]["indexed_documents"].as_u64().unwrap() > 0);
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
    assert!(human_setup.contains("History is ready to search"));
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
    assert_eq!(setup["lexical"]["certified_sources"], 1, "{setup:#}");
    assert_eq!(setup["lexical"]["indexed_documents"], 1, "{setup:#}");

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
    assert_eq!(setup["schema_version"], 2, "{setup:#}");
    assert!(setup.get("background_indexing").is_none(), "{setup:#}");
    assert_eq!(
        setup["daemon_autostart"]["status"], "not_requested",
        "{setup:#}"
    );
    assert_eq!(
        setup["daemon_autostart"]["reason"], "explicit_opt_out",
        "{setup:#}"
    );
    assert_eq!(setup["daemon_autostart"]["requested"], false, "{setup:#}");
    assert_eq!(
        setup["refresh_request"]["reason"], "explicit_opt_out",
        "{setup:#}"
    );

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(status["daemon"]["enabled"], true);
    assert_eq!(status["semantic"]["status"], "disabled");
    assert_eq!(status["semantic"]["reason"], "semantic_disabled");
    assert!(!data_root(&temp).join("config.toml").exists());

    let human_setup = ctx(&temp)
        .args(["setup", "--no-daemon", "--progress", "none"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let human_setup = String::from_utf8(human_setup).unwrap();
    assert!(
        human_setup.contains("Background  skipped because --no-daemon was used"),
        "{human_setup}"
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

    let setup =
        json_output(ctx(&temp).args(["setup", "--wait", "--format=json", "--progress", "none"]));
    assert_eq!(setup["mode"], "ready", "{setup:#}");
    assert_eq!(setup["lexical"]["certified_sources"], 2, "{setup:#}");
    assert!(
        setup["lexical"]["indexed_documents"]
            .as_u64()
            .is_some_and(|count| count >= 1),
        "{setup:#}"
    );
    assert_eq!(
        setup["refresh_request"]["receipt"]["current"]["current_rejected_records"], 0,
        "{setup:#}"
    );
    assert_eq!(
        setup["refresh_request"]["receipt"]["current"]["current_ignored_records"], 1,
        "{setup:#}"
    );

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(status["lexical"]["status"], "ready", "{status:#}");
    assert!(status["lexical"]["indexed_documents"].as_u64().unwrap() > 0);

    let search = json_output(ctx(&temp).args([
        "search",
        "setup should import",
        "--provider",
        "codex",
        "--format=json",
    ]));
    assert_search_provider_oracle(&search, "codex", "setup should import", 1, "message");
}

#[test]
fn setup_all_invalid_source_publishes_a_verified_empty_generation() {
    let temp = tempdir();
    let sessions = temp
        .path()
        .join(".codex")
        .join("sessions")
        .join("2026/06/24");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(sessions.join("rollout-empty-only.jsonl"), "").unwrap();

    let setup =
        json_output(ctx(&temp).args(["setup", "--wait", "--format=json", "--progress", "none"]));
    assert_eq!(setup["schema_version"], 2, "{setup:#}");
    assert_eq!(setup["mode"], "ready", "{setup:#}");
    assert_eq!(setup["lexical"]["certified_sources"], 1, "{setup:#}");
    assert_eq!(setup["lexical"]["indexed_documents"], 0, "{setup:#}");
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
fn machine_readable_setup_attempts_enabled_daemon_startup() {
    let temp = tempdir();
    let missing_exe = temp.path().join("missing-ctx-binary");

    let stderr = failure_stderr(
        ctx(&temp)
            .args(["setup", "--format=json", "--progress", "none"])
            .env("CTX_DAEMON_AUTOSTART_EXE", &missing_exe)
            .env_remove("CI")
            .env_remove("CTX_DAEMON_AUTOSTART_OFF"),
    );
    assert!(stderr.contains("ctx daemon did not start"), "{stderr}");
    let status = json_output(ctx(&temp).args(["daemon", "status", "--format=json"]));
    assert_eq!(status["daemon"]["status"], "failed", "{status:#}");
    assert_eq!(status["daemon"]["reason"], "spawn_failed", "{status:#}");
}

#[test]
fn machine_readable_setup_uses_v2_top_level_persistent_daemon_contract() {
    let temp = tempdir();

    let setup = json_output(ctx(&temp).args(["setup", "--format=json", "--progress", "none"]));
    assert_eq!(setup["schema_version"], 2, "{setup:#}");
    assert!(setup.get("background_indexing").is_none(), "{setup:#}");
    assert_eq!(setup["daemon_autostart"]["status"], "degraded", "{setup:#}");
    assert_eq!(setup["daemon_autostart"]["requested"], true, "{setup:#}");
    assert_eq!(
        setup["daemon_autostart"]["reason"], "native_supervisor_unavailable",
        "{setup:#}"
    );
    assert_eq!(setup["daemon_autostart"]["persistent"], false, "{setup:#}");
    assert_eq!(
        setup["daemon_autostart"]["supervisor"]["status"], "fallback",
        "{setup:#}"
    );
    let pid = setup["daemon_autostart"]["pid"].as_u64().unwrap() as u32;
    assert_daemon_process_running(pid);

    let running = json_output(ctx(&temp).args(["daemon", "status", "--format=json"]));
    assert_eq!(running["daemon"]["running"], true, "{running:#}");
    assert_eq!(running["daemon"]["pid"], pid, "{running:#}");
    assert_eq!(running["daemon"]["trigger_command"], "setup", "{running:#}");
}

#[test]
fn progress_json_setup_attempts_enabled_daemon_startup() {
    let temp = tempdir();
    let missing_exe = temp.path().join("missing-ctx-binary");

    let output = ctx(&temp)
        .args(["setup", "--progress", "json"])
        .env("CTX_DAEMON_AUTOSTART_EXE", &missing_exe)
        .env_remove("CI")
        .env_remove("CTX_DAEMON_AUTOSTART_OFF")
        .assert()
        .failure()
        .get_output()
        .clone();

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("ctx daemon did not start"), "{stderr}");
}

#[test]
fn setup_wait_progress_json_uses_stderr_and_keeps_final_json_on_stdout() {
    let temp = tempdir();
    let _daemon = start_full_source_refresh_daemon(&temp);
    let output = ctx(&temp)
        .args(["setup", "--wait", "--format=json", "--progress", "json"])
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["schema_version"], 2, "{stdout:#}");
    let events = String::from_utf8(output.stderr)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(!events.is_empty());
    assert!(events.iter().all(|event| event["operation"] == "setup"));
    assert_eq!(
        events.iter().filter(|event| event["done"] == true).count(),
        1
    );
    let terminal = events.last().unwrap();
    assert_eq!(terminal["request_state"], "published");
    assert_eq!(terminal["logical_phase"], "terminal");
    assert!(terminal["structured_outcome"]["code"].is_string());
}

#[test]
fn human_setup_without_sources_starts_daemon_and_reports_observed_refresh_state() {
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
    let reports_ready = stdout.contains("History is ready to search");
    let reports_queued = stdout.contains("History indexing is queued");
    assert_ne!(reports_ready, reports_queued, "{stdout}");
    if reports_ready {
        assert!(
            stdout.contains("Sources     0 sources")
                && stdout.contains("Events      0 searchable events"),
            "{stdout}"
        );
        assert!(stdout.contains("  ctx search \"test failure\""), "{stdout}");
    } else {
        assert!(
            stdout.contains("Background indexing will publish the first searchable index."),
            "{stdout}"
        );
        assert!(stdout.contains("  ctx index watch"), "{stdout}");
    }
    assert!(!stdout.contains("Refresh"), "{stdout}");

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
        serde_json::from_slice(&fs::read(data_root(&temp).join("daemon/daemon.lock")).unwrap())
            .unwrap();
    assert_eq!(lock["pid"], pid, "{lock:#}");
    assert_eq!(lock["released"], true, "{lock:#}");
}

#[test]
fn foreground_import_rejections_complete_and_preserve_diagnostics() {
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

    let _daemon = start_full_source_refresh_daemon(&temp);
    let import = json_output(
        ctx_from_binary(&temp, &binary)
            .args(["import", "--all", "--format=json", "--progress", "none"])
            .env("CTX_UPGRADE_AUTO", "off"),
    );
    let source = &import["sources"][0];
    let generation = source["published_generation"].as_str().unwrap();

    assert_eq!(import["outcome"], "completed_with_rejections", "{import:#}");
    assert_eq!(
        import["totals"]["current_rejected_records"], 1,
        "{import:#}"
    );
    assert_eq!(
        import["totals"]["current_sources_with_rejections"], 1,
        "{import:#}"
    );
    assert_eq!(source["status"], "partial", "{import:#}");

    let status = wait_for_core_generation(&temp, generation);
    assert_eq!(status["lexical"]["generation_id"], generation, "{status:#}");
    assert!(status.get("relational").is_none(), "{status:#}");
    assert!(!data_root(&temp).join("relational.sqlite").exists());

    let status = json_output(ctx_from_binary(&temp, &binary).args(["status", "--format=json"]));
    assert_eq!(status["lexical"]["status"], "ready", "{status:#}");
    assert_eq!(status["refresh"]["status"], "partial", "{status:#}");
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
}

#[test]
fn foreground_import_rejection_diagnostics_survive_a_noop_source_cycle() {
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

    let _daemon = start_full_source_refresh_daemon(&temp);
    let mut generation = None;
    let mut refresh_request_id = None;
    for cycle in 0..2 {
        let report = json_output(
            ctx_from_binary(&temp, &binary)
                .args(["import", "--all", "--format=json", "--progress", "none"])
                .env("CTX_UPGRADE_AUTO", "off"),
        );
        let refresh = &report["sources"][0];
        let request_id = refresh["daemon_request_id"].as_str();
        assert_ne!(request_id, refresh_request_id.as_deref(), "{report:#}");
        refresh_request_id = request_id.map(str::to_owned);
        assert_eq!(refresh["current_rejected_records"], 1, "{report:#}");
        assert_eq!(refresh["current_sources_with_rejections"], 1, "{report:#}");
        let published = refresh["published_generation"].as_str().unwrap();
        if cycle == 0 {
            generation = Some(published.to_owned());
        } else {
            assert_eq!(Some(published), generation.as_deref(), "{report:#}");
            assert_eq!(refresh["generation_changed"], false, "{report:#}");
        }
        let status = wait_for_core_generation(&temp, published);
        assert_eq!(status["lexical"]["generation_id"], published, "{status:#}");
        assert!(status.get("relational").is_none(), "{status:#}");
    }

    let doctor = json_output(ctx_from_binary(&temp, &binary).args(["doctor", "--format=json"]));
    assert_eq!(
        doctor["daemon"]["jobs"]["core_refresh"]["receipt"]["current"]["current_rejected_records"],
        1,
        "{doctor:#}"
    );
}

#[test]
fn foreground_import_returns_at_ready_core_generation() {
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

    let core_daemon = start_core_only_source_refresh_daemon(&temp);
    let import = json_output(
        ctx_from_binary(&temp, &binary)
            .args([
                "import",
                "--provider",
                "codex",
                "--format=json",
                "--progress",
                "none",
            ])
            .timeout(Duration::from_secs(10))
            .env("CTX_UPGRADE_AUTO", "off"),
    );
    assert_eq!(import["outcome"], "success", "{import:#}");
    assert_eq!(import["totals"]["current_source_count"], 1, "{import:#}");
    assert_eq!(
        import["totals"]["current_indexed_documents"], 1,
        "{import:#}"
    );
    let generation = import["sources"][0]["published_generation"]
        .as_str()
        .unwrap();

    let status = json_output(ctx_from_binary(&temp, &binary).args(["status", "--format=json"]));
    assert_eq!(status["lexical"]["generation_id"], generation, "{status:#}");
    assert_eq!(status["lexical"]["status"], "ready", "{status:#}");
    assert_eq!(status["refresh"]["status"], "ready", "{status:#}");
    assert_eq!(
        status["refresh"]["published_generation"], generation,
        "{status:#}"
    );
    assert!(status.get("relational").is_none(), "{status:#}");
    assert!(!data_root(&temp).join("relational.sqlite").exists());

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

    drop(core_daemon);
}

#[test]
fn human_wait_setup_starts_daemon_after_foreground_import() {
    let temp = tempdir();
    write_codex_setup_session(&temp);
    let binary = copied_ctx_binary(&temp);

    let output = ctx_from_binary(&temp, &binary)
        .args(["setup", "--wait", "--progress", "none"])
        .env("CTX_DAEMON_AUTOSTART_IDLE_EXIT_SECONDS", "2")
        .env("CTX_DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS", "60")
        .env("CTX_UPGRADE_AUTO", "off")
        .env_remove("CI")
        .env_remove("CTX_DAEMON_AUTOSTART_OFF")
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("History is ready to search"), "{stdout}");
    assert!(!stdout.contains("Refresh"), "{stdout}");
    assert!(stdout.contains("  ctx search \"test failure\""), "{stdout}");

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
        serde_json::from_slice(&fs::read(data_root(&temp).join("daemon/daemon.lock")).unwrap())
            .unwrap();
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
    assert_eq!(setup["mode"], "ready", "{setup:#}");
    assert_eq!(setup["lexical"]["certified_sources"], 1, "{setup:#}");
    assert!(
        setup["lexical"]["indexed_documents"]
            .as_u64()
            .is_some_and(|count| count >= 2),
        "{setup:#}"
    );
    let generation = setup["lexical"]["generation_id"].as_str().unwrap();
    let status = wait_for_core_generation(&temp, generation);
    assert_eq!(status["lexical"]["status"], "ready", "{status:#}");
    let (sessions, events) = provider_core_counts(&data_root(&temp), "claude");
    assert_eq!(sessions, 1);
    assert!(events >= 2);
    assert!(!data_root(&temp).join("relational.sqlite").exists());
}

#[test]
fn setup_inventories_whole_source_sqlite_providers() {
    let temp = tempdir();
    install_default_hermes_fixture(&temp, "setup should inventory hermes");

    let setup =
        json_output(ctx(&temp).args(["setup", "--wait", "--format=json", "--progress", "none"]));
    assert_eq!(setup["mode"], "ready", "{setup:#}");
    assert_eq!(setup["lexical"]["certified_sources"], 1, "{setup:#}");
    assert!(
        setup["lexical"]["indexed_documents"]
            .as_u64()
            .is_some_and(|count| count >= 2),
        "{setup:#}"
    );

    let generation = setup["lexical"]["generation_id"].as_str().unwrap();
    let status = wait_for_core_generation(&temp, generation);
    assert_eq!(status["lexical"]["status"], "ready", "{status:#}");
    let (sessions, events) = provider_core_counts(&data_root(&temp), "hermes");
    assert_eq!(sessions, 1);
    assert!(events >= 2);
    assert!(!data_root(&temp).join("relational.sqlite").exists());
}

#[test]
fn clean_multisource_setup_preserves_core_identity() {
    let temp = tempdir();
    write_large_codex_setup_sessions(&temp, 40, 4, 4 * 1024);
    write_large_hermes_setup_db(&temp, 130, 8 * 1024);
    let _daemon = start_full_source_refresh_daemon(&temp);
    let setup = ready_setup(&temp);
    let generation = setup["lexical"]["generation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let status = wait_for_core_generation(&temp, &generation);

    assert_eq!(setup["schema_version"], 2, "{setup:#}");
    assert_eq!(setup["mode"], "ready", "{setup:#}");
    assert_eq!(status["lexical"]["generation_id"], generation, "{status:#}");
    assert!(status.get("relational").is_none(), "{status:#}");
    let core_counts = (
        provider_core_counts(&data_root(&temp), "codex"),
        provider_core_counts(&data_root(&temp), "hermes"),
    );
    assert!((core_counts.0).1 > 0);
    assert!((core_counts.1).1 > 0);
    assert!(!data_root(&temp).join("relational.sqlite").exists());

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
    wait_for_core_generation(&temp, &generation);
    assert_eq!(
        (
            provider_core_counts(&data_root(&temp), "codex"),
            provider_core_counts(&data_root(&temp), "hermes"),
        ),
        core_counts
    );
    assert!(!data_root(&temp).join("relational.sqlite").exists());
}
