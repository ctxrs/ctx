use super::*;

#[cfg(unix)]
#[test]
fn runtime_publication_rolls_back_cli_runtime_and_marker_on_marker_failure() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let runtime = add_fake_release_runtime(&temp, &release);
    fs::create_dir_all(&runtime.target).unwrap();
    fs::write(runtime.target.join("old-runtime"), "old\n").unwrap();
    let cli_before = fs::read(&release.target).unwrap();
    let marker_path = install_marker_path(&release.target);
    let marker_before = fs::read(&marker_path).unwrap();

    let stderr = failure_stderr(
        fake_release_env(ctx(&temp).args(["upgrade", "--json"]), &release)
            .env("CTX_UPGRADE_FAIL_MARKER_PUBLISH_FOR_TESTS", "1"),
    );

    assert!(
        stderr.contains("injected install marker publication failure"),
        "{stderr}"
    );
    assert_eq!(fs::read(&release.target).unwrap(), cli_before);
    assert_eq!(fs::read(&marker_path).unwrap(), marker_before);
    assert_eq!(
        fs::read_to_string(runtime.target.join("old-runtime")).unwrap(),
        "old\n"
    );
    assert!(!runtime.target.join("VERSION_NUMBER").exists());
}

#[cfg(unix)]
#[test]
fn runtime_restore_failure_reports_primary_error_and_retains_backup() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let runtime = add_fake_release_runtime(&temp, &release);
    fs::create_dir_all(&runtime.target).unwrap();
    fs::write(runtime.target.join("old-runtime"), "old\n").unwrap();

    let stderr = failure_stderr(
        fake_release_env(ctx(&temp).args(["upgrade", "--json"]), &release)
            .env("CTX_UPGRADE_FAIL_MARKER_PUBLISH_FOR_TESTS", "1")
            .env("CTX_UPGRADE_FAIL_RUNTIME_RESTORE_FOR_TESTS", "1"),
    );

    assert!(
        stderr.contains("injected install marker publication failure"),
        "{stderr}"
    );
    assert!(
        stderr.contains("injected ONNX Runtime restore failure"),
        "{stderr}"
    );
    assert!(
        stderr.contains("recoverable backup retained at"),
        "{stderr}"
    );
    let runtime_parent = runtime.target.parent().unwrap();
    let backup = fs::read_dir(runtime_parent)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .contains(".runtime.previous")
        })
        .expect("recoverable runtime backup");
    assert_eq!(
        fs::read_to_string(backup.join("old-runtime")).unwrap(),
        "old\n"
    );
}

#[cfg(unix)]
#[test]
fn interrupted_publications_recover_before_the_next_upgrade_action() {
    for (injection, point) in [
        ("CTX_UPGRADE_ABORT_AFTER_BACKUP_FOR_TESTS", "runtime"),
        ("CTX_UPGRADE_ABORT_AFTER_BACKUP_FOR_TESTS", "marker"),
        ("CTX_UPGRADE_ABORT_AFTER_PUBLISH_FOR_TESTS", "runtime"),
        ("CTX_UPGRADE_ABORT_AFTER_PUBLISH_FOR_TESTS", "binary"),
        ("CTX_UPGRADE_ABORT_AFTER_PUBLISH_FOR_TESTS", "marker"),
    ] {
        let temp = tempdir();
        let release = fake_release(&temp, "9.9.9");
        let runtime = add_fake_release_runtime(&temp, &release);
        fs::create_dir_all(&runtime.target).unwrap();
        fs::write(runtime.target.join("old-runtime"), "old\n").unwrap();
        let cli_before = fs::read(&release.target).unwrap();
        let marker_path = install_marker_path(&release.target);
        let marker_before = fs::read(&marker_path).unwrap();

        let _ = failure_stderr(
            fake_release_env(ctx(&temp).args(["upgrade", "--json"]), &release)
                .env(injection, point),
        );
        assert!(
            release
                .target
                .with_file_name(".ctx.upgrade-install-transaction.json")
                .is_file(),
            "{injection}={point} did not retain a recovery journal"
        );

        let output = fake_release_env(ctx(&temp).args(["upgrade", "--json"]), &release)
            .env("CTX_UPGRADE_STOP_AFTER_RECOVERY_FOR_TESTS", "1")
            .output()
            .unwrap();
        let restored_running_executable = point == "marker"
            || (injection == "CTX_UPGRADE_ABORT_AFTER_PUBLISH_FOR_TESTS" && point == "binary");
        if restored_running_executable {
            assert!(output.status.success(), "{injection}={point}: {output:?}");
            assert_eq!(
                output.stdout,
                format!("ctx {}\n", env!("CARGO_PKG_VERSION")).as_bytes(),
                "{injection}={point}"
            );
        } else {
            assert!(!output.status.success(), "{injection}={point}: {output:?}");
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(
                stderr.contains("stopped after interrupted install recovery"),
                "{injection}={point}: {stderr}"
            );
        }
        assert_eq!(
            fs::read(&release.target).unwrap(),
            cli_before,
            "{injection}={point} did not restore the CLI"
        );
        assert_eq!(
            fs::read(&marker_path).unwrap(),
            marker_before,
            "{injection}={point} did not restore the marker"
        );
        assert_eq!(
            fs::read_to_string(runtime.target.join("old-runtime")).unwrap(),
            "old\n",
            "{injection}={point} did not restore the runtime"
        );
        assert!(
            !release
                .target
                .with_file_name(".ctx.upgrade-install-transaction.json")
                .exists(),
            "{injection}={point} left the recovery journal behind"
        );

        let applied = json_output(fake_release_env(
            ctx(&temp).args(["upgrade", "--json"]),
            &release,
        ));
        assert_eq!(applied["status"], "applied", "{injection}={point}");
    }
}

