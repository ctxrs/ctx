use super::*;

#[test]
fn setup_wait_survives_more_than_five_seconds_of_authenticated_daemon_starting() {
    let temp = daemon_test_root();
    let root = data_root(&temp);
    fs::create_dir_all(&root).unwrap();
    let block = root.join(".block-daemon-main-before-ready-for-test");
    let blocked = root.join(".daemon-main-blocked-before-ready-for-test");
    fs::write(&block, b"block\n").unwrap();
    let blocked_for_release = blocked.clone();
    let release = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !blocked_for_release.exists() {
            assert!(
                Instant::now() < deadline,
                "daemon did not reach the pre-readiness fence"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(5_500));
        fs::remove_file(block).unwrap();
    });

    let started = Instant::now();
    let output = ctx(&temp)
        .args(["setup", "--wait", "--format=json", "--progress", "none"])
        .output()
        .unwrap();
    release.join().unwrap();
    let elapsed = started.elapsed();
    assert!(
        output.status.success(),
        "setup failed after {elapsed:?}: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(elapsed > Duration::from_secs(5), "elapsed={elapsed:?}");
    assert!(!blocked.exists(), "daemon did not clear its test fence");

    let setup: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(setup["mode"], "ready", "{setup:#}");
    assert_eq!(setup["refresh_request"]["mode"], "wait", "{setup:#}");
    assert_eq!(
        setup["refresh_request"]["published_generation"], setup["lexical"]["generation_id"],
        "{setup:#}"
    );
    assert_eq!(setup["daemon"]["running"], true, "{setup:#}");
}

#[test]
fn installer_style_setup_succeeds_with_a_genuinely_empty_source_catalog() {
    let temp = daemon_test_root();

    let setup = json_output(ctx(&temp).args(["setup", "--format=json", "--progress", "none"]));
    assert_eq!(setup["schema_version"], 2, "{setup:#}");
    assert_empty_catalog_default_background_setup(&setup);

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(status["lexical"]["status"], "ready", "{status:#}");
    assert_eq!(status["lexical"]["certified_sources"], 0, "{status:#}");
    assert_eq!(status["lexical"]["indexed_documents"], 0, "{status:#}");
}

