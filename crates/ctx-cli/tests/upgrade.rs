mod support;

#[cfg(unix)]
use support::*;

#[cfg(unix)]
#[test]
fn upgrade_status_check_and_apply_support_managed_installs() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let _runtime = add_fake_release_runtime(&temp, &release);

    let status = json_output(fake_release_env(
        ctx(&temp).args(["upgrade", "status", "--json"]),
        &release,
    ));
    assert_eq!(status["schema_version"], 1);
    assert_eq!(status["install"]["managed"], true);

    let check = json_output(fake_release_env(
        ctx(&temp).args(["upgrade", "check", "--json"]),
        &release,
    ));
    assert_eq!(check["status"], "available");
    assert_eq!(check["latest_version"], "9.9.9");
    assert_eq!(check["managed"], true);

    let dry_run = json_output(fake_release_env(
        ctx(&temp).args(["upgrade", "--dry-run", "--json"]),
        &release,
    ));
    assert_eq!(dry_run["status"], "dry_run");
    assert_eq!(dry_run["applied"], false);

    let applied = json_output(fake_release_env(
        ctx(&temp).args(["upgrade", "--json"]),
        &release,
    ));
    assert_eq!(applied["status"], "applied");
    assert_eq!(applied["applied"], true);
    assert_eq!(
        fs::read_to_string(&release.target).unwrap(),
        "#!/bin/sh\nprintf 'ctx 9.9.9\\n'\n"
    );
    let marker: Value =
        serde_json::from_slice(&fs::read(install_marker_path(&release.target)).unwrap()).unwrap();
    assert_eq!(marker["version"], "9.9.9");
    assert_eq!(marker["sha256"], release.artifact_sha);
    assert_eq!(marker["install_attempt_id"], "ia_test_upgrade_attempt");
}

#[cfg(unix)]
#[test]
fn upgrade_installs_sidecar_from_signed_release_metadata() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let runtime = add_fake_release_runtime(&temp, &release);

    let applied = json_output(fake_release_env(
        ctx(&temp).args(["upgrade", "--json"]),
        &release,
    ));

    assert_eq!(applied["status"], "applied");
    assert_eq!(
        fs::read_to_string(&release.target).unwrap(),
        "#!/bin/sh\nprintf 'ctx 9.9.9\\n'\n"
    );
    assert_eq!(
        fs::read_to_string(runtime.target.join("VERSION_NUMBER")).unwrap(),
        "1.27.0\n"
    );
    let library = if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    };
    assert!(runtime.target.join("lib").join(library).is_file());
    let manifest: Value =
        serde_json::from_slice(&fs::read(runtime.target.join("ctx-runtime-install.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["manager"], "ctx-hosted-installer");
    assert_eq!(manifest["metadata_trust"], "signed-release-metadata");
    assert_eq!(manifest["sha256"], runtime.artifact_sha);
    assert_eq!(manifest["artifact_url"], file_url(&runtime.artifact));
}

#[cfg(unix)]
#[test]
fn sidecar_hash_failure_leaves_cli_and_runtime_unmodified() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let runtime = add_fake_release_runtime(&temp, &release);
    let before = fs::read(&release.target).unwrap();
    rewrite_fake_release_metadata(&release, |metadata| {
        metadata.replace(
            &format!(
                "CTX_RELEASE_ONNXRUNTIME_SHA256_{}={}\n",
                test_platform_key(),
                runtime.artifact_sha
            ),
            &format!(
                "CTX_RELEASE_ONNXRUNTIME_SHA256_{}={}\n",
                test_platform_key(),
                "f".repeat(64)
            ),
        )
    });

    let stderr = failure_stderr(fake_release_env(
        ctx(&temp).args(["upgrade", "--json"]),
        &release,
    ));

    assert!(stderr.contains("artifact checksum mismatch"), "{stderr}");
    assert_eq!(fs::read(&release.target).unwrap(), before);
    assert!(
        !runtime.target.exists(),
        "failed sidecar verification must not publish a runtime"
    );
}

#[cfg(unix)]
#[test]
fn upgrade_status_accepts_current_legacy_metadata_without_sidecar_fields() {
    let temp = tempdir();
    let release = fake_legacy_release(&temp, env!("CARGO_PKG_VERSION"));

    let outcome = json_output(fake_release_env(
        ctx(&temp).args(["upgrade", "--json"]),
        &release,
    ));

    assert_eq!(outcome["status"], "up_to_date");
    assert!(!temp.path().join("runtime").exists());
}

#[cfg(unix)]
#[test]
fn upgrade_refuses_newer_legacy_metadata_without_sidecar_fields() {
    let temp = tempdir();
    let release = fake_legacy_release(&temp, "9.9.9");

    let stderr = failure_stderr(fake_release_env(
        ctx(&temp).args(["upgrade", "--json"]),
        &release,
    ));

    assert!(
        stderr.contains("has no complete ONNX Runtime sidecar metadata"),
        "{stderr}"
    );
    assert!(!temp.path().join("runtime").exists());
}

#[cfg(unix)]
#[test]
fn upgrade_installs_future_runtime_version_from_target_metadata() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let runtime = add_fake_release_runtime_version(&temp, &release, "1.28.0");

    let applied = json_output(fake_release_env(
        ctx(&temp).args(["upgrade", "--json"]),
        &release,
    ));

    assert_eq!(applied["status"], "applied");
    assert_eq!(
        fs::read_to_string(runtime.target.join("VERSION_NUMBER")).unwrap(),
        "1.28.0\n"
    );
}

