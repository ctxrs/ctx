mod support;

#[cfg(unix)]
mod unix {
    use std::{
        fs,
        os::unix::{ffi::OsStrExt as _, fs::PermissionsExt as _},
        path::{Path, PathBuf},
        process::{Child, Command as StdCommand, Stdio},
        time::{Duration, Instant},
    };

    use serde_json::Value;

    use super::support::*;

    const FIXTURE_TARGET_VERSION: &str = "9.9.9";
    const FIXTURE_QUIESCENCE_TIMEOUT: Duration = Duration::from_secs(45);

    fn installation_sibling(binary: &Path, suffix: &str) -> PathBuf {
        let name = binary
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("ctx");
        binary.with_file_name(format!(".{name}.{suffix}"))
    }

    fn scheduler_state_path(binary: &Path) -> PathBuf {
        installation_sibling(binary, "upgrade-state.json")
    }

    fn installation_home(binary: &Path) -> &Path {
        binary
            .parent()
            .expect("managed test binary has an installation directory")
    }

    fn installation_registration_root(binary: &Path) -> PathBuf {
        let canonical = fs::canonicalize(binary).unwrap();
        let namespace = sha256_hex(canonical.as_os_str().as_bytes());
        installation_home(binary)
            .join(".ctx")
            .join("daemon-installations")
            .join(namespace)
            .join("daemon-quiescence-acks")
    }