#[cfg(target_os = "linux")]
#[test]
fn operational_systemd_installer_style_setup_verifies_an_empty_noop_core() {
    let temp = tempdir();
    let binary = copied_ctx_binary(&temp);
    install_managed_test_marker(&binary);
    let managed_root = temp.path().join(".ctx");
    let (manager_bin, _daemon_guard) =
        fake_operational_systemd_user_manager(&temp, &binary, &managed_root, false);
    let path = std::env::var_os("PATH")
        .map(|path| {
            let mut paths = vec![manager_bin.clone()];
            paths.extend(std::env::split_paths(&path));
            std::env::join_paths(paths).unwrap()
        })
        .unwrap_or_else(|| manager_bin.as_os_str().to_os_string());

    let mut setup_command = ctx_from_binary(&temp, &binary);
    setup_command
        .env("CTX_DATA_ROOT", &managed_root)
        .env("CTX_DAEMON_AUTOSTART_EXE", &binary)
        .env("CTX_HOSTED_INSTALLER_SETUP", "1")
        .env("CTX_SEARCH_SEMANTIC", "false")
        .env("CTX_UPGRADE_CHANNEL", "staging")
        .env("PATH", &path)
        .env_remove("CTX_DAEMON_AUTOSTART_OFF");
    let output = setup_command
        .args(["setup", "--format=json", "--progress", "none"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "setup stderr:\n{}\ndaemon stdout:\n{}\ndaemon stderr:\n{}",
        String::from_utf8_lossy(&output.stderr),
        fs::read_to_string(temp.path().join("fake-systemd-daemon.stdout")).unwrap_or_default(),
        fs::read_to_string(temp.path().join("fake-systemd-daemon.stderr")).unwrap_or_default(),
    );
    let setup: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(setup["mode"], "ready", "{setup:#}");
    assert_eq!(setup["daemon_autostart"]["status"], "verified", "{setup:#}");
    assert_eq!(setup["daemon_autostart"]["persistent"], true, "{setup:#}");
    assert_eq!(
        setup["daemon"]["supervisor"]["status"], "installed",
        "{setup:#}"
    );
    assert_empty_catalog_default_background_setup(&setup);

    let mut ordinary_status = ctx_from_binary(&temp, &binary);
    ordinary_status
        .env("CTX_DATA_ROOT", &managed_root)
        .env("PATH", &path)
        .env_remove("CTX_DAEMON_AUTOSTART_OFF")
        .env_remove("CTX_HOSTED_INSTALLER_SETUP")
        .env_remove("CTX_SEARCH_SEMANTIC")
        .env_remove("CTX_UPGRADE_CHANNEL");
    let status = json_output(ordinary_status.args(["status", "--format=json"]));
    assert_eq!(
        status["daemon"]["supervisor"]["status"], "installed",
        "{status:#}"
    );
    assert_eq!(
        status["daemon"]["supervisor"]["registration_verified"], true,
        "{status:#}"
    );
    assert_eq!(
        status["daemon"]["supervisor"]["environment_snapshot"]["restart_required"], false,
        "{status:#}"
    );

    let mut disable = ctx_from_binary(&temp, &binary);
    disable
        .env("CTX_DATA_ROOT", &managed_root)
        .env("PATH", &path)
        .env_remove("CTX_DAEMON_AUTOSTART_OFF");
    disable.args(["daemon", "disable"]).assert().success();
}

#[cfg(target_os = "linux")]
#[test]
fn hosted_setup_recovers_when_the_new_systemd_service_first_exits_cleanly() {
    let temp = tempdir();
    let binary = copied_ctx_binary(&temp);
    install_managed_test_marker(&binary);
    let managed_root = temp.path().join(".ctx");
    let (manager_bin, _daemon_guard) =
        fake_operational_systemd_user_manager(&temp, &binary, &managed_root, true);
    let path = std::env::var_os("PATH")
        .map(|path| {
            let mut paths = vec![manager_bin.clone()];
            paths.extend(std::env::split_paths(&path));
            std::env::join_paths(paths).unwrap()
        })
        .unwrap_or_else(|| manager_bin.as_os_str().to_os_string());

    let mut setup_command = ctx_from_binary(&temp, &binary);
    setup_command
        .env("CTX_DATA_ROOT", &managed_root)
        .env("CTX_DAEMON_AUTOSTART_EXE", &binary)
        .env("CTX_HOSTED_INSTALLER_SETUP", "1")
        .env("CTX_SEARCH_SEMANTIC", "false")
        .env("CTX_UPGRADE_CHANNEL", "staging")
        .env("PATH", &path)
        .env_remove("CTX_DAEMON_AUTOSTART_OFF");
    let output = setup_command
        .args(["setup", "--format=json", "--progress", "none"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "setup stderr:\n{}\ndaemon stdout:\n{}\ndaemon stderr:\n{}",
        String::from_utf8_lossy(&output.stderr),
        fs::read_to_string(temp.path().join("fake-systemd-daemon.stdout")).unwrap_or_default(),
        fs::read_to_string(temp.path().join("fake-systemd-daemon.stderr")).unwrap_or_default(),
    );
    let setup: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(setup["mode"], "ready", "{setup:#}");
    assert_eq!(setup["daemon_autostart"]["status"], "verified", "{setup:#}");
    assert_eq!(setup["daemon_autostart"]["persistent"], true, "{setup:#}");
    assert_eq!(
        setup["daemon"]["supervisor"]["status"], "installed",
        "{setup:#}"
    );
    assert_empty_catalog_default_background_setup(&setup);
}

#[cfg(target_os = "linux")]
#[test]
fn unavailable_systemd_installer_style_setup_starts_a_persistent_fallback() {
    use std::os::unix::fs::symlink;

    let temp = daemon_test_root();
    let binary = copied_ctx_binary(&temp);
    install_managed_test_marker(&binary);
    let managed_root = temp.path().join(".ctx");
    let manager_bin = temp.path().join("unavailable-systemd-bin");
    fs::create_dir(&manager_bin).unwrap();
    symlink("/bin/false", manager_bin.join("systemctl")).unwrap();
    let path = std::env::var_os("PATH")
        .map(|path| {
            let mut paths = vec![manager_bin.clone()];
            paths.extend(std::env::split_paths(&path));
            std::env::join_paths(paths).unwrap()
        })
        .unwrap_or_else(|| manager_bin.as_os_str().to_os_string());
    let mut setup_command = ctx_from_binary(&temp, &binary);
    setup_command
        .env("CTX_DATA_ROOT", &managed_root)
        .env("CTX_DAEMON_AUTOSTART_EXE", &binary)
        .env("CTX_HOSTED_INSTALLER_SETUP", "1")
        .env("CTX_SEARCH_SEMANTIC", "false")
        .env("PATH", &path)
        .env_remove("CTX_DAEMON_AUTOSTART_OFF");
    let setup = json_output(setup_command.args(["setup", "--format=json", "--progress", "none"]));
    assert_eq!(setup["mode"], "ready", "{setup:#}");
    assert_eq!(setup["daemon_autostart"]["status"], "degraded", "{setup:#}");
    assert_eq!(setup["daemon_autostart"]["persistent"], true, "{setup:#}");
    assert!(
        setup["daemon_autostart"]["limitation"].is_null(),
        "{setup:#}"
    );
    assert_eq!(
        setup["daemon"]["supervisor"]["status"], "manager_unavailable",
        "{setup:#}"
    );
    assert_eq!(setup["refresh_request"]["mode"], "wait", "{setup:#}");
    assert_empty_catalog_default_background_setup(&setup);

    let mut human_command = ctx_from_binary(&temp, &binary);
    human_command
        .env("CTX_DATA_ROOT", &managed_root)
        .env("CTX_DAEMON_AUTOSTART_EXE", &binary)
        .env("CTX_SEARCH_SEMANTIC", "false")
        .env("PATH", &path)
        .env_remove("CTX_DAEMON_AUTOSTART_OFF");
    let human = success_stdout(human_command.args(["setup", "--progress", "none"]));
    assert!(
        human.contains("persistent daemon (automatic restart unavailable)"),
        "{human}"
    );
    assert!(
        !human.contains("Continuous refresh is unavailable"),
        "{human}"
    );
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
    assert!(stderr.contains("ctx status --format json"), "{stderr}");
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
    let temp = daemon_test_root();

    let setup = json_output(ctx(&temp).args(["setup", "--format=json", "--progress", "none"]));
    assert_eq!(setup["schema_version"], 2, "{setup:#}");
    assert!(setup.get("background_indexing").is_none(), "{setup:#}");
    assert_eq!(setup["daemon_autostart"]["status"], "degraded", "{setup:#}");
    assert_eq!(setup["daemon_autostart"]["requested"], true, "{setup:#}");
    assert_eq!(
        setup["daemon_autostart"]["reason"], "native_supervisor_unavailable",
        "{setup:#}"
    );
    assert_eq!(setup["daemon_autostart"]["persistent"], true, "{setup:#}");
    assert!(
        setup["daemon_autostart"]["limitation"].is_null(),
        "{setup:#}"
    );
    assert_eq!(
        setup["daemon_autostart"]["supervisor"]["status"], "fallback",
        "{setup:#}"
    );
    let pid = setup["daemon_autostart"]["pid"].as_u64().unwrap() as u32;
    assert_eq!(setup["daemon"]["running"], true, "{setup:#}");
    assert_eq!(setup["daemon"]["pid"], pid, "{setup:#}");
    assert_daemon_process_running_with_status(&temp, pid);

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
    let temp = daemon_test_root();
    let binary = copied_ctx_binary(&temp);

    let output = ctx_from_binary(&temp, &binary)
        .args(["setup", "--progress", "none"])
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
        let normalized = stdout.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            normalized.contains("Indexed 0 sessions; 0 messages; 0 tool calls; 0 B processed"),
            "{stdout}"
        );
        assert!(!stdout.contains("indexed sources"), "{stdout}");
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

    let lock: Value =
        serde_json::from_slice(&fs::read(data_root(&temp).join("daemon/daemon.lock")).unwrap())
            .unwrap();
    assert_eq!(lock["pid"], pid, "{lock:#}");
    assert_eq!(lock["released"], false, "{lock:#}");
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
        sessions.join("codex-malformed-session.jsonl"),
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
    assert_eq!(status["refresh"]["status"], "ready", "{status:#}");
    assert_eq!(
        status["refresh"]["current"]["current_rejected_records"], 1,
        "{status:#}"
    );

    let search = json_output(ctx_from_binary(&temp, &binary).args([
        "search",
        "after malformed",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert!(
        !search["results"].as_array().unwrap().is_empty(),
        "{search:#}"
    );

    let doctor = json_output(ctx_from_binary(&temp, &binary).args(["doctor", "--format=json"]));
    assert_eq!(doctor["ok"], true, "{doctor:#}");
    assert_eq!(doctor["findings"], json!([]), "{doctor:#}");
    assert_eq!(
        doctor["source_epoch"]["refresh"]["status"], "ready",
        "{doctor:#}"
    );
    assert_eq!(
        doctor["source_epoch"]["refresh"]["current"]["current_rejected_records"], 1,
        "{doctor:#}"
    );
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
        sessions.join("codex-malformed-session.jsonl"),
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
    let initial_refresh_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = json_output(ctx_from_binary(&temp, &binary).args(["status", "--format=json"]));
        if status["refresh"]["status"] == "ready"
            && status["refresh"]["published_generation"].is_string()
        {
            break;
        }
        assert!(
            Instant::now() < initial_refresh_deadline,
            "timed out waiting for the daemon's initial Core refresh: {status:#}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
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
    let source = &import["sources"][0];
    assert_eq!(source["status"], "published", "{import:#}");
    let request_id = source["daemon_request_id"].as_str().unwrap();
    assert!(!request_id.is_empty(), "{import:#}");
    assert_eq!(
        source["daemon_request_metadata"]["owner"], "daemon",
        "{import:#}",
    );
    let generation = source["published_generation"].as_str().unwrap();
    assert!(!generation.is_empty(), "{import:#}");

    let status = json_output(ctx_from_binary(&temp, &binary).args(["status", "--format=json"]));
    assert_eq!(status["lexical"]["generation_id"], generation, "{status:#}");
    assert_eq!(status["lexical"]["status"], "ready", "{status:#}");
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
    assert_eq!(
        search["retrieval"]["generation_id"], generation,
        "{search:#}"
    );

    drop(core_daemon);
}

#[test]
fn human_wait_setup_starts_daemon_after_foreground_import() {
    let temp = daemon_test_root();
    write_codex_setup_session(&temp);
    let binary = copied_ctx_binary(&temp);

    let output = ctx_from_binary(&temp, &binary)
        .args(["setup", "--wait", "--progress", "none"])
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

    let lock: Value =
        serde_json::from_slice(&fs::read(data_root(&temp).join("daemon/daemon.lock")).unwrap())
            .unwrap();
    assert_eq!(lock["pid"], pid, "{lock:#}");
    assert_eq!(lock["released"], false, "{lock:#}");
}

#[test]
fn setup_inventories_and_imports_claude_sources_by_default() {
    let temp = daemon_test_root();
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
fn clean_multisource_setup_imports_hermes_and_preserves_source_bytes() {
    let temp = tempdir();
    write_large_codex_setup_sessions(&temp, 40, 4, 4 * 1024);
    write_large_hermes_setup_db(&temp, 130, 8 * 1024);
    let hermes_db = temp.path().join(".hermes/state.db");
    let hermes_before = fs::read(&hermes_db).unwrap();
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
    assert_eq!(fs::read(&hermes_db).unwrap(), hermes_before);
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
    assert_eq!(fs::read(&hermes_db).unwrap(), hermes_before);
}