#[cfg(unix)]
#[test]
fn signed_runtime_metadata_requires_complete_supported_platform_matrix() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let _runtime = add_fake_release_runtime(&temp, &release);
    rewrite_fake_release_metadata(&release, |metadata| {
        metadata.replace(
            "CTX_RELEASE_ONNXRUNTIME_ARTIFACT_windows_x64=ctx-onnxruntime-windows-x64.zip\n",
            "",
        )
    });

    let stderr = failure_stderr(fake_release_env(
        ctx(&temp).args(["upgrade", "check"]),
        &release,
    ));

    assert!(
        stderr.contains("metadata missing CTX_RELEASE_ONNXRUNTIME_ARTIFACT_windows_x64"),
        "{stderr}"
    );
}

#[cfg(unix)]
#[test]
fn signed_runtime_metadata_rejects_indented_partial_and_malformed_lines() {
    for rewrite in [
        Box::new(|metadata: String| {
            metadata.replace(
                "CTX_RELEASE_ONNXRUNTIME_VERSION=1.27.0",
                " CTX_RELEASE_ONNXRUNTIME_VERSION=1.27.0",
            )
        }) as Box<dyn FnOnce(String) -> String>,
        Box::new(|metadata: String| {
            metadata.replace(
                "CTX_RELEASE_ONNXRUNTIME_VERSION=1.27.0",
                "CTX_RELEASE_ONNXRUNTIME_VERSION 1.27.0",
            )
        }),
        Box::new(|metadata: String| {
            metadata.replace(
                "CTX_RELEASE_ONNXRUNTIME_SHA256_windows_x64=",
                "CTX_RELEASE_ONNXRUNTIME_SHA256_windows_x64_BAD=",
            )
        }),
    ] {
        let temp = tempdir();
        let release = fake_release(&temp, "9.9.9");
        let _runtime = add_fake_release_runtime(&temp, &release);
        rewrite_fake_release_metadata(&release, rewrite);

        let stderr = failure_stderr(fake_release_env(
            ctx(&temp).args(["upgrade", "check"]),
            &release,
        ));

        assert!(
            stderr.contains("metadata contains invalid key")
                || stderr.contains("metadata contains malformed line")
                || stderr.contains("metadata missing CTX_RELEASE_ONNXRUNTIME_SHA256_windows_x64"),
            "{stderr}"
        );
    }
}