    fn managed_release_env_for_installation<'a>(
        command: &'a mut assert_cmd::Command,
        release: &FakeRelease,
        binary: &Path,
    ) -> &'a mut assert_cmd::Command {
        managed_release_env(command, release, binary).env("HOME", installation_home(binary))
    }

    fn configured_hook_fixture() -> PathBuf {
        let configured = PathBuf::from(
            std::env::var_os("CTX_AUTO_UPGRADE_ACCEPTANCE_FIXTURE")
                .expect("Bazel must provide the auto-upgrade hook fixture"),
        );
        if configured.is_absolute() {
            configured
        } else {
            std::env::current_dir().unwrap().join(configured)
        }
    }

    fn configured_v025_fixture() -> PathBuf {
        let configured = PathBuf::from(
            std::env::var_os("CTX_V025_UPGRADE_FIXTURE")
                .expect("Bazel must provide the v0.25-like upgrade fixture"),
        );
        if configured.is_absolute() {
            configured
        } else {
            std::env::current_dir().unwrap().join(configured)
        }
    }

    fn hermetic_std_command(temp: &tempfile::TempDir, binary: &Path) -> StdCommand {
        let prepared = ctx_from_binary(temp, binary);
        std_command_from_assert(&prepared)
    }

    fn std_command_from_assert(prepared: &assert_cmd::Command) -> StdCommand {
        let mut command = StdCommand::new(prepared.get_program());
        command.args(prepared.get_args());
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
        if let Some(current_dir) = prepared.get_current_dir() {
            command.current_dir(current_dir);
        }
        command
    }

    fn spawn_persistent_daemon(command: &assert_cmd::Command) -> Child {
        std_command_from_assert(command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }

    fn stop_persistent_daemon(child: Child) -> std::process::Output {
        stop_daemon(child.id());
        child.wait_with_output().unwrap()
    }

    fn run_daemon_until(
        description: &str,
        command: &assert_cmd::Command,
        mut condition: impl FnMut() -> bool,
    ) -> std::process::Output {
        let mut child = spawn_persistent_daemon(command);
        let deadline = Instant::now() + Duration::from_secs(45);
        loop {
            if condition() {
                break;
            }
            if child.try_wait().unwrap().is_some() {
                let output = child.wait_with_output().unwrap();
                panic!(
                    "daemon exited before {description}: status={} stderr={}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            if Instant::now() >= deadline {
                let output = stop_persistent_daemon(child);
                panic!(
                    "timed out waiting for {description}: stderr={}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        stop_persistent_daemon(child)
    }

    fn run_daemon_until_shutdown_requested(
        command: &assert_cmd::Command,
        shutdown_request: &Path,
    ) -> std::process::Output {
        let child = spawn_persistent_daemon(command);
        wait_for(
            "test-owned persistent daemon shutdown",
            Duration::from_secs(30),
            || shutdown_request.exists(),
        );
        stop_persistent_daemon(child)
    }

    fn install_v025_fixture(temp: &DaemonTestRoot) -> PathBuf {
        let target = copied_ctx_binary(temp);
        fs::remove_file(&target).unwrap();
        fs::copy(configured_v025_fixture(), &target).unwrap();
        make_file_executable(&target);
        fs::write(
            install_marker_path(&target),
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema_version": 1,
                "manager": "ctx-hosted-installer",
                "install_attempt_id": "ia_v025_fixture_install",
                "install_path": target,
                "platform": test_platform_key().replace('_', "-"),
                "channel": "stable",
                "version": "0.25.0",
                "sha256": sha256_hex(&fs::read(&target).unwrap()),
                "installed_at": "2026-07-30T00:00:00Z",
            }))
            .unwrap(),
        )
        .unwrap();
        target
    }

    fn v1_v025_candidate(temp: &DaemonTestRoot) -> PathBuf {
        let candidate = temp.path().join("v025-next/ctx");
        if candidate.exists() {
            return candidate;
        }
        fs::create_dir_all(candidate.parent().unwrap()).unwrap();
        fs::copy(configured_hook_fixture(), &candidate).unwrap();
        fs::set_permissions(&candidate, fs::Permissions::from_mode(0o755)).unwrap();
        ensure_managed_test_binary_is_bounded(&candidate);
        make_file_executable(&candidate);
        candidate
    }

    fn start_v025_daemon(temp: &DaemonTestRoot, target: &Path) -> Child {
        let root = data_root(temp);
        start_v025_daemon_at_root(temp, target, &root)
    }

    fn start_v025_daemon_at_root(temp: &DaemonTestRoot, target: &Path, root: &Path) -> Child {
        fs::create_dir_all(root).unwrap();
        fs::write(
            root.join("config.toml"),
            "[daemon]\nenabled = true\nmode = \"source-refresh-only\"\n\n[upgrade]\nauto = \"apply\"\n",
        )
        .unwrap();
        let mut command = hermetic_std_command(temp, target);
        let child = command
            .args([
                "--data-root",
                root.to_str().unwrap(),
                "daemon",
                "run",
                "--loop-interval-seconds",
                "2",
                "--json",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        wait_for("v0.25 fixture daemon owner", Duration::from_secs(5), || {
            fs::read(root.join("daemon/daemon.lock"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .is_some_and(|value| {
                    value["pid"].as_u64() == Some(u64::from(child.id()))
                        && value.get("binary_sha256").is_none()
                })
        });
        child
    }

    fn read_v025_daemon_lock(root: &Path) -> Value {
        serde_json::from_slice(&fs::read(root.join("daemon/daemon.lock")).unwrap()).unwrap()
    }

    fn write_v025_daemon_lock(root: &Path, value: &Value) {
        let mut bytes = serde_json::to_vec_pretty(value).unwrap();
        bytes.push(b'\n');
        fs::write(root.join("daemon/daemon.lock"), bytes).unwrap();
    }

    fn stop_v025_daemon(child: &mut Child) {
        if process_is_running(child.id()) {
            stop_daemon(child.id());
        }
        let status = child.wait().unwrap();
        assert!(!status.success());
    }

    fn run_v025_upgrade(
        temp: &DaemonTestRoot,
        target: &Path,
        abort_after_probe: bool,
    ) -> std::process::Output {
        let candidate = v1_v025_candidate(temp);
        let root = data_root(temp);
        let mut command = hermetic_std_command(temp, target);
        command
            .env_remove("CTX_DATA_ROOT")
            .env("CTX_UPGRADE_BACKGROUND_CHILD", "1")
            .args(["--data-root", root.to_str().unwrap(), "upgrade"])
            .arg("--candidate")
            .arg(candidate);
        if abort_after_probe {
            command.env("CTX_V025_ABORT_AFTER_PROBE_FOR_TESTS", "1");
            command.env("CTX_LEGACY_UPGRADE_HELPER_TIMEOUT_MS_FOR_TESTS", "300");
        }
        command.output().unwrap()
    }

    fn v025_staged_binaries(target: &Path) -> Vec<PathBuf> {
        let mut staged = fs::read_dir(target.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| name.strip_prefix(".ctx-upgrade-"))
                    .and_then(|name| name.strip_suffix(".new"))
                    .is_some_and(|identity| {
                        let mut parts = identity.split('.');
                        parts.next().is_some_and(|pid| pid.parse::<u32>().is_ok())
                            && parts
                                .next()
                                .is_some_and(|nonce| nonce.parse::<u64>().is_ok())
                            && parts.next().is_none()
                    })
            })
            .collect::<Vec<_>>();
        staged.sort();
        staged
    }

    fn managed_hook_candidate(temp: &tempfile::TempDir, install_attempt_id: &str) -> PathBuf {
        managed_candidate_from_binary(temp, &configured_hook_fixture(), install_attempt_id)
    }

    fn managed_bound_hook_candidate(temp: &DaemonTestRoot, install_attempt_id: &str) -> PathBuf {
        managed_bound_candidate_from_binary(temp, &configured_hook_fixture(), install_attempt_id)
    }

    fn managed_daemon(
        temp: &tempfile::TempDir,
        release: &FakeRelease,
        binary: &Path,
    ) -> assert_cmd::Command {
        managed_daemon_with_timing(temp, release, binary, 1)
    }

    fn managed_daemon_with_timing(
        temp: &tempfile::TempDir,
        release: &FakeRelease,
        binary: &Path,
        loop_interval_seconds: u64,
    ) -> assert_cmd::Command {
        let mut command = ctx_from_binary(temp, binary);
        managed_release_env_for_installation(&mut command, release, binary);
        command
            .args(["daemon", "run"])
            .arg("--loop-interval-seconds")
            .arg(loop_interval_seconds.to_string())
            .args([
                "--start-mode",
                "auto",
                "--trigger-command",
                "setup",
                "--format=json",
            ])
            .env("CTX_DAEMON_BACKGROUND_CHILD", "1");
        command
    }

    fn patch_release_artifact_with_next_ctx(
        release: &mut FakeRelease,
        binary: &Path,
        next_version: &str,
    ) {
        let version_output = StdCommand::new(binary).arg("--version").output().unwrap();
        assert!(version_output.status.success(), "{version_output:?}");
        let version_output = String::from_utf8(version_output.stdout).unwrap();
        let current_version = version_output
            .trim()
            .strip_prefix("ctx ")
            .unwrap_or_else(|| panic!("unexpected ctx version output: {version_output:?}"));
        assert_eq!(
            current_version.len(),
            next_version.len(),
            "test binary version replacement must preserve byte width"
        );
        let mut bytes = fs::read(binary).unwrap();
        let current = current_version.as_bytes();
        let next = next_version.as_bytes();
        let mut replacements = 0;
        for offset in 0..=bytes.len().saturating_sub(current.len()) {
            if &bytes[offset..offset + current.len()] == current {
                bytes[offset..offset + next.len()].copy_from_slice(next);
                replacements += 1;
            }
        }
        assert!(
            replacements > 0,
            "candidate binary did not contain its embedded version"
        );

        let artifact = release.metadata.parent().unwrap().join("ctx");
        fs::write(&artifact, &bytes).unwrap();
        make_file_executable(&artifact);
        let artifact_sha = sha256_hex(&bytes);
        rewrite_fake_release_metadata(release, |metadata| {
            metadata
                .replace(
                    &format!("CTX_RELEASE_VERSION={FIXTURE_TARGET_VERSION}\n"),
                    &format!("CTX_RELEASE_VERSION={next_version}\n"),
                )
                .replace(&release.artifact_sha, &artifact_sha)
        });
        release.artifact_sha = artifact_sha;
    }

    fn read_daemon_status(data_root: &Path) -> Option<Value> {
        fs::read(data_root.join("daemon/status.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    }

    fn start_managed_background_daemon(
        temp: &DaemonTestRoot,
        release: &FakeRelease,
        binary: &Path,
    ) {
        let mut command = ctx(temp);
        managed_release_env_for_installation(&mut command, release, binary);
        let output = command
            .env("CTX_DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS", "1")
            .args(["daemon", "enable", "--format=json"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }

    fn wait_for(description: &str, timeout: Duration, mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + timeout;
        while !condition() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {description}"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn running_daemon_pid(data_root: &Path, previous_pid: Option<u32>) -> Option<u32> {
        let status = read_daemon_status(data_root)?;
        let pid = status["pid"]
            .as_u64()
            .and_then(|pid| u32::try_from(pid).ok())?;
        (status["status"] == "running" && previous_pid != Some(pid)).then_some(pid)
    }

    fn process_is_running(pid: u32) -> bool {
        let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }

    fn installation_acknowledgement(
        binary: &Path,
        data_root: &Path,
        attempt_id: &str,
    ) -> Option<Value> {
        let root = installation_registration_root(binary);
        fs::read_dir(root).ok()?.find_map(|entry| {
            let value: Value = serde_json::from_slice(&fs::read(entry.ok()?.path()).ok()?).ok()?;
            (value["status"] == "acknowledged"
                && value["attempt_id"] == attempt_id
                && value["data_root"] == data_root.display().to_string())
            .then_some(value)
        })
    }

    fn stop_daemon(pid: u32) {
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
    }

    fn seed_authoritative_codex_source(home: &Path) {
        let native_session_id = "019fafc5-0000-7000-8000-000000000001";
        let sessions = home.join(".codex/sessions");
        fs::create_dir_all(&sessions).unwrap();
        let metadata = serde_json::json!({
            "timestamp": "2026-07-29T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": native_session_id,
                "timestamp": "2026-07-29T12:00:00Z",
                "cwd": "/tmp/auto-upgrade-acceptance",
                "originator": "codex_cli_rs",
                "cli_version": env!("CARGO_PKG_VERSION"),
                "source": "cli",
                "model_provider": "openai"
            }
        });
        let message = serde_json::json!({
            "timestamp": "2026-07-29T12:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": "authoritative source-backed auto-upgrade fixture"
                }]
            }
        });
        let mut bytes = serde_json::to_vec(&metadata).unwrap();
        bytes.push(b'\n');
        bytes.extend(serde_json::to_vec(&message).unwrap());
        bytes.push(b'\n');
        fs::write(
            sessions.join(format!("rollout-{native_session_id}.jsonl")),
            bytes,
        )
        .unwrap();
    }

    fn initialize_source_backed_epoch(temp: &tempfile::TempDir) {
        seed_authoritative_codex_source(temp.path());
        let data_root = data_root(temp);
        let generation_id = initialize_generation_only_core(&data_root);
        assert!(!generation_id.is_empty());
        assert_source_backed_epoch_remained_fresh(&data_root);
    }

    fn assert_source_backed_epoch_remained_fresh(data_root: &Path) {
        assert!(!data_root.join("relational.sqlite").exists());
        assert!(data_root.join("search/lexical").is_dir());
        assert!(
            !data_root.join("work.sqlite").exists(),
            "v0.26 upgrade fixtures must not open or recreate prior-epoch history storage"
        );
    }

    #[derive(Clone, Copy, Debug)]
    enum RecoveryOwner {
        Automatic,
        Explicit,
    }

    fn acknowledgement_snapshot(binary: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let root = installation_registration_root(binary);
        let Ok(entries) = fs::read_dir(root) else {
            return Vec::new();
        };
        let mut snapshot = entries
            .map(|entry| {
                let path = entry.unwrap().path();
                let bytes = fs::read(&path).unwrap();
                (path, bytes)
            })
            .collect::<Vec<_>>();
        snapshot.sort_by(|left, right| left.0.cmp(&right.0));
        snapshot
    }

    fn replace_current_recovery_attempt(
        journal_path: &Path,
        previous_attempt_id: &str,
        replacement_attempt_id: &str,
    ) {
        let previous_bytes = fs::read(journal_path).unwrap();
        let previous: Value = serde_json::from_slice(&previous_bytes).unwrap();
        let replacement_bytes = String::from_utf8(previous_bytes)
            .unwrap()
            .replace(previous_attempt_id, replacement_attempt_id)
            .into_bytes();
        let replacement: Value = serde_json::from_slice(&replacement_bytes).unwrap();
        for (previous_path, replacement_path) in previous["paths"]
            .as_array()
            .unwrap()
            .iter()
            .zip(replacement["paths"].as_array().unwrap())
        {
            for field in ["staged", "backup"] {
                let previous_path = PathBuf::from(previous_path[field].as_str().unwrap());
                let replacement_path = PathBuf::from(replacement_path[field].as_str().unwrap());
                if previous_path.exists() {
                    fs::rename(previous_path, replacement_path).unwrap();
                }
            }
        }
        fs::write(journal_path, replacement_bytes).unwrap();
    }

    fn assert_original_terminal_scheduler_evidence(binary: &Path, original_attempt_id: &str) {
        let state: Value =
            serde_json::from_slice(&fs::read(scheduler_state_path(binary)).unwrap()).unwrap();
        assert_eq!(state["schema_version"], 1);
        assert_eq!(state["status"], "error");
        assert_eq!(state["attempt_id"], original_attempt_id);
        assert_eq!(state["attempt_source"], "automatic");
        assert_eq!(state["current_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(state["latest_version"], FIXTURE_TARGET_VERSION);
        assert_eq!(state["install_path"], binary.display().to_string());
        assert_eq!(state["managed"], true);
        assert!(
            state["error"]
                .as_str()
                .is_some_and(|error| error.contains("injected applying-state write failure")),
            "{state:#}"
        );
    }

    fn assert_current_replacement_recovery_evidence(
        journal: &[u8],
        replacement_attempt_id: &str,
        binary: &Path,
        data_root: &Path,
    ) {
        let journal: Value = serde_json::from_slice(journal).unwrap();
        assert_eq!(journal["schema_version"], 2);
        assert_eq!(journal["phase"], "prepared");
        assert_eq!(journal["attempt_id"], replacement_attempt_id);
        assert_eq!(journal["install_path"], binary.display().to_string());
        assert_eq!(journal["data_root"], data_root.display().to_string());
        assert!(
            journal["paths"]
                .as_array()
                .is_some_and(|paths| !paths.is_empty()),
            "{journal:#}"
        );
    }

    fn prove_stale_recovery_observation_is_rejected(recovery_owner: RecoveryOwner) {
        let installation = tempdir();
        let release = fake_release(&installation, "9.9.9");
        let binary = managed_hook_candidate(
            &installation,
            &format!("ia_stale_current_recovery_{recovery_owner:?}"),
        );
        let binary_before = fs::read(&binary).unwrap();
        let owner = tempdir();
        let owner_data_root = data_root(&owner);
        initialize_source_backed_epoch(&owner);

        let journal_path = installation_sibling(&binary, "upgrade-install-transaction.json");
        let mut prepare = managed_daemon_with_timing(&owner, &release, &binary, 1);
        prepare
            .env("CTX_DAEMON_AUTOSTART_OFF", "1")
            .env("CTX_UPGRADE_FAIL_APPLYING_STATE_WRITE_FOR_TESTS", "1");
        let prepared = run_daemon_until("prepared recovery journal", &prepare, || {
            journal_path.exists() && scheduler_state_path(&binary).exists()
        });
        assert!(prepared.status.success(), "{prepared:?}");
        let journal: Value = serde_json::from_slice(&fs::read(&journal_path).unwrap()).unwrap();
        assert_eq!(journal["phase"], "prepared");
        let attempt_id = journal["attempt_id"].as_str().unwrap().to_owned();
        assert_original_terminal_scheduler_evidence(&binary, &attempt_id);
        let acknowledgements_before = acknowledgement_snapshot(&binary);
        let pause = installation
            .path()
            .join(format!("stale-current-{recovery_owner:?}"));
        let stale_rejection = installation
            .path()
            .join(format!("stale-current-rejected-{recovery_owner:?}"));
        let shutdown_request = installation
            .path()
            .join(format!("stop-stale-current-{recovery_owner:?}"));

        std::thread::scope(|scope| {
            let claimant = scope.spawn(|| {
                let mut command = match recovery_owner {
                    RecoveryOwner::Automatic => {
                        managed_daemon_with_timing(&owner, &release, &binary, 1)
                    }
                    RecoveryOwner::Explicit => {
                        let mut command = ctx_from_binary(&owner, &binary);
                        managed_release_env_for_installation(&mut command, &release, &binary);
                        command.args(["upgrade", "--format=json"]);
                        command
                    }
                };
                if matches!(recovery_owner, RecoveryOwner::Automatic) {
                    command.env(
                        "CTX_UPGRADE_PAUSE_AFTER_STALE_RECOVERY_FOR_TESTS",
                        &stale_rejection,
                    );
                }
                command
                    .env(
                        "CTX_UPGRADE_PAUSE_AFTER_RECOVERY_DISCOVERY_FOR_TESTS",
                        &pause,
                    )
                    .env("CTX_DAEMON_AUTOSTART_OFF", "1");
                match recovery_owner {
                    RecoveryOwner::Automatic => {
                        run_daemon_until_shutdown_requested(&command, &shutdown_request)
                    }
                    RecoveryOwner::Explicit => command.output().unwrap(),
                }
            });
            wait_for("stale recovery discovery", Duration::from_secs(10), || {
                pause.exists()
            });

            let replacement_attempt_id = format!("{attempt_id}-replacement");
            replace_current_recovery_attempt(&journal_path, &attempt_id, &replacement_attempt_id);
            let replacement_journal = fs::read(&journal_path).unwrap();
            assert_current_replacement_recovery_evidence(
                &replacement_journal,
                &replacement_attempt_id,
                &binary,
                &owner_data_root,
            );
            assert_original_terminal_scheduler_evidence(&binary, &attempt_id);

            fs::write(pause.with_extension("continue"), b"continue\n").unwrap();
            if matches!(recovery_owner, RecoveryOwner::Automatic) {
                wait_for(
                    "automatic stale recovery rejection",
                    Duration::from_secs(10),
                    || stale_rejection.exists(),
                );
                assert_original_terminal_scheduler_evidence(&binary, &attempt_id);
                assert_eq!(fs::read(&journal_path).unwrap(), replacement_journal);
                // The stale observation has now been rejected under the lock.
                // Disable later scheduler ticks so a fresh, valid discovery of
                // the replacement attempt cannot obscure what this claimant did.
                fs::write(
                    owner_data_root.join("config.toml"),
                    "[upgrade]\nauto = \"off\"\n",
                )
                .unwrap();
                fs::write(stale_rejection.with_extension("continue"), b"continue\n").unwrap();
                std::thread::sleep(Duration::from_millis(100));
                fs::write(&shutdown_request, b"stop\n").unwrap();
            }
            let claimant = claimant.join().unwrap();
            match recovery_owner {
                RecoveryOwner::Automatic => assert!(claimant.status.success(), "{claimant:?}"),
                RecoveryOwner::Explicit => {
                    assert!(!claimant.status.success(), "{claimant:?}");
                    assert!(
                        String::from_utf8_lossy(&claimant.stderr)
                            .contains("refusing stale recovery ownership"),
                        "{claimant:?}"
                    );
                }
            }

            assert_original_terminal_scheduler_evidence(&binary, &attempt_id);
            assert_eq!(
                fs::read(&journal_path).unwrap(),
                replacement_journal,
                "{recovery_owner:?} mutated replacement recovery authority"
            );
            assert_eq!(
                fs::read(&binary).unwrap(),
                binary_before,
                "{recovery_owner:?} mutated the installation"
            );
            assert_eq!(
                acknowledgement_snapshot(&binary),
                acknowledgements_before,
                "{recovery_owner:?} began a daemon handoff from stale discovery"
            );
        });
        assert_source_backed_epoch_remained_fresh(&owner_data_root);
    }

    fn prove_recovery_quiescence(recovery_owner: RecoveryOwner) {
        let second = daemon_test_root();
        let release = fake_release(&second, "9.9.9");
        let binary = managed_bound_hook_candidate(
            &second,
            &format!("ia_current_recovery_{recovery_owner:?}"),
        );
        let binary_before = fs::read(&binary).unwrap();
        let owner = tempdir();
        let owner_data_root = data_root(&owner);
        let second_data_root = data_root(&second);
        initialize_source_backed_epoch(&owner);
        initialize_source_backed_epoch(&second);
        fs::write(
            second_data_root.join("config.toml"),
            "[upgrade]\nauto = \"off\"\n",
        )
        .unwrap();

        let journal_path = installation_sibling(&binary, "upgrade-install-transaction.json");
        let mut prepare = managed_daemon_with_timing(&owner, &release, &binary, 1);
        prepare
            .env("CTX_DAEMON_AUTOSTART_OFF", "1")
            .env("CTX_UPGRADE_FAIL_APPLYING_STATE_WRITE_FOR_TESTS", "1");
        let prepared = run_daemon_until("prepared recovery journal", &prepare, || {
            journal_path.exists() && scheduler_state_path(&binary).exists()
        });
        assert!(prepared.status.success(), "{prepared:?}");
        let state: Value =
            serde_json::from_slice(&fs::read(scheduler_state_path(&binary)).unwrap()).unwrap();
        assert_eq!(state["status"], "error");
        let journal: Value = serde_json::from_slice(&fs::read(&journal_path).unwrap()).unwrap();
        assert_eq!(journal["phase"], "prepared");
        let attempt_id = journal["attempt_id"].as_str().unwrap().to_owned();
        let journal_before = fs::read(&journal_path).unwrap();
        if matches!(recovery_owner, RecoveryOwner::Explicit) {
            rewrite_fake_release_metadata(&release, |metadata| {
                metadata.replace(
                    "CTX_RELEASE_VERSION=9.9.9\n",
                    &format!("CTX_RELEASE_VERSION={}\n", env!("CARGO_PKG_VERSION")),
                )
            });
        }
        let pause = second
            .path()
            .join(format!("recovering-current-{recovery_owner:?}"));

        start_managed_background_daemon(&second, &release, &binary);
        wait_for(
            "long-running recovery participant",
            Duration::from_secs(10),
            || running_daemon_pid(&second_data_root, None).is_some(),
        );
        let second_pid = running_daemon_pid(&second_data_root, None).unwrap();

        std::thread::scope(|scope| {
            let owner_handle = scope.spawn(|| {
                let mut command = match recovery_owner {
                    RecoveryOwner::Automatic => {
                        managed_daemon_with_timing(&owner, &release, &binary, 30)
                    }
                    RecoveryOwner::Explicit => {
                        let mut command = ctx_from_binary(&owner, &binary);
                        managed_release_env_for_installation(&mut command, &release, &binary);
                        command.args(["upgrade", "--format=json"]);
                        command
                    }
                };
                command
                    .env("CTX_UPGRADE_INTERVAL_SECONDS", "3600")
                    .env("CTX_UPGRADE_PAUSE_AFTER_QUIESCENCE_FOR_TESTS", &pause)
                    .env_remove("CTX_DAEMON_AUTOSTART_OFF")
                    .output()
                    .unwrap()
            });
            wait_for(
                "owned recovery quiescence",
                FIXTURE_QUIESCENCE_TIMEOUT,
                || pause.exists(),
            );

            assert_eq!(
                fs::read(&binary).unwrap(),
                binary_before,
                "{recovery_owner:?} mutated the executable before quiescence"
            );
            assert_eq!(
                fs::read(&journal_path).unwrap(),
                journal_before,
                "{recovery_owner:?} mutated its journal before quiescence"
            );
            assert!(
                !process_is_running(second_pid),
                "{recovery_owner:?} left the opted-out second root running"
            );
            let recovering: Value =
                serde_json::from_slice(&fs::read(scheduler_state_path(&binary)).unwrap()).unwrap();
            assert_eq!(recovering["status"], "recovering");
            assert_eq!(recovering["attempt_id"], attempt_id);
            let second_ack = installation_acknowledgement(&binary, &second_data_root, &attempt_id)
                .unwrap_or_else(|| {
                    panic!("{recovery_owner:?} has no second-root recovery acknowledgement")
                });
            assert_eq!(second_ack["pid"], second_pid);
            let mut contender = ctx_from_binary(&owner, &binary);
            managed_release_env_for_installation(&mut contender, &release, &binary);
            let contended = contender
                .args(["upgrade", "check", "--format=json"])
                .output()
                .unwrap();
            assert!(
                !contended.status.success(),
                "{recovery_owner:?} allowed a second recovery owner"
            );
            assert!(
                String::from_utf8_lossy(&contended.stderr)
                    .contains("upgrade lock is held for interrupted recovery"),
                "{recovery_owner:?}: {contended:?}"
            );
            assert_eq!(
                fs::read(&journal_path).unwrap(),
                journal_before,
                "{recovery_owner:?} contender mutated recovery state"
            );

            let owner_pid = if matches!(recovery_owner, RecoveryOwner::Automatic) {
                installation_acknowledgement(&binary, &owner_data_root, &attempt_id)
                    .and_then(|value| value["pid"].as_u64())
                    .and_then(|pid| u32::try_from(pid).ok())
            } else {
                None
            };
            fs::write(pause.with_extension("continue"), b"continue\n").unwrap();
            let owner_output = owner_handle.join().unwrap();
            assert!(owner_output.status.success(), "{owner_output:?}");
            assert!(
                !journal_path.exists(),
                "{recovery_owner:?} retained a recovered journal"
            );

            let mut restarted_second = None;
            wait_for(
                "opted-out second root recovery restart replay",
                Duration::from_secs(15),
                || {
                    restarted_second = running_daemon_pid(&second_data_root, Some(second_pid));
                    restarted_second.is_some()
                },
            );
            stop_daemon(restarted_second.unwrap());
            if let Some(owner_pid) = owner_pid {
                let mut restarted_owner = None;
                wait_for(
                    "automatic recovery owner restart",
                    Duration::from_secs(15),
                    || {
                        restarted_owner = running_daemon_pid(&owner_data_root, Some(owner_pid));
                        restarted_owner.is_some()
                    },
                );
                stop_daemon(restarted_owner.unwrap());
            }

            let final_state: Value =
                serde_json::from_slice(&fs::read(scheduler_state_path(&binary)).unwrap()).unwrap();
            match recovery_owner {
                RecoveryOwner::Automatic => {
                    assert_eq!(final_state["attempt_id"], attempt_id);
                    assert_eq!(final_state["status"], "error");
                }
                RecoveryOwner::Explicit => {
                    assert_eq!(final_state["status"], "up_to_date");
                    assert_eq!(final_state["attempt_source"], "manual_apply");
                }
            }
        });
        assert_source_backed_epoch_remained_fresh(&owner_data_root);
        assert_source_backed_epoch_remained_fresh(&second_data_root);
    }

    mod invocation_driver {
        include!("auto_upgrade_acceptance/invocation_driver.rs");
        include!("auto_upgrade_acceptance/invalid_marker.rs");
    }

    #[test]
    fn automatic_indexing_foreground_command_defers_to_daemon_driver() {
        let temp = tempdir();
        let release = fake_release(&temp, "9.9.9");
        let binary = managed_candidate(&temp, "ia_foreground_no_authority");
        let before = fs::read(&binary).unwrap();

        managed_release_env_for_installation(
            ctx_from_binary(&temp, &binary).arg("sources"),
            &release,
            &binary,
        )
        .assert()
        .success();

        assert_eq!(fs::read(&binary).unwrap(), before);
        assert!(!scheduler_state_path(&binary).exists());
    }

    #[test]
    fn daemon_disabled_has_no_automatic_upgrade_side_effects() {
        let temp = tempdir();
        let release = fake_release(&temp, "9.9.9");
        let binary = managed_candidate(&temp, "ia_disabled_daemon");
        let before = fs::read(&binary).unwrap();

        managed_daemon(&temp, &release, &binary)
            .env("CTX_DAEMON_ENABLED", "false")
            .assert()
            .success();

        assert_eq!(fs::read(&binary).unwrap(), before);
        assert!(!scheduler_state_path(&binary).exists());
    }

    #[test]
    fn disabled_daemon_does_not_recover_an_interrupted_install() {
        let temp = tempdir();
        let release = fake_release(&temp, "9.9.9");
        let binary = managed_hook_candidate(&temp, "ia_disabled_recovery");
        let interrupted = managed_release_env_for_installation(
            ctx_from_binary(&temp, &binary).args(["upgrade", "--format=json"]),
            &release,
            &binary,
        )
        .env("CTX_UPGRADE_ABORT_AFTER_BACKUP_FOR_TESTS", "binary")
        .output()
        .unwrap();
        assert!(!interrupted.status.success(), "{interrupted:?}");

        let journal = installation_sibling(&binary, "upgrade-install-transaction.json");
        assert!(journal.exists());
        let binary_before = fs::read(&binary).unwrap();
        let journal_before = fs::read(&journal).unwrap();

        managed_daemon(&temp, &release, &binary)
            .env("CTX_DAEMON_ENABLED", "false")
            .env("CTX_UPGRADE_AUTO", "off")
            .assert()
            .success();

        assert_eq!(fs::read(&binary).unwrap(), binary_before);
        assert_eq!(fs::read(&journal).unwrap(), journal_before);
    }

    #[test]
    fn applying_state_failure_leaves_a_prepublication_journal() {
        let temp = tempdir();
        let release = fake_release(&temp, "9.9.9");
        let binary = managed_hook_candidate(&temp, "ia_journal_before_applying");
        let before = fs::read(&binary).unwrap();

        let journal = installation_sibling(&binary, "upgrade-install-transaction.json");
        let mut daemon = managed_daemon(&temp, &release, &binary);
        daemon
            .env("CTX_DAEMON_AUTOSTART_OFF", "1")
            .env("CTX_UPGRADE_FAIL_APPLYING_STATE_WRITE_FOR_TESTS", "1");
        let output = run_daemon_until("prepublication upgrade failure", &daemon, || {
            journal.exists() && scheduler_state_path(&binary).exists()
        });
        assert!(output.status.success(), "{output:?}");

        let transaction: Value = serde_json::from_slice(&fs::read(&journal).unwrap()).unwrap();
        assert_eq!(transaction["phase"], "prepared");
        assert_eq!(fs::read(&binary).unwrap(), before);
        let state: Value =
            serde_json::from_slice(&fs::read(scheduler_state_path(&binary)).unwrap()).unwrap();
        assert_eq!(state["status"], "error");
    }

    #[test]
    fn persistent_daemon_drives_the_shared_automatic_scheduler() {
        let temp = tempdir();
        let mut release = fake_release(&temp, FIXTURE_TARGET_VERSION);
        let binary = managed_hook_candidate(&temp, "ia_daemon_authority");
        patch_release_artifact_with_next_ctx(&mut release, &binary, FIXTURE_TARGET_VERSION);
        seed_authoritative_codex_source(temp.path());

        managed_daemon(&temp, &release, &binary)
            .env("CTX_DAEMON_AUTOSTART_OFF", "1")
            .assert()
            .success();

        let marker: Value =
            serde_json::from_slice(&fs::read(install_marker_path(&binary)).unwrap()).unwrap();
        let state: Value =
            serde_json::from_slice(&fs::read(scheduler_state_path(&binary)).unwrap()).unwrap();
        assert_eq!(marker["version"], FIXTURE_TARGET_VERSION);
        assert_eq!(state["status"], "applied");
        assert_eq!(state["attempt_source"], "automatic");
        assert!(!temp.path().join("upgrade-state.json").exists());
        assert!(!temp.path().join("upgrade.lock").exists());
    }

    #[test]
    fn v025_automatic_upgrade_quiesces_before_replacement_and_restarts_persistent_owner() {
        let temp = daemon_test_root();
        let target = install_v025_fixture(&temp);
        let old_bytes = fs::read(&target).unwrap();
        let mut old_daemon = start_v025_daemon(&temp, &target);
        let old_pid = old_daemon.id();

        let output = run_v025_upgrade(&temp, &target, false);
        assert!(output.status.success(), "{output:?}");
        assert_ne!(fs::read(&target).unwrap(), old_bytes);
        assert!(!old_daemon.wait().unwrap().success());
        assert!(!process_is_running(old_pid));

        let root = data_root(&temp);
        let mut replacement_pid = None;
        wait_for(
            "identity-verified v1 persistent daemon",
            Duration::from_secs(15),
            || {
                replacement_pid = running_daemon_pid(&root, Some(old_pid));
                replacement_pid.is_some()
            },
        );
        let status = json_output(ctx_from_binary(&temp, &target).args([
            "daemon",
            "status",
            "--format=json",
        ]));
        assert_eq!(status["daemon"]["running"], true);
        assert_eq!(
            status["daemon"]["lock_identity"]["owner_image_matches"],
            true
        );
        assert_eq!(status["daemon"]["pid"], replacement_pid.unwrap());
        let human = ctx_from_binary(&temp, &target)
            .args(["daemon", "status"])
            .output()
            .unwrap();
        assert!(human.status.success(), "{human:?}");
        let human_stdout = String::from_utf8(human.stdout).unwrap();
        assert!(!human_stdout.trim().is_empty());
        assert!(serde_json::from_str::<Value>(&human_stdout).is_err());
        assert!(!root.join("Store").exists());
        stop_daemon(replacement_pid.unwrap());
    }

    #[test]
    fn v025_automatic_upgrade_rejects_unrelated_same_binary_lock_pid() {
        let temp = daemon_test_root();
        let target = install_v025_fixture(&temp);
        let old_bytes = fs::read(&target).unwrap();
        let root = data_root(&temp);
        let unrelated_root = temp.path().join("unrelated-data");
        let mut owner = start_v025_daemon(&temp, &target);
        let mut unrelated = start_v025_daemon_at_root(&temp, &target, &unrelated_root);

        let original_lock = read_v025_daemon_lock(&root);
        let mut spoofed_lock = original_lock.clone();
        spoofed_lock["pid"] = Value::from(unrelated.id());
        write_v025_daemon_lock(&root, &spoofed_lock);

        let output = run_v025_upgrade(&temp, &target, false);
        let owner_survived = process_is_running(owner.id());
        let unrelated_survived = process_is_running(unrelated.id());
        let target_unchanged = fs::read(&target).unwrap() == old_bytes;
        write_v025_daemon_lock(&root, &original_lock);
        stop_v025_daemon(&mut owner);
        stop_v025_daemon(&mut unrelated);

        assert!(!output.status.success(), "{output:?}");
        assert!(owner_survived, "the actual lock owner was signaled");
        assert!(
            unrelated_survived,
            "an unrelated same-binary daemon was signaled"
        );
        assert!(
            target_unchanged,
            "managed executable changed after rejection"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("process data root does not match its held lock"),
            "{output:?}"
        );
    }

    #[test]
    fn v025_automatic_upgrade_rejects_same_bytes_different_inode_owner() {
        use std::os::unix::fs::MetadataExt as _;

        let temp = daemon_test_root();
        let target = install_v025_fixture(&temp);
        let old_bytes = fs::read(&target).unwrap();
        let mut owner = start_v025_daemon(&temp, &target);
        let owner_inode = fs::metadata(format!("/proc/{}/exe", owner.id()))
            .unwrap()
            .ino();
        let replacement = target.with_extension("same-image");
        fs::copy(&target, &replacement).unwrap();
        fs::set_permissions(&replacement, fs::Permissions::from_mode(0o755)).unwrap();
        fs::rename(&replacement, &target).unwrap();
        assert_ne!(fs::metadata(&target).unwrap().ino(), owner_inode);

        let output = run_v025_upgrade(&temp, &target, false);
        let owner_survived = process_is_running(owner.id());
        let target_unchanged = fs::read(&target).unwrap() == old_bytes;
        stop_v025_daemon(&mut owner);

        assert!(!output.status.success(), "{output:?}");
        assert!(owner_survived, "the stale-inode owner was signaled");
        assert!(
            target_unchanged,
            "managed executable changed after rejection"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("executable inode does not match"),
            "{output:?}"
        );
    }

    #[test]
    fn v025_automatic_upgrade_does_not_downgrade_recorded_digest_verification() {
        let temp = daemon_test_root();
        let target = install_v025_fixture(&temp);
        let old_bytes = fs::read(&target).unwrap();
        let root = data_root(&temp);
        let mut owner = start_v025_daemon(&temp, &target);
        let original_lock = read_v025_daemon_lock(&root);
        let mut mismatched_lock = original_lock.clone();
        mismatched_lock["binary_sha256"] = Value::String("0".repeat(64));
        write_v025_daemon_lock(&root, &mismatched_lock);

        let output = run_v025_upgrade(&temp, &target, false);
        let owner_survived = process_is_running(owner.id());
        let target_unchanged = fs::read(&target).unwrap() == old_bytes;
        write_v025_daemon_lock(&root, &original_lock);
        stop_v025_daemon(&mut owner);

        assert!(!output.status.success(), "{output:?}");
        assert!(owner_survived, "digest-mismatched owner was signaled");
        assert!(
            target_unchanged,
            "managed executable changed after rejection"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("owner image does not match its held ctx daemon lock"),
            "{output:?}"
        );
    }

    #[test]
    fn v025_interrupted_probe_is_fix_forward_and_retry_restarts_once() {
        let temp = daemon_test_root();
        let target = install_v025_fixture(&temp);
        let mut old_daemon = start_v025_daemon(&temp, &target);
        let old_pid = old_daemon.id();

        let interrupted = run_v025_upgrade(&temp, &target, true);
        assert_eq!(interrupted.status.code(), Some(86), "{interrupted:?}");
        assert!(!old_daemon.wait().unwrap().success());
        assert!(!process_is_running(old_pid));
        std::thread::sleep(Duration::from_millis(400));
        let stale_lock_pid = fs::read_to_string(data_root(&temp).join("upgrade.lock"))
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .parse::<u32>()
            .unwrap();
        assert!(!process_is_running(stale_lock_pid));
        assert!(
            running_daemon_pid(&data_root(&temp), Some(old_pid)).is_none(),
            "the interrupted fix-forward helper relaunched v0.25"
        );
        let interrupted_stages = v025_staged_binaries(&target);
        assert_eq!(interrupted_stages.len(), 1, "{interrupted_stages:?}");
        let interrupted_stage = interrupted_stages[0].clone();
        let active_stage =
            target.with_file_name(format!(".ctx-upgrade-{}.2.new", std::process::id()));
        fs::copy(v1_v025_candidate(&temp), &active_stage).unwrap();

        let retried = run_v025_upgrade(&temp, &target, false);
        assert!(retried.status.success(), "{retried:?}");
        let root = data_root(&temp);
        let mut replacement_pid = None;
        wait_for("retry replacement daemon", Duration::from_secs(15), || {
            replacement_pid = running_daemon_pid(&root, Some(old_pid));
            replacement_pid.is_some()
        });
        assert!(!root.join("upgrade.lock").exists());
        wait_for(
            "abandoned v0.25 stage cleanup",
            Duration::from_secs(5),
            || !interrupted_stage.exists(),
        );
        assert!(
            active_stage.exists(),
            "cleanup deleted a stage owned by a live process"
        );
        stop_daemon(replacement_pid.unwrap());
    }

    #[test]
    fn status_and_autostart_fail_closed_for_deleted_legacy_owner_image() {
        let temp = daemon_test_root();
        let target = install_v025_fixture(&temp);
        let mut old_daemon = start_v025_daemon(&temp, &target);
        let old_pid = old_daemon.id();
        let candidate = v1_v025_candidate(&temp);
        let staged = target.with_extension("new");
        fs::copy(&candidate, &staged).unwrap();
        fs::rename(&staged, &target).unwrap();
        fs::write(
            install_marker_path(&target),
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema_version": 1,
                "manager": "ctx-hosted-installer",
                "install_attempt_id": "ia_stale_owner_replacement",
                "install_path": target,
                "platform": test_platform_key().replace('_', "-"),
                "channel": "stable",
                "version": env!("CARGO_PKG_VERSION"),
                "sha256": sha256_hex(&fs::read(&target).unwrap()),
                "installed_at": "2026-07-30T00:00:00Z",
            }))
            .unwrap(),
        )
        .unwrap();

        let stale = json_output(ctx_from_binary(&temp, &target).args([
            "daemon",
            "status",
            "--format=json",
        ]));
        assert_eq!(stale["daemon"]["running"], false);
        assert_eq!(stale["daemon"]["status"], "stale_lock");
        assert_eq!(stale["daemon"]["recoverable"], true);
        assert_eq!(stale["daemon"]["reason"], "daemon_owner_identity_mismatch");

        let rejected = ctx_from_binary(&temp, &target)
            .args(["daemon", "enable", "--format=json"])
            .output()
            .unwrap();
        assert!(!rejected.status.success(), "{rejected:?}");
        assert!(
            String::from_utf8_lossy(&rejected.stderr)
                .contains("a live ctx daemon is owned by a different binary image"),
            "{rejected:?}"
        );
        assert!(
            process_is_running(old_pid),
            "ambiguous deleted-inode owner was signaled"
        );
        stop_v025_daemon(&mut old_daemon);
        assert!(!process_is_running(old_pid));

        ctx_from_binary(&temp, &target)
            .args(["daemon", "enable", "--format=json"])
            .assert()
            .success();
        let root = data_root(&temp);
        let mut replacement_pid = None;
        wait_for("recovered v1 owner", Duration::from_secs(15), || {
            replacement_pid = running_daemon_pid(&root, Some(old_pid));
            replacement_pid.is_some()
        });
        stop_daemon(replacement_pid.unwrap());
    }

    #[test]
    fn daemon_reports_one_terminal_upgrade_event_after_durable_state() {
        let temp = tempdir();
        let mut release = fake_release(&temp, FIXTURE_TARGET_VERSION);
        let binary = managed_hook_candidate(&temp, "ia_daemon_telemetry");
        patch_release_artifact_with_next_ctx(&mut release, &binary, FIXTURE_TARGET_VERSION);
        let events_path = temp.path().join("analytics.jsonl");
        let data_root = temp.path().join("data");
        let home = temp.path().join("home");
        let state_root = temp.path().join("state");
        fs::create_dir(&home).unwrap();
        seed_authoritative_codex_source(&home);

        let output = managed_daemon(&temp, &release, &binary)
            .env("CTX_DATA_ROOT", &data_root)
            .env("HOME", &home)
            .env("XDG_STATE_HOME", &state_root)
            .env("LOCALAPPDATA", &state_root)
            .env_remove("CTX_ANALYTICS_ENABLED")
            .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
            .env("CTX_DAEMON_AUTOSTART_OFF", "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(
            events_path.exists(),
            "daemon emitted no telemetry file; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );

        let state: Value =
            serde_json::from_slice(&fs::read(scheduler_state_path(&binary)).unwrap()).unwrap();
        assert_eq!(state["status"], "applied");
        let events = read_analytics_events(&events_path);
        let upgrades = events
            .iter()
            .filter(|batch| {
                batch["events"].as_array().is_some_and(|events| {
                    events.iter().any(|event| event["operation"] == "upgrade")
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(upgrades.len(), 1);
        assert_operation_event(upgrades[0], "upgrade", "success");
        let properties = analytics_event_properties(upgrades[0]);
        assert_eq!(properties["upgrade_mode"], "auto");
        assert_eq!(properties["upgrade_status"], "applied");
        assert_eq!(properties["upgrade_applied"], true);
        assert_eq!(
            properties["upgrade_attempt_id"],
            state["attempt_id"].as_str().unwrap()
        );
    }

    #[test]
    fn long_running_second_root_acknowledges_before_mutation_and_restarts() {
        let second = daemon_test_root();
        let mut release = fake_release(&second, FIXTURE_TARGET_VERSION);
        let binary = managed_bound_hook_candidate(&second, "ia_cross_root");
        patch_release_artifact_with_next_ctx(&mut release, &binary, FIXTURE_TARGET_VERSION);
        let binary_before = fs::read(&binary).unwrap();
        let first = tempdir();
        let first_data_root = data_root(&first);
        let second_data_root = data_root(&second);
        initialize_source_backed_epoch(&first);
        initialize_source_backed_epoch(&second);
        fs::write(
            second_data_root.join("config.toml"),
            "[upgrade]\nauto = \"off\"\n",
        )
        .unwrap();
        let pause = second.path().join("installation-quiesced");

        start_managed_background_daemon(&second, &release, &binary);
        wait_for(
            "long-running second daemon",
            Duration::from_secs(10),
            || running_daemon_pid(&second_data_root, None).is_some(),
        );
        let second_pid = running_daemon_pid(&second_data_root, None).unwrap();

        std::thread::scope(|scope| {
            let owner_handle = scope.spawn(|| {
                managed_daemon_with_timing(&first, &release, &binary, 30)
                    .env("CTX_UPGRADE_INTERVAL_SECONDS", "3600")
                    .env("CTX_UPGRADE_PAUSE_AFTER_QUIESCENCE_FOR_TESTS", &pause)
                    .env_remove("CTX_DAEMON_AUTOSTART_OFF")
                    .output()
                    .unwrap()
            });
            wait_for(
                "installation-wide quiescence",
                FIXTURE_QUIESCENCE_TIMEOUT,
                || pause.exists(),
            );

            assert_eq!(
                fs::read(&binary).unwrap(),
                binary_before,
                "managed executable changed before every daemon acknowledged quiescence"
            );
            assert!(
                !process_is_running(second_pid),
                "long-running non-owner daemon had not exited before first mutation"
            );
            let state: Value =
                serde_json::from_slice(&fs::read(scheduler_state_path(&binary)).unwrap()).unwrap();
            assert_eq!(state["status"], "staged");
            let attempt_id = state["attempt_id"].as_str().unwrap();
            let second_ack = installation_acknowledgement(&binary, &second_data_root, attempt_id)
                .expect("second root did not leave an attempt-bound acknowledgement");
            assert_eq!(second_ack["pid"], second_pid);
            let owner_pid = installation_acknowledgement(&binary, &first_data_root, attempt_id)
                .and_then(|value| value["pid"].as_u64())
                .and_then(|pid| u32::try_from(pid).ok())
                .expect("owner root did not leave an attempt-bound acknowledgement");
            assert!(
                second_data_root
                    .join("daemon/upgrade-restart-requests")
                    .join(format!("{attempt_id}.json"))
                    .exists(),
                "second root did not preserve restart intent"
            );

            fs::write(pause.with_extension("continue"), b"continue\n").unwrap();
            let owner_output = owner_handle.join().unwrap();
            assert!(owner_output.status.success(), "{owner_output:?}");

            let mut restarted_first = None;
            let mut restarted_second = None;
            wait_for(
                "both installation daemons to restart",
                Duration::from_secs(15),
                || {
                    restarted_first = running_daemon_pid(&first_data_root, Some(owner_pid));
                    restarted_second = running_daemon_pid(&second_data_root, Some(second_pid));
                    restarted_first.is_some() && restarted_second.is_some()
                },
            );

            let final_state: Value =
                serde_json::from_slice(&fs::read(scheduler_state_path(&binary)).unwrap()).unwrap();
            let marker: Value =
                serde_json::from_slice(&fs::read(install_marker_path(&binary)).unwrap()).unwrap();
            assert_eq!(final_state["status"], "applied");
            assert_eq!(final_state["attempt_source"], "automatic");
            assert_eq!(marker["version"], FIXTURE_TARGET_VERSION);
            assert_ne!(fs::read(&binary).unwrap(), binary_before);
            assert!(!first_data_root.join("upgrade-state.json").exists());
            assert!(!second_data_root.join("upgrade-state.json").exists());

            stop_daemon(restarted_first.unwrap());
            stop_daemon(restarted_second.unwrap());
        });
        assert_source_backed_epoch_remained_fresh(&first_data_root);
        assert_source_backed_epoch_remained_fresh(&second_data_root);
    }

    #[test]
    fn current_recovery_quiesces_opted_out_second_root_before_mutation() {
        for owner in [RecoveryOwner::Automatic, RecoveryOwner::Explicit] {
            prove_recovery_quiescence(owner);
        }
    }

    #[test]
    fn stale_current_recovery_discovery_cannot_claim_replacement_attempt() {
        for owner in [RecoveryOwner::Automatic, RecoveryOwner::Explicit] {
            prove_stale_recovery_observation_is_rejected(owner);
        }
    }

    #[test]
    fn explicit_manual_upgrade_still_works_with_daemon_and_auto_disabled() {
        let temp = tempdir();
        let mut release = fake_release(&temp, FIXTURE_TARGET_VERSION);
        let binary = managed_hook_candidate(&temp, "ia_manual_disabled");
        patch_release_artifact_with_next_ctx(&mut release, &binary, FIXTURE_TARGET_VERSION);

        managed_release_env_for_installation(
            ctx_from_binary(&temp, &binary).args(["upgrade", "--format=json"]),
            &release,
            &binary,
        )
        .env("CTX_DAEMON_ENABLED", "false")
        .env("CTX_UPGRADE_AUTO", "off")
        .assert()
        .success();

        let marker: Value =
            serde_json::from_slice(&fs::read(install_marker_path(&binary)).unwrap()).unwrap();
        assert_eq!(marker["version"], FIXTURE_TARGET_VERSION);
    }
}
