use super::{
    assert_daemon_process_running, assert_no_daemon_autostart_mutation, ctx, support::*,
    wait_for_daemon_status, write_active_daemon_upgrade_handoff, write_codex_setup_session,
};
use std::{
    io::Read,
    process::{Child, Command as StdCommand, Stdio},
};

#[path = "import_orchestration/relocation.rs"]
mod relocation;

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
        .args(["daemon", "run", "--force", "--loop-interval-seconds", "600"])
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
    let daemon_pid = child.id();
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
                && status["daemon"]["pid"] == daemon_pid
                && status["daemon"]["core_refresh_endpoint"]["available"] == true
                && status["daemon"]["core_refresh_endpoint"]["owner_pid"] == daemon_pid
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

fn wait_for_core_generation(temp: &TempDir, generation: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = json_output(ctx(temp).args(["status", "--format=json"]));
        if status["history_epoch"]["status"] == "ready"
            && status["lexical"]["status"] == "ready"
            && status["lexical"]["generation_id"] == generation
        {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for Core generation {generation}: {status:#}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn wait_for_initial_source_refresh(temp: &TempDir) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = json_output(ctx(temp).args(["status", "--format=json"]));
        if status["refresh"]["status"] == "ready"
            && status["refresh"]["published_generation"].is_string()
        {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the daemon's initial source refresh: {status:#}"
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

#[cfg(unix)]
fn ctx_with_umask(temp: &TempDir, mask: &str) -> assert_cmd::Command {
    let prepared = ctx(temp);
    let binary = prepared.get_program().to_owned();
    let mut command = assert_cmd::Command::new("sh");
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
        .args(["-c", "umask \"$1\"; shift; exec \"$@\"", "ctx-umask", mask])
        .arg(binary);
    command
}

#[cfg(unix)]
fn unix_mode(path: impl AsRef<Path>) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;

    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[cfg(unix)]
fn set_unix_mode(path: impl AsRef<Path>, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

#[cfg(unix)]
fn assert_private_index_control_state(temp: &TempDir) {
    let search_root = data_root(temp).join("search");
    let lexical_root = search_root.join("lexical");
    for directory in [
        data_root(temp),
        search_root,
        lexical_root.clone(),
        lexical_root.join("ctx-generations"),
        lexical_root.join("index-generations"),
    ] {
        assert_eq!(unix_mode(&directory), 0o700, "{}", directory.display());
    }
    for file in [
        lexical_root.join(".ctx-generation-writer.lock"),
        lexical_root.join("active-generation.json"),
    ] {
        assert_eq!(unix_mode(&file), 0o600, "{}", file.display());
    }
    for entry in fs::read_dir(lexical_root.join("ctx-generations")).unwrap() {
        let path = entry.unwrap().path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            assert_eq!(unix_mode(&path), 0o600, "{}", path.display());
        }
    }
}

#[cfg(unix)]
#[test]
fn manual_first_import_is_umask_independent_and_publishes_private_control_state() {
    let temp = tempdir();
    write_codex_setup_session(&temp);

    ctx_with_umask(&temp, "0002")
        .args(["daemon", "disable"])
        .assert()
        .success();
    let imported = json_output(
        ctx_with_umask(&temp, "0002")
            .args(["import", "--all", "--format=json", "--progress", "none"])
            .timeout(Duration::from_secs(20)),
    );
    assert_eq!(imported["outcome"], "success", "{imported:#}");

    assert_private_index_control_state(&temp);
}

#[cfg(unix)]
#[test]
fn manual_exact_noop_repairs_legacy_permissive_control_state() {
    let temp = tempdir();
    write_codex_setup_session(&temp);

    ctx_with_umask(&temp, "0077")
        .args(["daemon", "disable"])
        .assert()
        .success();
    let initial = json_output(
        ctx_with_umask(&temp, "0077")
            .args(["import", "--all", "--format=json", "--progress", "none"])
            .timeout(Duration::from_secs(20)),
    );
    assert_eq!(initial["outcome"], "success", "{initial:#}");

    let search_root = data_root(&temp).join("search");
    let lexical_root = search_root.join("lexical");
    for directory in [
        search_root,
        lexical_root.clone(),
        lexical_root.join("ctx-generations"),
        lexical_root.join("index-generations"),
    ] {
        set_unix_mode(directory, 0o775);
    }
    for file in [
        lexical_root.join(".ctx-generation-writer.lock"),
        lexical_root.join("active-generation.json"),
    ] {
        set_unix_mode(file, 0o664);
    }
    for entry in fs::read_dir(lexical_root.join("ctx-generations")).unwrap() {
        let path = entry.unwrap().path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            set_unix_mode(path, 0o664);
        }
    }

    let repaired = json_output(
        ctx_with_umask(&temp, "0002")
            .args(["import", "--all", "--format=json", "--progress", "none"])
            .timeout(Duration::from_secs(20)),
    );
    assert_eq!(repaired["outcome"], "success", "{repaired:#}");
    assert_eq!(
        published_generation(&repaired),
        published_generation(&initial)
    );
    assert_private_index_control_state(&temp);
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
        .stderr(predicate::str::contains("--partial is deprecated"))
        .stderr(predicate::str::contains(
            "tolerant import is always enabled",
        ))
        .stderr(predicate::str::contains("no foreground writer was started"));
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
    assert!(
        !stderr.contains("Refreshing the provider-authoritative source index"),
        "{stderr}"
    );
    let events = stderr
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        events.iter().filter(|event| event["done"] == true).count(),
        1
    );
}

#[test]
fn provider_import_uses_named_root_when_automatic_discovery_is_disabled() {
    let temp = tempdir();
    let named_root = temp.path().join("work-codex");
    copy_dir_all(
        Path::new(&provider_history_fixture("codex-sessions")),
        &named_root.join("sessions"),
    );
    fs::create_dir_all(data_root(&temp)).unwrap();
    fs::write(
        data_root(&temp).join("config.toml"),
        format!(
            "[sources]\nautomatic = false\n\n[sources.roots.work]\nprovider = \"codex\"\npath = {:?}\n",
            named_root.display().to_string(),
        ),
    )
    .unwrap();

    let report = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--format=json",
        "--progress",
        "none",
    ]));

    assert_eq!(report["outcome"], "success", "{report:#}");
    assert_eq!(report["sources"][0]["status"], "published", "{report:#}");
    assert_eq!(report["sources"][0]["successful_routes"], 1, "{report:#}");
}