#[cfg(unix)]
#[test]
fn signed_runtime_metadata_rejects_unsafe_version_identifiers() {
    for version in ["1.28", "01.28.0", "../1.28.0", "1.28.0 "] {
        let temp = tempdir();
        let release = fake_release(&temp, "9.9.9");
        let _runtime = add_fake_release_runtime(&temp, &release);
        rewrite_fake_release_metadata(&release, |metadata| {
            metadata.replace(
                "CTX_RELEASE_ONNXRUNTIME_VERSION=1.27.0",
                &format!("CTX_RELEASE_ONNXRUNTIME_VERSION={version}"),
            )
        });

        let stderr = failure_stderr(fake_release_env(
            ctx(&temp).args(["upgrade", "check"]),
            &release,
        ));

        assert!(
            stderr.contains("safe MAJOR.MINOR.PATCH identifier"),
            "{version:?}: {stderr}"
        );
    }
}

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
        ("CTX_UPGRADE_ABORT_AFTER_BACKUP_FOR_TESTS", "binary"),
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
            temp.path()
                .join("upgrade-install-transaction.json")
                .is_file(),
            "{injection}={point} did not retain a recovery journal"
        );

        let stderr = failure_stderr(
            fake_release_env(ctx(&temp).args(["upgrade", "--json"]), &release)
                .env("CTX_UPGRADE_STOP_AFTER_RECOVERY_FOR_TESTS", "1"),
        );
        assert!(
            stderr.contains("stopped after interrupted install recovery"),
            "{injection}={point}: {stderr}"
        );
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
            !temp
                .path()
                .join("upgrade-install-transaction.json")
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
fn forged_recovery_journal_fails_closed_without_touching_paths() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let _runtime = add_fake_release_runtime(&temp, &release);
    let sentinel = temp.path().join("must-survive");
    fs::write(&sentinel, "safe\n").unwrap();
    fs::write(
        temp.path().join("upgrade-install-transaction.json"),
        serde_json::to_vec(&json!({
            "schema_version": 1,
            "transaction_id": "forged",
            "phase": "publishing",
            "install_path": sentinel,
            "paths": [
                {
                    "label": "ctx binary",
                    "staged": sentinel.parent().unwrap().join(".ctx-upgrade-forged.new"),
                    "target": sentinel,
                    "backup": sentinel.parent().unwrap().join(".must-survive.ctx-upgrade-forged.binary.previous"),
                    "kind": "file"
                },
                {
                    "label": "ctx install marker",
                    "staged": sentinel.parent().unwrap().join(".ctx-upgrade-forged.install.json.new"),
                    "target": sentinel.parent().unwrap().join("must-survive.install.json"),
                    "backup": sentinel.parent().unwrap().join(".must-survive.install.json.ctx-upgrade-forged.marker.previous"),
                    "kind": "file"
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
        stderr.contains("expected current managed install"),
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
    assert!(!temp
        .path()
        .join("upgrade-install-transaction.json")
        .exists());
}

#[cfg(unix)]
#[test]
fn runtime_installs_at_semantic_discovery_roots() {
    let explicit = tempdir();
    let release = fake_release(&explicit, "9.9.9");
    let _runtime = add_fake_release_runtime(&explicit, &release);
    let runtime_root = explicit.path().join("custom-runtime");
    let applied = json_output(
        fake_release_env(ctx(&explicit).args(["upgrade", "--json"]), &release)
            .env("CTX_RUNTIME_DIR", &runtime_root),
    );
    assert_eq!(applied["status"], "applied");
    assert!(runtime_root
        .join("onnxruntime")
        .join("1.27.0")
        .join(test_platform_key().replace('_', "-"))
        .join("VERSION_NUMBER")
        .is_file());

    let custom_data = tempdir();
    let release = fake_release(&custom_data, "9.9.9");
    let _runtime = add_fake_release_runtime(&custom_data, &release);
    let data_root = custom_data.path().join("custom-data-root");
    let applied = json_output(
        fake_release_env(ctx(&custom_data).args(["upgrade", "--json"]), &release)
            .env("CTX_DATA_ROOT", &data_root),
    );
    assert_eq!(applied["status"], "applied");
    assert!(data_root
        .join("runtime")
        .join("onnxruntime")
        .join("1.27.0")
        .join(test_platform_key().replace('_', "-"))
        .join("VERSION_NUMBER")
        .is_file());
}

#[cfg(unix)]
#[test]
fn runtime_install_honors_cli_selected_data_root() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let _runtime = add_fake_release_runtime(&temp, &release);
    let selected_root = temp.path().join("selected-data-root");
    let unrelated_home = temp.path().join("unrelated-home");
    fs::create_dir(&unrelated_home).unwrap();
    let mut command = ctx(&temp);
    command
        .env_remove("CTX_DATA_ROOT")
        .env("HOME", &unrelated_home)
        .args([
            "--data-root",
            selected_root.to_str().unwrap(),
            "upgrade",
            "--json",
        ]);

    let applied = json_output(fake_release_env(&mut command, &release));

    assert_eq!(applied["status"], "applied");
    assert!(selected_root
        .join("runtime")
        .join("onnxruntime")
        .join("1.27.0")
        .join(test_platform_key().replace('_', "-"))
        .join("VERSION_NUMBER")
        .is_file());
    assert!(!unrelated_home.join(".ctx/runtime").exists());
}

#[cfg(unix)]
#[test]
fn runtime_discovery_roots_reject_relative_and_whitespace_paths() {
    for (key, value, expected) in [
        ("CTX_RUNTIME_DIR", "relative", "must be an absolute path"),
        (
            "CTX_RUNTIME_DIR",
            " /tmp/ctx-runtime",
            "must not be empty or whitespace-padded",
        ),
        ("CTX_DATA_ROOT", "relative", "must be an absolute path"),
        (
            "CTX_DATA_ROOT",
            " /tmp/ctx-data",
            "must not be empty or whitespace-padded",
        ),
    ] {
        let temp = tempdir();
        let release = fake_release(&temp, "9.9.9");
        let _runtime = add_fake_release_runtime(&temp, &release);
        let cli_before = fs::read(&release.target).unwrap();
        let mut command = ctx(&temp);
        command
            .args(["upgrade", "--json"])
            .env(key, value)
            .current_dir(temp.path());
        let stderr = failure_stderr(fake_release_env(&mut command, &release));
        assert!(stderr.contains(expected), "{key}={value:?}: {stderr}");
        assert_eq!(fs::read(&release.target).unwrap(), cli_before);
    }
}

#[cfg(unix)]
#[test]
fn runtime_archive_rejects_traversal_links_specials_and_unexpected_entries() {
    for (mode, expected) in [
        ("traversal", "unsafe or non-canonical runtime archive path"),
        ("symlink", "runtime archive entry is not a regular file"),
        ("special", "runtime archive entry is not a regular file"),
        ("unexpected", "unexpected runtime archive entry"),
        ("duplicate", "duplicate runtime archive entry"),
        ("unsafe_mode", "unsafe permission bits"),
    ] {
        let temp = tempdir();
        let release = fake_release(&temp, "9.9.9");
        let mut runtime = add_fake_release_runtime(&temp, &release);
        rewrite_fake_runtime_archive(&release, &mut runtime, mode);
        let cli_before = fs::read(&release.target).unwrap();

        let stderr = failure_stderr(fake_release_env(
            ctx(&temp).args(["upgrade", "--json"]),
            &release,
        ));

        assert!(stderr.contains(expected), "{mode}: {stderr}");
        assert_eq!(fs::read(&release.target).unwrap(), cli_before);
        assert!(!runtime.target.exists(), "{mode} published a runtime");
        assert!(!temp.path().join("escape").exists(), "{mode} escaped");
    }
}

#[cfg(unix)]
#[test]
fn runtime_archive_rejects_expansion_over_limit_without_partial_install() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let runtime = add_fake_release_runtime(&temp, &release);
    let cli_before = fs::read(&release.target).unwrap();

    let stderr = failure_stderr(
        fake_release_env(ctx(&temp).args(["upgrade", "--json"]), &release)
            .env("CTX_UPGRADE_RUNTIME_MAX_EXPANDED_BYTES_FOR_TESTS", "16"),
    );

    assert!(
        stderr.contains("runtime archive expands beyond the 1 GiB safety limit"),
        "{stderr}"
    );
    assert_eq!(fs::read(&release.target).unwrap(), cli_before);
    assert!(!runtime.target.exists());
}

#[cfg(unix)]
#[test]
fn runtime_extraction_does_not_require_external_python() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let runtime = add_fake_release_runtime(&temp, &release);
    let cli_before = fs::read(&release.target).unwrap();
    let empty_path = temp.path().join("empty-path");
    fs::create_dir(&empty_path).unwrap();

    let applied = json_output(
        fake_release_env(ctx(&temp).args(["upgrade", "--json"]), &release).env("PATH", &empty_path),
    );

    assert_eq!(applied["status"], "applied");
    assert_ne!(fs::read(&release.target).unwrap(), cli_before);
    assert!(runtime.target.join("VERSION_NUMBER").is_file());
}

#[cfg(unix)]
#[test]
fn upgrade_status_text_output_shows_error_details() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");

    let state = json!({
        "schema_version": 1,
        "status": "error",
        "checked_at": "2026-07-10T12:00:00Z",
        "last_checked_unix_s": 1778500000,
        "error": "download artifact: connection refused",
    });
    fs::write(
        temp.path().join("upgrade-state.json"),
        serde_json::to_vec_pretty(&state).unwrap(),
    )
    .unwrap();

    let stdout = {
        let mut command = ctx(&temp);
        command.args(["upgrade", "status"]);
        let assert = fake_release_env(&mut command, &release).assert().success();
        let output = assert.get_output();
        String::from_utf8(output.stdout.clone()).unwrap()
    };

    assert!(
        stdout.contains("ctx upgrade status: error"),
        "status line should be present: {stdout}"
    );
    assert!(
        stdout.contains("download artifact: connection refused"),
        "error details should appear in text output: {stdout}"
    );
}

#[cfg(unix)]
#[test]
fn upgrade_status_reconciles_completed_scheduled_replacement() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    write_fake_ctx_binary(&release.target, "9.9.9");

    let mut marker: Value =
        serde_json::from_slice(&fs::read(install_marker_path(&release.target)).unwrap()).unwrap();
    marker["version"] = Value::String("9.9.9".to_owned());
    marker["sha256"] = Value::String(release.artifact_sha.clone());
    fs::write(
        install_marker_path(&release.target),
        serde_json::to_vec_pretty(&marker).unwrap(),
    )
    .unwrap();
    fs::write(
        temp.path().join("upgrade-state.json"),
        serde_json::to_vec_pretty(&json!({
            "status": "scheduled",
            "current_version": env!("CARGO_PKG_VERSION"),
            "latest_version": "9.9.9",
            "update_available": true,
            "channel": "stable",
            "platform": test_platform_key().replace('_', "-"),
            "install_path": release.target,
            "managed": true
        }))
        .unwrap(),
    )
    .unwrap();

    let status = json_output(fake_release_env(
        ctx(&temp).args(["upgrade", "status", "--json"]),
        &release,
    ));

    assert_eq!(status["state"]["status"], "applied");
    assert_eq!(status["state"]["applied"], true);
    assert_eq!(status["state"]["reconciled_from"], "scheduled");
    assert_eq!(status["install"]["version"], "9.9.9");
}