#[cfg(unix)]
#[test]
fn interrupted_publication_journal_exposes_minimal_v2_transaction_contract() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let runtime = add_fake_release_runtime(&temp, &release);
    fs::create_dir_all(&runtime.target).unwrap();

    let _ = failure_stderr(
        fake_release_env(ctx(&temp).args(["upgrade", "--json"]), &release)
            .env("CTX_UPGRADE_ABORT_AFTER_BACKUP_FOR_TESTS", "runtime"),
    );

    let journal_path = release
        .target
        .with_file_name(".ctx.upgrade-install-transaction.json");
    let journal: Value = serde_json::from_slice(&fs::read(&journal_path).unwrap()).unwrap();
    assert_eq!(journal["schema_version"], 2);
    assert!(journal["attempt_id"].as_str().is_some());
    assert!(journal.get("attempt_generation").is_none());
    assert_eq!(journal["data_root"], temp.path().display().to_string());
    assert!(journal.get("ownership_token").is_none());
    assert!(journal.get("scheduler").is_none());
    assert!(journal.get("telemetry").is_none());
    assert_eq!(journal["phase"], "publishing");
    assert_eq!(
        journal["install_path"],
        release.target.display().to_string()
    );
    let paths = journal["paths"].as_array().unwrap();
    assert_eq!(paths.len(), 3);
    for (path, label, kind) in [
        (&paths[0], "ONNX Runtime sidecar", "directory"),
        (&paths[1], "ctx binary", "file"),
        (&paths[2], "ctx install marker", "file"),
    ] {
        assert_eq!(path["label"], label);
        assert_eq!(path["kind"], kind);
        assert!(path["target_preexisted"].as_bool().is_some());
        assert!(path["state"].as_str().is_some());
        for identity in ["staged_identity", "original_target_identity"] {
            if let Some(identity) = path.get(identity) {
                assert!(identity["device"].is_u64());
                assert!(identity["inode"].is_u64());
                assert!(identity["length"].is_u64());
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn forged_recovery_journal_fails_closed_without_touching_paths() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let _runtime = add_fake_release_runtime(&temp, &release);
    let sentinel = temp.path().join("must-survive");
    fs::write(&sentinel, "safe\n").unwrap();
    fs::write(
        release.target.with_file_name(".ctx.upgrade-install-transaction.json"),
        serde_json::to_vec(&json!({
            "schema_version": 2,
            "attempt_id": "forged",
            "data_root": temp.path(),
            "runtime_root": temp.path().join("runtime"),
            "phase": "publishing",
            "install_path": sentinel,
            "paths": [
                {
                    "label": "ctx binary",
                    "staged": sentinel.parent().unwrap().join(".ctx-upgrade-forged.new"),
                    "target": sentinel,
                    "backup": sentinel.parent().unwrap().join(".must-survive.ctx-upgrade-forged.binary.previous"),
                    "kind": "file",
                    "target_preexisted": true,
                    "state": "published"
                },
                {
                    "label": "ctx install marker",
                    "staged": sentinel.parent().unwrap().join(".ctx-upgrade-forged.install.json.new"),
                    "target": sentinel.parent().unwrap().join("must-survive.install.json"),
                    "backup": sentinel.parent().unwrap().join(".must-survive.install.json.ctx-upgrade-forged.marker.previous"),
                    "kind": "file",
                    "target_preexisted": false,
                    "state": "staged"
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let stderr = failure_stderr(fake_release_env(
        ctx(&temp).args(["upgrade", "--json"]),
        &release,
    ));

    assert!(
        stderr.contains("install transaction runtime root")
            || stderr.contains("expected current managed install"),
        "{stderr}"
    );
    assert_eq!(fs::read_to_string(&sentinel).unwrap(), "safe\n");
}

#[cfg(unix)]
#[test]
fn interrupted_committed_transaction_finishes_without_rolling_back() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let runtime = add_fake_release_runtime(&temp, &release);
    fs::create_dir_all(&runtime.target).unwrap();
    fs::write(runtime.target.join("old-runtime"), "old\n").unwrap();

    let _ = failure_stderr(
        fake_release_env(ctx(&temp).args(["upgrade", "--json"]), &release)
            .env("CTX_UPGRADE_ABORT_AFTER_COMMIT_FOR_TESTS", "1"),
    );

    let stderr = failure_stderr(
        fake_release_env(ctx(&temp).args(["upgrade", "--json"]), &release)
            .env("CTX_UPGRADE_STOP_AFTER_RECOVERY_FOR_TESTS", "1"),
    );
    assert!(
        stderr.contains("stopped after interrupted install recovery"),
        "{stderr}"
    );
    assert_eq!(
        fs::read_to_string(&release.target).unwrap(),
        "#!/bin/sh\nprintf 'ctx 9.9.9\\n'\n"
    );
    assert!(runtime.target.join("VERSION_NUMBER").is_file());
    assert!(!runtime.target.join("old-runtime").exists());
    let marker: Value =
        serde_json::from_slice(&fs::read(install_marker_path(&release.target)).unwrap()).unwrap();
    assert_eq!(marker["version"], "9.9.9");
}

#[cfg(unix)]
#[test]
fn state_write_failure_after_commit_is_reported_as_warning() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let runtime = add_fake_release_runtime(&temp, &release);

    let applied = json_output(
        fake_release_env(ctx(&temp).args(["upgrade", "--json"]), &release)
            .env("CTX_UPGRADE_FAIL_STATE_WRITE_FOR_TESTS", "1"),
    );

    assert_eq!(applied["status"], "applied");
    assert_eq!(applied["applied"], true);
    assert!(applied["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|warning| warning
            .as_str()
            .unwrap()
            .contains("local upgrade state could not be written")));
    assert!(runtime.target.join("VERSION_NUMBER").is_file());
    let marker: Value =
        serde_json::from_slice(&fs::read(install_marker_path(&release.target)).unwrap()).unwrap();
    assert_eq!(marker["version"], "9.9.9");
}

#[cfg(unix)]
#[test]
fn committed_journal_write_failure_rolls_back_immediately() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let runtime = add_fake_release_runtime(&temp, &release);
    fs::create_dir_all(&runtime.target).unwrap();
    fs::write(runtime.target.join("old-runtime"), "old\n").unwrap();
    let cli_before = fs::read(&release.target).unwrap();
    let marker_path = install_marker_path(&release.target);
    let marker_before = fs::read(&marker_path).unwrap();

    let stderr = failure_stderr(
        fake_release_env(ctx(&temp).args(["upgrade", "--json"]), &release)
            .env("CTX_UPGRADE_FAIL_COMMIT_JOURNAL_WRITE_FOR_TESTS", "1"),
    );

    assert!(
        stderr.contains("injected committed journal write failure"),
        "{stderr}"
    );
    assert_eq!(fs::read(&release.target).unwrap(), cli_before);
    assert_eq!(fs::read(&marker_path).unwrap(), marker_before);
    assert_eq!(
        fs::read_to_string(runtime.target.join("old-runtime")).unwrap(),
        "old\n"
    );
    assert!(!release
        .target
        .with_file_name(".ctx.upgrade-install-transaction.json")
        .exists());
}
