mod support;

#[cfg(unix)]
mod unix {
    use std::{
        fs,
        path::{Path, PathBuf},
        time::{Duration, Instant},
    };

    use serde_json::Value;

    use super::support::*;

    fn scheduler_state_path(binary: &Path) -> PathBuf {
        binary.with_file_name(".ctx.upgrade-state.json")
    }

    fn managed_hook_candidate(temp: &tempfile::TempDir, install_attempt_id: &str) -> PathBuf {
        let configured = PathBuf::from(
            std::env::var_os("CTX_AUTO_UPGRADE_ACCEPTANCE_FIXTURE")
                .expect("Bazel must provide the auto-upgrade hook fixture"),
        );
        let source = if configured.is_absolute() {
            configured
        } else {
            std::env::current_dir().unwrap().join(configured)
        };
        managed_candidate_from_binary(temp, &source, install_attempt_id)
    }

    fn managed_daemon(
        temp: &tempfile::TempDir,
        release: &FakeRelease,
        binary: &Path,
    ) -> assert_cmd::Command {
        managed_daemon_with_timing(temp, release, binary, 1, 1)
    }

    fn managed_daemon_with_timing(
        temp: &tempfile::TempDir,
        release: &FakeRelease,
        binary: &Path,
        idle_exit_seconds: u64,
        loop_interval_seconds: u64,
    ) -> assert_cmd::Command {
        let mut command = ctx_from_binary(temp, binary);
        managed_release_env(&mut command, release, binary);
        command
            .args(["daemon", "run", "--idle-exit-seconds"])
            .arg(idle_exit_seconds.to_string())
            .arg("--loop-interval-seconds")
            .arg(loop_interval_seconds.to_string())
            .args([
                "--start-mode",
                "auto",
                "--trigger-command",
                "setup",
                "--format=json",
            ])
            .env(
                "CTX_DAEMON_AUTOSTART_IDLE_EXIT_SECONDS",
                idle_exit_seconds.to_string(),
            )
            .env(
                "CTX_DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS",
                loop_interval_seconds.to_string(),
            )
            .env("CTX_DAEMON_BACKGROUND_CHILD", "1");
        command
    }