#[cfg(unix)]
#[test]
fn upgrade_status_reports_path_shadowing() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let shadow_dir = temp.path().join("shadow-bin");
    fs::create_dir_all(&shadow_dir).unwrap();
    let shadow_ctx = shadow_dir.join("ctx");
    write_fake_ctx_binary(&shadow_ctx, "0.9.0");
    let managed_dir = release.target.parent().unwrap();
    let path = std::env::join_paths([shadow_dir.as_path(), managed_dir]).unwrap();

    let mut command = ctx(&temp);
    command
        .args(["upgrade", "status", "--json"])
        .env("PATH", path);
    let status = json_output(fake_release_env(&mut command, &release));

    assert_eq!(status["current_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        status["path"]["entries"][0]["path"],
        shadow_ctx.display().to_string()
    );
    assert!(status["path"]["entries"][0]["version"].is_null());
    assert!(status["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|warning| { warning.as_str().unwrap().contains("PATH resolves ctx to") }));
}

#[cfg(unix)]
#[test]
fn upgrade_commands_do_not_execute_hanging_shadow_path_ctx() {
    for args in [
        ["upgrade", "status", "--json"].as_slice(),
        ["upgrade", "check", "--json"].as_slice(),
        ["upgrade", "--json"].as_slice(),
    ] {
        let temp = tempdir();
        let release = fake_release(&temp, "9.9.9");
        let _runtime = add_fake_release_runtime(&temp, &release);
        let shadow_dir = temp.path().join("shadow-bin");
        fs::create_dir_all(&shadow_dir).unwrap();
        let shadow_ctx = shadow_dir.join("ctx");
        write_hanging_ctx_binary(&shadow_ctx);
        let marker = temp.path().join("shadow-ran");
        let managed_dir = release.target.parent().unwrap();
        let path = std::env::join_paths([shadow_dir.as_path(), managed_dir]).unwrap();

        let mut command = ctx(&temp);
        command
            .args(args)
            .env("PATH", &path)
            .env("CTX_SHADOW_MARKER", &marker);
        let output = json_output(fake_release_env(&mut command, &release));
        assert_eq!(
            output["path"]["entries"][0]["path"],
            shadow_ctx.display().to_string()
        );
        assert!(
            output["path"]["entries"][0]["version"].is_null(),
            "shadow ctx versions should not be probed"
        );
        assert!(
            !marker.exists(),
            "PATH shadow ctx should not have been executed"
        );
    }
}

#[cfg(unix)]
#[test]
fn upgrade_recovers_stale_lock_for_dead_pid() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let _runtime = add_fake_release_runtime(&temp, &release);
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg("exit 0")
        .spawn()
        .unwrap();
    let stale_pid = child.id();
    child.wait().unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    fs::write(
        temp.path().join("upgrade.lock"),
        format!("{stale_pid} {}\n", now.saturating_sub(60)),
    )
    .unwrap();

    let dry_run = json_output(fake_release_env(
        ctx(&temp).args(["upgrade", "--dry-run", "--json"]),
        &release,
    ));

    assert_eq!(dry_run["status"], "dry_run");
    assert!(!temp.path().join("upgrade.lock").exists());
}

#[cfg(unix)]
#[test]
fn upgrade_lock_still_rejects_active_pid() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    fs::write(
        temp.path().join("upgrade.lock"),
        format!("{} {now}\n", std::process::id()),
    )
    .unwrap();

    let stderr = failure_stderr(fake_release_env(
        ctx(&temp).args(["upgrade", "--dry-run"]),
        &release,
    ));

    assert!(stderr.contains("ctx upgrade lock is held"), "{stderr}");
    assert!(temp.path().join("upgrade.lock").exists());
}