#[test]
fn warm_no_op_import_progress_keeps_per_run_bytes_unknown() {
    let temp = tempdir();
    copy_dir_all(
        Path::new(&provider_history_fixture("codex-sessions")),
        &temp.path().join(".codex").join("sessions"),
    );
    ctx(&temp)
        .args(["import", "--all", "--format=json", "--progress", "none"])
        .assert()
        .success();
    let output = ctx(&temp)
        .args(["import", "--all", "--format=json", "--progress", "json"])
        .assert()
        .success()
        .get_output()
        .clone();
    let terminal = String::from_utf8(output.stderr)
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|line| line["done"] == true && line["phase"] == "published")
        .expect("terminal JSON progress event");
    assert_eq!(terminal["completed_bytes"], 0);
    assert_eq!(terminal["total_bytes"], 0);
    assert_eq!(terminal["percent"], 0.0);
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
    assert!(stderr.contains("\nHistory refresh complete\n"), "{stderr}");
    assert_eq!(
        stderr.matches("History refresh complete\n").count(),
        1,
        "{stderr}"
    );
    assert!(
        stderr
            .lines()
            .last()
            .is_some_and(|line| line.split_whitespace().eq(["Remaining", "complete"])),
        "{stderr}"
    );
    assert!(
        !stderr.contains("Published the source for indexing"),
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
fn explicit_import_reuses_running_daemon_when_autostart_is_disabled() {
    let temp = tempdir();
    let _daemon = start_full_source_refresh_daemon(&temp);
    let fixture = temp.path().join("autostart-disabled-explicit.jsonl");
    write_valid_explicit_custom_source(&fixture, "running daemon explicit overlay oracle");

    let imported = json_output(
        ctx(&temp)
            .args([
                "import",
                "--input-format",
                "ctx-history-jsonl-v2",
                "--path",
                fixture.to_str().unwrap(),
                "--format=json",
                "--progress",
                "none",
            ])
            .env("CTX_DAEMON_AUTOSTART_OFF", "1"),
    );

    assert_eq!(imported["outcome"], "success", "{imported:#}");
    assert_eq!(
        imported["sources"][0]["status"], "published",
        "{imported:#}"
    );
    assert_eq!(
        imported["sources"][0]["daemon_request_metadata"]["owner"], "daemon",
        "{imported:#}"
    );
}

#[test]
fn manual_indexing_import_uses_a_finite_worker_and_background_search_stays_inert() {
    let temp = tempdir();
    fs::create_dir_all(data_root(&temp)).unwrap();
    let config = "[indexing]\nmode = \"manual\"\n\n[search]\nsemantic = false\n";
    fs::write(data_root(&temp).join("config.toml"), config).unwrap();
    let fixture = temp.path().join("manual-finite-worker.jsonl");
    let query = "manual finite worker publication oracle";
    write_valid_explicit_custom_source(&fixture, query);

    let imported = json_output(
        ctx(&temp)
            .args([
                "import",
                "--input-format",
                "ctx-history-jsonl-v2",
                "--path",
                fixture.to_str().unwrap(),
                "--format=json",
                "--progress",
                "none",
            ])
            .timeout(Duration::from_secs(20)),
    );
    assert_eq!(imported["outcome"], "success", "{imported:#}");
    assert_eq!(
        imported["sources"][0]["status"], "published",
        "{imported:#}"
    );

    let stopped = wait_for_daemon_status(&temp, "disabled", false, "import");
    assert_eq!(
        stopped["daemon"]["config_reload"]["applied"]["daemon_mode"], "source-refresh-only",
        "{stopped:#}"
    );
    let daemon_root = data_root(&temp).join("daemon");
    assert!(!daemon_root.join("source-refresh-endpoint.json").exists());
    assert!(!daemon_root.join("wakeup.json").exists());
    assert!(!daemon_root.join("supervisor.json").exists());
    assert!(!daemon_root.join("semantic-index.json").exists());
    assert_eq!(
        fs::read_to_string(data_root(&temp).join("config.toml")).unwrap(),
        config
    );

    let status_before = fs::read(daemon_root.join("status.json")).unwrap();
    let native_query = "manual wait discovers newly arrived native history";
    write_codex_message_fixture(
        &temp.path().join(".codex/sessions/2026/08/17"),
        "019fcaaa-0000-7000-8000-000000000817",
        native_query,
    );
    let search = json_output(ctx(&temp).args([
        "search",
        native_query,
        "--provider=codex",
        "--refresh=background",
        "--format=json",
    ]));
    assert!(
        search["results"].as_array().unwrap().is_empty(),
        "{search:#}"
    );
    assert_eq!(
        fs::read(daemon_root.join("status.json")).unwrap(),
        status_before,
        "manual background search must not start or wake a worker"
    );
    assert!(!daemon_root.join("source-refresh-endpoint.json").exists());

    let off = json_output(ctx(&temp).args([
        "search",
        native_query,
        "--provider=codex",
        "--refresh=off",
        "--format=json",
    ]));
    assert!(off["results"].as_array().unwrap().is_empty(), "{off:#}");
    assert_eq!(
        fs::read(daemon_root.join("status.json")).unwrap(),
        status_before,
        "refresh off must not start or wake a worker"
    );

    let waited = json_output(ctx(&temp).args([
        "search",
        native_query,
        "--provider=codex",
        "--refresh=wait",
        "--format=json",
    ]));
    assert!(
        !waited["results"].as_array().unwrap().is_empty(),
        "{waited:#}"
    );
    let stopped = wait_for_daemon_status(&temp, "disabled", false, "search");
    assert_eq!(stopped["daemon"]["running"], false, "{stopped:#}");
    assert!(!daemon_root.join("source-refresh-endpoint.json").exists());
    assert!(!daemon_root.join("supervisor.json").exists());
}

#[test]
fn manual_wait_recovers_when_its_finite_worker_retires_before_admission() {
    let temp = tempdir();
    fs::create_dir_all(data_root(&temp)).unwrap();
    fs::write(
        data_root(&temp).join("config.toml"),
        "[indexing]\nmode = \"manual\"\n\n[search]\nsemantic = false\n",
    )
    .unwrap();
    let query = "finite retirement handoff recovery oracle";
    write_codex_message_fixture(
        &temp.path().join(".codex/sessions/2026/08/17"),
        "019fcaaa-0000-7000-8000-000000000818",
        query,
    );

    let gate = data_root(&temp).join(".block-source-refresh-after-availability-for-test");
    let blocked = data_root(&temp).join(".source-refresh-blocked-after-availability-for-test");
    fs::write(&gate, b"block\n").unwrap();
    let prepared = ctx(&temp);
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
            "search",
            query,
            "--provider=codex",
            "--refresh=wait",
            "--format=json",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut search = SourceRefreshDaemon {
        child: Some(command.spawn().expect("start blocked manual wait")),
    };

    let marker_deadline = Instant::now() + Duration::from_secs(15);
    while !blocked.exists() {
        if let Some(status) = search.child.as_mut().unwrap().try_wait().unwrap() {
            panic!("manual wait exited before its availability gate: {status}");
        }
        assert!(
            Instant::now() < marker_deadline,
            "manual wait did not reach its post-availability gate"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    let stopped = wait_for_daemon_status(&temp, "disabled", false, "search");
    assert_eq!(stopped["daemon"]["running"], false, "{stopped:#}");
    fs::remove_file(&gate).unwrap();

    let exit_deadline = Instant::now() + Duration::from_secs(25);
    let status = loop {
        if let Some(status) = search.child.as_mut().unwrap().try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < exit_deadline,
            "manual wait did not recover after finite-worker retirement"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let output = search.child.take().unwrap().wait_with_output().unwrap();
    assert_eq!(output.status, status);
    assert!(
        status.success(),
        "manual wait recovery failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let search: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        !search["results"].as_array().unwrap().is_empty(),
        "{search:#}"
    );
    let stopped = wait_for_daemon_status(&temp, "disabled", false, "search");
    assert_eq!(stopped["daemon"]["running"], false, "{stopped:#}");
    assert!(!data_root(&temp)
        .join("daemon/source-refresh-endpoint.json")
        .exists());
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
    let temp = daemon_test_root();
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

    let lock: Value =
        serde_json::from_slice(&fs::read(data_root(&temp).join("daemon/daemon.lock")).unwrap())
            .unwrap();
    assert_eq!(lock["pid"], pid, "{lock:#}");
    assert_eq!(lock["released"], false, "{lock:#}");
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
        "ctx-history-jsonl-v2",
        "--path",
        &fixture,
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(first["totals"]["current_indexed_documents"], 2);
    assert_eq!(first["totals"]["current_rejected_records"], 0);
    assert_eq!(first["sources"][0]["provider"], "custom");
    assert_eq!(first["sources"][0]["source_format"], "ctx_history_jsonl_v2");

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
    assert_eq!(search["results"][0]["agent_scope"], "primary");
    assert_eq!(search["results"][0]["provider_session_id"], "demo-session");

    let second = json_output(ctx(&temp).args([
        "import",
        "--input-format",
        "ctx-history-jsonl-v2",
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
fn one_event_native_and_explicit_imports_publish_core_generations() {
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
    let native_status = wait_for_core_generation(&native, &native_generation);
    assert_eq!(native_status["lexical"]["indexed_documents"], 1);
    assert_eq!(
        provider_core_counts(&data_root(&native), "openhands"),
        (1, 1)
    );
    assert!(data_root(&native)
        .join("search/lexical/active-generation.json")
        .is_file());
    assert!(!data_root(&native).join("relational.sqlite").exists());
    let native_search = json_output(ctx(&native).args([
        "search",
        "one event must publish",
        "--provider",
        "openhands",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(native_search["retrieval"]["index"], "core");
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
            "schema_version": "ctx-history-jsonl-v2"
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
            "provider_session_id": "one-event-session",
            "started_at": "2026-07-26T12:00:00Z",
            "agent_scope": "primary",
            "status": "completed"
        }),
        json!({
            "record_type": "event",
            "source_id": "one-event-source",
            "provider_session_id": "one-event-session",
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
        "ctx-history-jsonl-v2",
        "--path",
        fixture.to_str().unwrap(),
        "--no-daemon",
        "--format=json",
        "--progress",
        "none",
    ]));
    let explicit_generation = published_generation(&explicit_import);
    assert_eq!(explicit_import["sources"][0]["status"], "published");
    assert_eq!(
        explicit_import["sources"][0]["daemon_request_metadata"]["owner"],
        "daemon"
    );
    let explicit_status = wait_for_core_generation(&explicit, &explicit_generation);
    assert_eq!(explicit_status["lexical"]["indexed_documents"], 1);
    assert_eq!(
        provider_core_counts(&data_root(&explicit), "custom"),
        (1, 1)
    );
    assert!(data_root(&explicit)
        .join("search/lexical/active-generation.json")
        .is_file());
    assert!(!data_root(&explicit).join("relational.sqlite").exists());
    let explicit_search = json_output(ctx(&explicit).args([
        "search",
        "explicit one event",
        "--provider",
        "custom",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(explicit_search["retrieval"]["index"], "core");
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
        "ctx-history-jsonl-v2",
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

    let cases: [(&str, &[u8], &str, &str); 4] = [
        (
            "missing",
            b"",
            "missing manifest record for ctx-history-jsonl-v2",
            "invalid capture payload",
        ),
        (
            "released-v1",
            b"{\"record_type\":\"manifest\",\"schema_version\":\"ctx-history-jsonl-v1\"}\n",
            "unsupported custom history schema version `ctx-history-jsonl-v1`",
            "unsupported provider schema",
        ),
        (
            "unsupported",
            b"{\"record_type\":\"manifest\",\"schema_version\":\"ctx-history-jsonl-v999\"}\n",
            "unsupported custom history schema version `ctx-history-jsonl-v999`",
            "unsupported provider schema",
        ),
        (
            "duplicate",
            b"{\"record_type\":\"manifest\",\"schema_version\":\"ctx-history-jsonl-v2\"}\n{\"record_type\":\"manifest\",\"schema_version\":\"ctx-history-jsonl-v2\"}\n",
            "duplicate manifest record at line 2",
            "invalid capture payload",
        ),
    ];
    for (name, bytes, failure_kind, cli_classification) in cases {
        fs::write(&fixture, bytes).unwrap();
        let stderr = failure_stderr(ctx(&temp).args([
            "import",
            "--input-format",
            "ctx-history-jsonl-v2",
            "--path",
            fixture_arg,
            "--no-daemon",
            "--format=json",
            "--progress",
            "none",
        ]));
        assert!(stderr.contains(failure_kind), "{name}: {stderr}");
        assert!(stderr.contains(cli_classification), "{name}: {stderr}");
        let status = json_output(ctx(&temp).args(["status", "--format=json"]));
        assert!(
            matches!(
                status["refresh"]["status"].as_str(),
                Some("pending" | "unavailable")
            ),
            "{name}: {status:#}"
        );
        assert!(
            status["indexed_sources"]
                .as_u64()
                .is_none_or(|count| count == 0),
            "{name}: {status:#}"
        );
        assert!(
            status["indexed_events"]
                .as_u64()
                .is_none_or(|count| count == 0),
            "{name}: {status:#}"
        );
    }

    fs::write(
        &fixture,
        concat!(
            "{\"record_type\":\"manifest\",\"schema_version\":\"ctx-history-jsonl-v2\"}\n",
            "{\"record_type\":\"source\",\"source_id\":\"recovered-source\",\"provider_key\":\"recovered-agent\",\"source_format\":\"recovered-jsonl\"}\n",
            "{\"record_type\":\"session\",\"source_id\":\"recovered-source\",\"provider_session_id\":\"recovered-session\",\"started_at\":\"2026-07-31T12:00:00Z\",\"agent_scope\":\"primary\"}\n",
            "{malformed-json}\n",
            "{\"record_type\":\"event\",\"source_id\":\"recovered-source\",\"provider_session_id\":\"recovered-session\",\"event_index\":0,\"event_type\":\"message\",\"role\":\"user\",\"occurred_at\":\"2026-07-31T12:00:01Z\",\"payload\":{\"text\":\"structural manifest recovery oracle\"}}\n",
        ),
    )
    .unwrap();
    let recovered = json_output(ctx(&temp).args([
        "import",
        "--input-format",
        "ctx-history-jsonl-v2",
        "--path",
        fixture_arg,
        "--no-daemon",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(
        recovered["outcome"], "completed_with_rejections",
        "{recovered:#}"
    );
    assert_eq!(
        recovered["totals"]["current_indexed_documents"], 1,
        "{recovered:#}"
    );
    assert_eq!(
        recovered["totals"]["current_rejected_records"], 1,
        "{recovered:#}"
    );
}

fn write_valid_explicit_custom_source(path: &Path, text: &str) {
    fs::write(
        path,
        format!(
            concat!(
                "{{\"record_type\":\"manifest\",\"schema_version\":\"ctx-history-jsonl-v2\"}}\n",
                "{{\"record_type\":\"source\",\"source_id\":\"explicit-receipt-source\",\"provider_key\":\"explicit-receipt-agent\",\"source_format\":\"explicit-receipt-jsonl\"}}\n",
                "{{\"record_type\":\"session\",\"source_id\":\"explicit-receipt-source\",\"provider_session_id\":\"explicit-receipt-session\",\"started_at\":\"2026-08-01T12:00:00Z\",\"agent_scope\":\"primary\"}}\n",
                "{{\"record_type\":\"event\",\"source_id\":\"explicit-receipt-source\",\"provider_session_id\":\"explicit-receipt-session\",\"event_index\":0,\"event_type\":\"message\",\"role\":\"user\",\"occurred_at\":\"2026-08-01T12:00:01Z\",\"payload\":{{\"text\":{text:?}}}}}\n",
            ),
            text = text,
        ),
    )
    .unwrap();
}

#[test]
fn explicit_import_failure_does_not_refresh_an_unrelated_cold_route() {
    let temp = tempdir();
    let _daemon = start_full_source_refresh_daemon(&temp);
    wait_for_initial_source_refresh(&temp);
    write_codex_setup_session(&temp);
    let fixture = temp.path().join("cold-explicit-failure.jsonl");
    fs::write(&fixture, b"").unwrap();

    let output = ctx(&temp)
        .args([
            "import",
            "--input-format",
            "ctx-history-jsonl-v2",
            "--path",
            fixture.to_str().unwrap(),
            "--no-daemon",
            "--format=json",
            "--progress",
            "json",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(output.stdout.is_empty());
    assert_eq!(provider_core_counts(&data_root(&temp), "codex"), (0, 0));

    let stderr = String::from_utf8(output.stderr).unwrap();
    let terminal_progress = stderr
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|event| event["done"] == true)
        .expect("terminal explicit-import progress event");
    assert_eq!(terminal_progress["operation"], "import", "{stderr}");
    assert_eq!(terminal_progress["phase"], "failed", "{stderr}");
    assert_eq!(
        terminal_progress["structured_outcome"]["code"], "malformed_source",
        "{stderr}"
    );
    assert!(stderr.contains(fixture.to_str().unwrap()), "{stderr}");
}

#[path = "import_orchestration/additional.rs"]
mod additional;