    fn patch_release_artifact_with_next_ctx(
        release: &mut FakeRelease,
        binary: &Path,
        next_version: &str,
    ) {
        let current_version = env!("CARGO_PKG_VERSION");
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
                    &format!("CTX_RELEASE_VERSION={}\n", "9.9.9"),
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

    fn installation_acknowledgement(
        binary: &Path,
        data_root: &Path,
        attempt_id: &str,
    ) -> Option<Value> {
        let root = binary.with_file_name(".ctx.daemon-quiescence-acks");
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

    fn initialize_source_backed_epoch(data_root: &Path) {
        let fixture = PathBuf::from(provider_history_fixture("codex-sessions"));
        copy_dir_all(&fixture, &data_root.join(".codex/sessions"));
        let generation_id = initialize_generation_only_sql_projection(data_root);
        assert!(!generation_id.is_empty());
        assert_source_backed_epoch_remained_store_free(data_root);
    }

    fn assert_source_backed_epoch_remained_store_free(data_root: &Path) {
        assert!(data_root.join("relational.sqlite").is_file());
        assert!(data_root.join("search/lexical").is_dir());
        assert!(
            !data_root.join("work.sqlite").exists(),
            "v0.26 upgrade fixtures must not open or recreate the legacy Store"
        );
    }

    #[derive(Clone, Copy, Debug)]
    enum RecoveryJournal {
        CurrentPrepared,
        LegacyV025,
    }

    #[derive(Clone, Copy, Debug)]
    enum RecoveryOwner {
        Automatic,
        Explicit,
    }

    fn write_legacy_v025_committed_journal(
        data_root: &Path,
        binary: &Path,
        attempt_id: &str,
    ) -> (PathBuf, PathBuf) {
        let marker = install_marker_path(binary);
        let binary_name = binary.file_name().unwrap().to_str().unwrap();
        let marker_name = marker.file_name().unwrap().to_str().unwrap();
        let binary_backup = binary.with_file_name(format!(
            ".{binary_name}.ctx-upgrade-{attempt_id}.binary.previous"
        ));
        fs::copy(binary, &binary_backup).unwrap();
        let journal_path = data_root.join("upgrade-install-transaction.json");
        fs::write(
            &journal_path,
            serde_json::to_vec_pretty(&json!({
                "schema_version": 1,
                "transaction_id": attempt_id,
                "phase": "committed",
                "install_path": binary,
                "paths": [
                    {
                        "label": "ctx binary",
                        "staged": binary.with_file_name(format!(".ctx-upgrade-{attempt_id}.new")),
                        "target": binary,
                        "backup": binary_backup,
                        "kind": "file"
                    },
                    {
                        "label": "ctx install marker",
                        "staged": marker.with_file_name(format!(".ctx-upgrade-{attempt_id}.install.json.new")),
                        "target": marker,
                        "backup": marker.with_file_name(format!(".{marker_name}.ctx-upgrade-{attempt_id}.marker.previous")),
                        "kind": "file"
                    }
                ]
            }))
            .unwrap(),
        )
        .unwrap();
        (journal_path, binary_backup)
    }

    fn acknowledgement_snapshot(binary: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let root = binary.with_file_name(".ctx.daemon-quiescence-acks");
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

    fn write_completed_attempt_state(binary: &Path, attempt_id: &str) {
        fs::write(
            scheduler_state_path(binary),
            serde_json::to_vec_pretty(&json!({
                "schema_version": 1,
                "status": "error",
                "attempt_id": attempt_id,
                "attempt_source": "daemon",
                "last_attempt_at": "2026-07-24T00:00:00Z",
                "last_attempt_finished_at": "2026-07-24T00:00:01Z",
                "consecutive_failures": 1,
                "error": "attempt completed before a stale recovery claimant acquired ownership"
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn prove_stale_recovery_observation_is_rejected(
        journal_kind: RecoveryJournal,
        recovery_owner: RecoveryOwner,
    ) {
        let installation = tempdir();
        let release = fake_release(&installation, "9.9.9");
        let binary = managed_hook_candidate(
            &installation,
            &format!("ia_stale_recovery_{journal_kind:?}_{recovery_owner:?}"),
        );
        let binary_before = fs::read(&binary).unwrap();
        let owner = tempdir();
        initialize_source_backed_epoch(owner.path());

        let (attempt_id, journal_path) = match journal_kind {
            RecoveryJournal::CurrentPrepared => {
                let prepared = managed_daemon_with_timing(&owner, &release, &binary, 1, 1)
                    .env("CTX_DAEMON_AUTOSTART_OFF", "1")
                    .env("CTX_UPGRADE_FAIL_APPLYING_STATE_WRITE_FOR_TESTS", "1")
                    .output()
                    .unwrap();
                assert!(prepared.status.success(), "{prepared:?}");
                let journal_path = binary.with_file_name(".ctx.upgrade-install-transaction.json");
                let journal: Value =
                    serde_json::from_slice(&fs::read(&journal_path).unwrap()).unwrap();
                assert_eq!(journal["phase"], "prepared");
                (
                    journal["attempt_id"].as_str().unwrap().to_owned(),
                    journal_path,
                )
            }
            RecoveryJournal::LegacyV025 => {
                let attempt_id = format!(
                    "stale-legacy-{}",
                    match recovery_owner {
                        RecoveryOwner::Automatic => "automatic",
                        RecoveryOwner::Explicit => "explicit",
                    }
                );
                let (journal_path, _) =
                    write_legacy_v025_committed_journal(owner.path(), &binary, &attempt_id);
                write_completed_attempt_state(&binary, &attempt_id);
                (attempt_id, journal_path)
            }
        };
        let state_before = fs::read(scheduler_state_path(&binary)).unwrap();
        let acknowledgements_before = acknowledgement_snapshot(&binary);
        let pause = installation
            .path()
            .join(format!("stale-{journal_kind:?}-{recovery_owner:?}"));
        let stale_rejection = installation.path().join(format!(
            "stale-rejected-{journal_kind:?}-{recovery_owner:?}"
        ));

        std::thread::scope(|scope| {
            let claimant = scope.spawn(|| {
                let mut command = match recovery_owner {
                    RecoveryOwner::Automatic => {
                        managed_daemon_with_timing(&owner, &release, &binary, 1, 1)
                    }
                    RecoveryOwner::Explicit => {
                        let mut command = ctx_from_binary(&owner, &binary);
                        managed_release_env(&mut command, &release, &binary);
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
                    .env("CTX_DAEMON_AUTOSTART_OFF", "1")
                    .output()
                    .unwrap()
            });
            wait_for("stale recovery discovery", Duration::from_secs(10), || {
                pause.exists()
            });

            let replacement_attempt_id = format!("{attempt_id}-replacement");
            match journal_kind {
                RecoveryJournal::CurrentPrepared => replace_current_recovery_attempt(
                    &journal_path,
                    &attempt_id,
                    &replacement_attempt_id,
                ),
                RecoveryJournal::LegacyV025 => {
                    write_legacy_v025_committed_journal(
                        owner.path(),
                        &binary,
                        &replacement_attempt_id,
                    );
                }
            }
            let replacement_journal = fs::read(&journal_path).unwrap();
            let terminal_state = fs::read(scheduler_state_path(&binary)).unwrap();
            assert_eq!(terminal_state, state_before);

            fs::write(pause.with_extension("continue"), b"continue\n").unwrap();
            if matches!(recovery_owner, RecoveryOwner::Automatic) {
                wait_for(
                    "automatic stale recovery rejection",
                    Duration::from_secs(10),
                    || stale_rejection.exists(),
                );
                assert_eq!(
                    fs::read(scheduler_state_path(&binary)).unwrap(),
                    terminal_state
                );
                assert_eq!(fs::read(&journal_path).unwrap(), replacement_journal);
                // The stale observation has now been rejected under the lock.
                // Disable later scheduler ticks so a fresh, valid discovery of
                // the replacement attempt cannot obscure what this claimant did.
                fs::write(
                    owner.path().join("config.toml"),
                    "[upgrade]\nauto = \"off\"\n",
                )
                .unwrap();
                fs::write(stale_rejection.with_extension("continue"), b"continue\n").unwrap();
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

            assert_eq!(
                fs::read(scheduler_state_path(&binary)).unwrap(),
                terminal_state,
                "{journal_kind:?}/{recovery_owner:?} rewrote terminal scheduler state"
            );
            assert_eq!(
                fs::read(&journal_path).unwrap(),
                replacement_journal,
                "{journal_kind:?}/{recovery_owner:?} mutated replacement recovery authority"
            );
            assert_eq!(
                fs::read(&binary).unwrap(),
                binary_before,
                "{journal_kind:?}/{recovery_owner:?} mutated the installation"
            );
            assert_eq!(
                acknowledgement_snapshot(&binary),
                acknowledgements_before,
                "{journal_kind:?}/{recovery_owner:?} began a daemon handoff from stale discovery"
            );
        });
        assert_source_backed_epoch_remained_store_free(owner.path());
    }

    fn prove_recovery_quiescence(journal_kind: RecoveryJournal, recovery_owner: RecoveryOwner) {
        let installation = tempdir();
        let release = fake_release(&installation, "9.9.9");
        let binary = managed_hook_candidate(
            &installation,
            &format!("ia_recovery_{journal_kind:?}_{recovery_owner:?}"),
        );
        let binary_before = fs::read(&binary).unwrap();
        let owner = tempdir();
        let second = tempdir();
        initialize_source_backed_epoch(owner.path());
        initialize_source_backed_epoch(second.path());
        fs::write(
            second.path().join("config.toml"),
            "[upgrade]\nauto = \"off\"\n",
        )
        .unwrap();

        let (attempt_id, journal_path, legacy_backup) = match journal_kind {
            RecoveryJournal::CurrentPrepared => {
                let prepared = managed_daemon_with_timing(&owner, &release, &binary, 1, 1)
                    .env("CTX_DAEMON_AUTOSTART_OFF", "1")
                    .env("CTX_UPGRADE_FAIL_APPLYING_STATE_WRITE_FOR_TESTS", "1")
                    .output()
                    .unwrap();
                assert!(prepared.status.success(), "{prepared:?}");
                let state: Value =
                    serde_json::from_slice(&fs::read(scheduler_state_path(&binary)).unwrap())
                        .unwrap();
                assert_eq!(state["status"], "error");
                let journal_path = binary.with_file_name(".ctx.upgrade-install-transaction.json");
                let journal: Value =
                    serde_json::from_slice(&fs::read(&journal_path).unwrap()).unwrap();
                assert_eq!(journal["phase"], "prepared");
                (
                    journal["attempt_id"].as_str().unwrap().to_owned(),
                    journal_path,
                    None,
                )
            }
            RecoveryJournal::LegacyV025 => {
                let attempt_id = format!(
                    "legacy-{}",
                    match recovery_owner {
                        RecoveryOwner::Automatic => "automatic",
                        RecoveryOwner::Explicit => "explicit",
                    }
                );
                let (journal_path, backup) =
                    write_legacy_v025_committed_journal(owner.path(), &binary, &attempt_id);
                (attempt_id, journal_path, Some(backup))
            }
        };
        let journal_before = fs::read(&journal_path).unwrap();
        if matches!(recovery_owner, RecoveryOwner::Explicit) {
            rewrite_fake_release_metadata(&release, |metadata| {
                metadata.replace(
                    "CTX_RELEASE_VERSION=9.9.9\n",
                    &format!("CTX_RELEASE_VERSION={}\n", env!("CARGO_PKG_VERSION")),
                )
            });
        }
        let pause = installation
            .path()
            .join(format!("recovering-{journal_kind:?}-{recovery_owner:?}"));

        std::thread::scope(|scope| {
            let second_handle = scope.spawn(|| {
                managed_daemon_with_timing(&second, &release, &binary, 60, 30)
                    .env("CTX_UPGRADE_AUTO", "off")
                    .env_remove("CTX_DAEMON_AUTOSTART_OFF")
                    .output()
                    .unwrap()
            });
            wait_for(
                "long-running recovery participant",
                Duration::from_secs(10),
                || running_daemon_pid(second.path(), None).is_some(),
            );
            let second_pid = running_daemon_pid(second.path(), None).unwrap();

            let owner_handle = scope.spawn(|| {
                let mut command = match recovery_owner {
                    RecoveryOwner::Automatic => {
                        managed_daemon_with_timing(&owner, &release, &binary, 60, 30)
                    }
                    RecoveryOwner::Explicit => {
                        let mut command = ctx_from_binary(&owner, &binary);
                        managed_release_env(&mut command, &release, &binary);
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
            wait_for("owned recovery quiescence", Duration::from_secs(15), || {
                pause.exists()
            });

            assert_eq!(
                fs::read(&binary).unwrap(),
                binary_before,
                "{journal_kind:?}/{recovery_owner:?} mutated the executable before quiescence"
            );
            assert_eq!(
                fs::read(&journal_path).unwrap(),
                journal_before,
                "{journal_kind:?}/{recovery_owner:?} mutated its journal before quiescence"
            );
            if let Some(backup) = legacy_backup.as_ref() {
                assert!(
                    backup.exists(),
                    "{journal_kind:?}/{recovery_owner:?} consumed a legacy backup before quiescence"
                );
            }
            assert!(
                second_handle.is_finished(),
                "{journal_kind:?}/{recovery_owner:?} left the opted-out second root running"
            );
            let second_output = second_handle.join().unwrap();
            assert!(second_output.status.success(), "{second_output:?}");
            let recovering: Value =
                serde_json::from_slice(&fs::read(scheduler_state_path(&binary)).unwrap()).unwrap();
            assert_eq!(recovering["status"], "recovering");
            assert_eq!(recovering["attempt_id"], attempt_id);
            let second_ack = installation_acknowledgement(&binary, second.path(), &attempt_id)
                .unwrap_or_else(|| {
                    panic!(
                        "{journal_kind:?}/{recovery_owner:?} has no second-root recovery acknowledgement"
                    )
                });
            assert_eq!(second_ack["pid"], second_pid);
            let mut contender = ctx_from_binary(&owner, &binary);
            managed_release_env(&mut contender, &release, &binary);
            let contended = contender
                .args(["upgrade", "check", "--format=json"])
                .output()
                .unwrap();
            assert!(
                !contended.status.success(),
                "{journal_kind:?}/{recovery_owner:?} allowed a second recovery owner"
            );
            assert!(
                String::from_utf8_lossy(&contended.stderr)
                    .contains("upgrade lock is held for interrupted recovery"),
                "{journal_kind:?}/{recovery_owner:?}: {contended:?}"
            );
            assert_eq!(
                fs::read(&journal_path).unwrap(),
                journal_before,
                "{journal_kind:?}/{recovery_owner:?} contender mutated recovery state"
            );

            let owner_pid = if matches!(recovery_owner, RecoveryOwner::Automatic) {
                installation_acknowledgement(&binary, owner.path(), &attempt_id)
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
                "{journal_kind:?}/{recovery_owner:?} retained a recovered journal"
            );

            let mut restarted_second = None;
            wait_for(
                "opted-out second root recovery restart replay",
                Duration::from_secs(15),
                || {
                    restarted_second = running_daemon_pid(second.path(), Some(second_pid));
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
                        restarted_owner = running_daemon_pid(owner.path(), Some(owner_pid));
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
                    assert_eq!(
                        final_state["status"],
                        match journal_kind {
                            RecoveryJournal::CurrentPrepared => "error",
                            RecoveryJournal::LegacyV025 => "applied",
                        }
                    );
                }
                RecoveryOwner::Explicit => {
                    assert_eq!(final_state["status"], "up_to_date");
                    assert_eq!(final_state["attempt_source"], "manual_apply");
                }
            }
        });
        assert_source_backed_epoch_remained_store_free(owner.path());
        assert_source_backed_epoch_remained_store_free(second.path());
    }

    #[test]
    fn foreground_command_never_claims_or_applies_an_upgrade() {
        let temp = tempdir();
        let release = fake_release(&temp, "9.9.9");
        let binary = managed_candidate(&temp, "ia_foreground_no_authority");
        let before = fs::read(&binary).unwrap();

        managed_release_env(
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
        let interrupted = managed_release_env(
            ctx_from_binary(&temp, &binary).args(["upgrade", "--format=json"]),
            &release,
            &binary,
        )
        .env("CTX_UPGRADE_ABORT_AFTER_BACKUP_FOR_TESTS", "binary")
        .output()
        .unwrap();
        assert!(!interrupted.status.success(), "{interrupted:?}");

        let journal = binary.with_file_name(".ctx.upgrade-install-transaction.json");
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

        managed_daemon(&temp, &release, &binary)
            .env("CTX_DAEMON_AUTOSTART_OFF", "1")
            .env("CTX_UPGRADE_FAIL_APPLYING_STATE_WRITE_FOR_TESTS", "1")
            .assert()
            .success();

        let journal = binary.with_file_name(".ctx.upgrade-install-transaction.json");
        let transaction: Value = serde_json::from_slice(&fs::read(&journal).unwrap()).unwrap();
        assert_eq!(transaction["phase"], "prepared");
        assert_eq!(fs::read(&binary).unwrap(), before);
        let state: Value =
            serde_json::from_slice(&fs::read(scheduler_state_path(&binary)).unwrap()).unwrap();
        assert_eq!(state["status"], "error");
    }

    #[test]
    fn enabled_daemon_is_the_only_automatic_apply_authority() {
        let temp = tempdir();
        let release = fake_release(&temp, "9.9.9");
        let binary = managed_candidate(&temp, "ia_daemon_authority");

        managed_daemon(&temp, &release, &binary).assert().success();

        let marker: Value =
            serde_json::from_slice(&fs::read(install_marker_path(&binary)).unwrap()).unwrap();
        let state: Value =
            serde_json::from_slice(&fs::read(scheduler_state_path(&binary)).unwrap()).unwrap();
        assert_eq!(marker["version"], "9.9.9");
        assert_eq!(state["status"], "applied");
        assert_eq!(state["attempt_source"], "daemon");
        assert!(!temp.path().join("upgrade-state.json").exists());
        assert!(!temp.path().join("upgrade.lock").exists());
    }

    #[test]
    fn daemon_reports_one_terminal_upgrade_event_after_durable_state() {
        let temp = tempdir();
        let release = fake_release(&temp, "9.9.9");
        let binary = managed_candidate(&temp, "ia_daemon_telemetry");
        let events_path = temp.path().join("analytics.jsonl");
        let data_root = temp.path().join("data");
        let home = temp.path().join("home");
        let state_root = temp.path().join("state");
        fs::create_dir(&home).unwrap();

        let output = managed_daemon(&temp, &release, &binary)
            .env("CTX_DATA_ROOT", &data_root)
            .env("HOME", &home)
            .env("XDG_STATE_HOME", &state_root)
            .env("LOCALAPPDATA", &state_root)
            .env_remove("CTX_ANALYTICS_ENABLED")
            .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
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
        let installation = tempdir();
        let mut release = fake_release(&installation, "9.9.9");
        let binary = managed_hook_candidate(&installation, "ia_cross_root");
        patch_release_artifact_with_next_ctx(&mut release, &binary, "9.99.9");
        let binary_before = fs::read(&binary).unwrap();
        let first = tempdir();
        let second = tempdir();
        initialize_source_backed_epoch(first.path());
        initialize_source_backed_epoch(second.path());
        let pause = installation.path().join("installation-quiesced");

        std::thread::scope(|scope| {
            let second_handle = scope.spawn(|| {
                managed_daemon_with_timing(&second, &release, &binary, 60, 30)
                    .env("CTX_UPGRADE_AUTO", "off")
                    .env_remove("CTX_DAEMON_AUTOSTART_OFF")
                    .output()
                    .unwrap()
            });
            wait_for(
                "long-running second daemon",
                Duration::from_secs(10),
                || running_daemon_pid(second.path(), None).is_some(),
            );
            let second_pid = running_daemon_pid(second.path(), None).unwrap();

            let owner_handle = scope.spawn(|| {
                managed_daemon_with_timing(&first, &release, &binary, 60, 30)
                    .env("CTX_UPGRADE_INTERVAL_SECONDS", "3600")
                    .env("CTX_UPGRADE_PAUSE_AFTER_QUIESCENCE_FOR_TESTS", &pause)
                    .env_remove("CTX_DAEMON_AUTOSTART_OFF")
                    .output()
                    .unwrap()
            });
            wait_for(
                "installation-wide quiescence",
                Duration::from_secs(15),
                || pause.exists(),
            );

            assert_eq!(
                fs::read(&binary).unwrap(),
                binary_before,
                "managed executable changed before every daemon acknowledged quiescence"
            );
            assert!(
                second_handle.is_finished(),
                "long-running non-owner daemon had not exited before first mutation"
            );
            let second_output = second_handle.join().unwrap();
            assert!(second_output.status.success(), "{second_output:?}");
            let state: Value =
                serde_json::from_slice(&fs::read(scheduler_state_path(&binary)).unwrap()).unwrap();
            assert_eq!(state["status"], "staged");
            let attempt_id = state["attempt_id"].as_str().unwrap();
            let second_ack = installation_acknowledgement(&binary, second.path(), attempt_id)
                .expect("second root did not leave an attempt-bound acknowledgement");
            assert_eq!(second_ack["pid"], second_pid);
            let owner_pid = installation_acknowledgement(&binary, first.path(), attempt_id)
                .and_then(|value| value["pid"].as_u64())
                .and_then(|pid| u32::try_from(pid).ok())
                .expect("owner root did not leave an attempt-bound acknowledgement");
            assert!(
                second
                    .path()
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
                    restarted_first = running_daemon_pid(first.path(), Some(owner_pid));
                    restarted_second = running_daemon_pid(second.path(), Some(second_pid));
                    restarted_first.is_some() && restarted_second.is_some()
                },
            );

            let final_state: Value =
                serde_json::from_slice(&fs::read(scheduler_state_path(&binary)).unwrap()).unwrap();
            let marker: Value =
                serde_json::from_slice(&fs::read(install_marker_path(&binary)).unwrap()).unwrap();
            assert_eq!(final_state["status"], "applied");
            assert_eq!(final_state["attempt_source"], "daemon");
            assert_eq!(marker["version"], "9.99.9");
            assert_ne!(fs::read(&binary).unwrap(), binary_before);
            assert!(!first.path().join("upgrade-state.json").exists());
            assert!(!second.path().join("upgrade-state.json").exists());

            stop_daemon(restarted_first.unwrap());
            stop_daemon(restarted_second.unwrap());
        });
        assert_source_backed_epoch_remained_store_free(first.path());
        assert_source_backed_epoch_remained_store_free(second.path());
    }

    #[test]
    fn current_and_legacy_recovery_quiesce_opted_out_second_root_before_mutation() {
        for journal in [
            RecoveryJournal::CurrentPrepared,
            RecoveryJournal::LegacyV025,
        ] {
            for owner in [RecoveryOwner::Automatic, RecoveryOwner::Explicit] {
                prove_recovery_quiescence(journal, owner);
            }
        }
    }

    #[test]
    fn stale_current_and_legacy_recovery_discovery_cannot_claim_replacement_attempt() {
        for journal in [
            RecoveryJournal::CurrentPrepared,
            RecoveryJournal::LegacyV025,
        ] {
            for owner in [RecoveryOwner::Automatic, RecoveryOwner::Explicit] {
                prove_stale_recovery_observation_is_rejected(journal, owner);
            }
        }
    }

    #[test]
    fn explicit_manual_upgrade_still_works_with_daemon_and_auto_disabled() {
        let temp = tempdir();
        let release = fake_release(&temp, "9.9.9");
        let binary = managed_candidate(&temp, "ia_manual_disabled");

        managed_release_env(
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
        assert_eq!(marker["version"], "9.9.9");
    }
}