#[cfg(unix)]
#[test]
fn upgrade_rejects_unmanaged_install_before_network() {
    let temp = tempdir();
    let stderr = failure_stderr(
        ctx(&temp)
            .args(["upgrade", "--dry-run"])
            .env(
                "CTX_RELEASE_METADATA_URL",
                "file:///definitely/not/a/real/ctx-release-metadata.env",
            )
            .env(
                "CTX_RELEASE_METADATA_SIGNATURE_URL",
                "file:///definitely/not/a/real/ctx-release-metadata.env.sig",
            ),
    );
    assert!(
        stderr.contains("ctx is not installed by the hosted installer"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("download release metadata"),
        "unmanaged installs should fail before metadata fetch: {stderr}"
    );
}

#[cfg(unix)]
#[test]
fn upgrade_verifies_signed_metadata_and_fails_closed() {
    let tampered = tempdir();
    let release = fake_release(&tampered, "9.9.9");
    fs::write(
        &release.metadata,
        format!(
            "{}# tampered after signing\n",
            fs::read_to_string(&release.metadata).unwrap()
        ),
    )
    .unwrap();
    let stderr = failure_stderr(fake_release_env(
        ctx(&tampered).args(["upgrade", "check"]),
        &release,
    ));
    assert!(
        stderr.contains("metadata signature verification failed"),
        "{stderr}"
    );

    let wrong_key = tempdir();
    let release = fake_release(&wrong_key, "9.9.9");
    let stderr = failure_stderr(
        ctx(&wrong_key)
            .args(["upgrade", "check"])
            .env("CTX_UPGRADE_TARGET", &release.target)
            .env("CTX_RELEASE_METADATA_URL", file_url(&release.metadata))
            .env(
                "CTX_RELEASE_METADATA_SIGNATURE_URL",
                file_url(&release.signature),
            ),
    );
    assert!(
        stderr.contains("metadata signature verification failed"),
        "{stderr}"
    );

    let bad_signature = tempdir();
    let release = fake_release(&bad_signature, "9.9.9");
    fs::write(&release.signature, "not-base64").unwrap();
    let stderr = failure_stderr(fake_release_env(
        ctx(&bad_signature).args(["upgrade", "check"]),
        &release,
    ));
    assert!(
        stderr.contains("metadata signature is not base64"),
        "{stderr}"
    );

    let missing_signature = tempdir();
    let release = fake_release(&missing_signature, "9.9.9");
    fs::remove_file(&release.signature).unwrap();
    let stderr = failure_stderr(fake_release_env(
        ctx(&missing_signature).args(["upgrade", "check"]),
        &release,
    ));
    assert!(
        stderr.contains("download release metadata signature"),
        "{stderr}"
    );

    let default_signature_path = tempdir();
    let release = fake_release(&default_signature_path, "9.9.9");
    let check = json_output(
        ctx(&default_signature_path)
            .args(["upgrade", "check", "--json"])
            .env("CTX_UPGRADE_TARGET", &release.target)
            .env("CTX_RELEASE_METADATA_URL", file_url(&release.metadata))
            .env(
                "CTX_RELEASE_METADATA_PUBLIC_KEY_PEM",
                TEST_RELEASE_PUBLIC_KEY_PEM,
            ),
    );
    assert_eq!(check["status"], "available");
}

#[cfg(unix)]
#[test]
fn upgrade_rejects_unsafe_metadata_and_bad_artifacts() {
    let duplicate_key = tempdir();
    let release = fake_release(&duplicate_key, "9.9.9");
    rewrite_fake_release_metadata(&release, |metadata| {
        format!("{metadata}CTX_RELEASE_VERSION=8.8.8\n")
    });
    let stderr = failure_stderr(fake_release_env(
        ctx(&duplicate_key).args(["upgrade", "check"]),
        &release,
    ));
    assert!(
        stderr.contains("metadata contains duplicate key CTX_RELEASE_VERSION"),
        "{stderr}"
    );

    let malformed_bool = tempdir();
    let release = fake_release(&malformed_bool, "9.9.9");
    rewrite_fake_release_metadata(&release, |metadata| {
        metadata.replace(
            "CTX_RELEASE_SELF_UPGRADE_ALLOWED=true\n",
            "CTX_RELEASE_SELF_UPGRADE_ALLOWED=definitely\n",
        )
    });
    let stderr = failure_stderr(fake_release_env(
        ctx(&malformed_bool).args(["upgrade", "check"]),
        &release,
    ));
    assert!(
        stderr.contains("metadata CTX_RELEASE_SELF_UPGRADE_ALLOWED must be a boolean"),
        "{stderr}"
    );

    let missing_policy = tempdir();
    let release = fake_release(&missing_policy, "9.9.9");
    rewrite_fake_release_metadata(&release, |metadata| {
        metadata
            .replace("CTX_RELEASE_SELF_UPGRADE_ALLOWED=true\n", "")
            .replace("CTX_RELEASE_AUTO_UPGRADE_ALLOWED=true\n", "")
    });
    let stderr = failure_stderr(fake_release_env(
        ctx(&missing_policy).args(["upgrade", "--dry-run"]),
        &release,
    ));
    assert!(stderr.contains("does not allow self-upgrade"), "{stderr}");

    let unsafe_artifact = tempdir();
    let release = fake_release(&unsafe_artifact, "9.9.9");
    rewrite_fake_release_metadata(&release, |metadata| {
        metadata.replace(
            &format!("CTX_RELEASE_ARTIFACT_{}=ctx\n", test_platform_key()),
            &format!("CTX_RELEASE_ARTIFACT_{}=../ctx\n", test_platform_key()),
        )
    });
    let stderr = failure_stderr(fake_release_env(
        ctx(&unsafe_artifact).args(["upgrade", "check"]),
        &release,
    ));
    assert!(stderr.contains("unsafe artifact name"), "{stderr}");

    let unsafe_base = tempdir();
    let release = fake_release(&unsafe_base, "9.9.9");
    rewrite_fake_release_metadata(&release, |metadata| {
        metadata.replace(
            "CTX_RELEASE_BASE_URL=file://",
            "CTX_RELEASE_BASE_URL=http://",
        )
    });
    let stderr = failure_stderr(fake_release_env(
        ctx(&unsafe_base).args(["upgrade", "check"]),
        &release,
    ));
    assert!(
        stderr.contains("metadata base URL must be HTTPS"),
        "{stderr}"
    );

    let bad_checksum = tempdir();
    let release = fake_release(&bad_checksum, "9.9.9");
    let _runtime = add_fake_release_runtime(&bad_checksum, &release);
    rewrite_fake_release_metadata(&release, |metadata| {
        metadata.replace(
            &format!(
                "CTX_RELEASE_SHA256_{}={}\n",
                test_platform_key(),
                release.artifact_sha
            ),
            &format!(
                "CTX_RELEASE_SHA256_{}={}\n",
                test_platform_key(),
                "f".repeat(64)
            ),
        )
    });
    let stderr = failure_stderr(fake_release_env(
        ctx(&bad_checksum).args(["upgrade", "--json"]),
        &release,
    ));
    assert!(stderr.contains("artifact checksum mismatch"), "{stderr}");
}

#[cfg(unix)]
#[test]
fn status_json_does_not_spawn_background_upgrade() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");

    let status = json_output(fake_release_env(
        ctx(&temp).args(["status", "--json"]),
        &release,
    ));
    assert_eq!(status["schema_version"], 1);
    assert_eq!(
        fs::read_to_string(&release.target).unwrap(),
        format!("#!/bin/sh\nprintf 'ctx {}\\n'\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(
        !temp.path().join("upgrade-state.json").exists(),
        "JSON status must not start a background upgrade"
    );
}

#[cfg(unix)]
#[test]
fn eligible_json_command_spawns_background_upgrade_without_polluting_output() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let binary = copied_ctx_binary(&temp);
    let before = fs::read(&binary).unwrap();
    let current_sha = sha256_hex(&before);
    fs::write(
        install_marker_path(&binary),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "manager": "ctx-hosted-installer",
            "install_attempt_id": "ia_test_doctor_json_background",
            "install_path": binary.display().to_string(),
            "platform": test_platform_key().replace('_', "-"),
            "channel": "stable",
            "version": env!("CARGO_PKG_VERSION"),
            "sha256": current_sha,
            "metadata_url": null,
            "artifact_url": null,
        }))
        .unwrap(),
    )
    .unwrap();

    let mut command = ctx_from_binary(&temp, &binary);
    let output = command
        .args(["doctor", "--json"])
        .env("CTX_RELEASE_METADATA_URL", file_url(&release.metadata))
        .env(
            "CTX_RELEASE_METADATA_SIGNATURE_URL",
            file_url(&release.signature),
        )
        .env(
            "CTX_RELEASE_METADATA_PUBLIC_KEY_PEM",
            TEST_RELEASE_PUBLIC_KEY_PEM,
        )
        .assert()
        .success()
        .get_output()
        .clone();
    let doctor: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(doctor["schema_version"], 1);
    assert_eq!(
        output.stderr, b"",
        "background upgrade must not write to JSON command stderr"
    );

    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if fs::read_to_string(&binary).unwrap_or_default() == "#!/bin/sh\nprintf 'ctx 9.9.9\\n'\n" {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "eligible JSON command did not apply background upgrade; state: {:?}",
        fs::read_to_string(temp.path().join("upgrade-state.json")).ok()
    );
}

#[cfg(unix)]
#[test]
fn status_command_does_not_spawn_background_upgrade_even_for_managed_installs() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let binary = copied_ctx_binary(&temp);
    let before = fs::read(&binary).unwrap();
    let current_sha = sha256_hex(&before);
    fs::write(
        install_marker_path(&binary),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "manager": "ctx-hosted-installer",
            "install_attempt_id": "ia_test_status_no_background",
            "install_path": binary.display().to_string(),
            "platform": test_platform_key().replace('_', "-"),
            "channel": "stable",
            "version": env!("CARGO_PKG_VERSION"),
            "sha256": current_sha,
            "metadata_url": null,
            "artifact_url": null,
        }))
        .unwrap(),
    )
    .unwrap();

    ctx_from_binary(&temp, &binary)
        .arg("status")
        .env("CTX_RELEASE_METADATA_URL", file_url(&release.metadata))
        .env(
            "CTX_RELEASE_METADATA_SIGNATURE_URL",
            file_url(&release.signature),
        )
        .env(
            "CTX_RELEASE_METADATA_PUBLIC_KEY_PEM",
            TEST_RELEASE_PUBLIC_KEY_PEM,
        )
        .assert()
        .success()
        .stdout(predicate::str::contains("read_only: true"));

    std::thread::sleep(Duration::from_millis(1_000));
    assert_eq!(
        fs::read(&binary).unwrap(),
        before,
        "status must not spawn a background upgrade that replaces the binary"
    );
    assert!(
        !temp.path().join("upgrade-state.json").exists(),
        "status must not write background upgrade state"
    );
}
