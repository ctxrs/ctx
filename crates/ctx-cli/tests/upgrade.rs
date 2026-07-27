mod support;

#[cfg(any(unix, windows))]
use support::*;

#[cfg(unix)]
fn scheduler_state_path(binary: &Path) -> PathBuf {
    binary.with_file_name(".ctx.upgrade-state.json")
}

#[cfg(unix)]
fn installation_lock_path(binary: &Path) -> PathBuf {
    binary.with_file_name(".ctx.install.lock")
}

#[cfg(unix)]
fn ctx_with_umask(temp: &TempDir, mask: &str) -> Command {
    let program = ctx(temp).get_program().to_owned();
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", &format!("umask {mask}; exec \"$0\" \"$@\"")])
        .arg(program);
    apply_hermetic_env(&mut command, temp);
    command
}

#[cfg(unix)]
fn assert_mode(path: &Path, expected: u32) {
    use std::os::unix::fs::PermissionsExt as _;

    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        expected,
        "unexpected mode for {}",
        path.display()
    );
}

#[test]
fn windows_runtime_extractor_keeps_external_source_contract() {
    let installer_source = include_str!("../src/upgrade/install.rs");
    let declaration = "const EXTRACT_SCRIPT: &str = r#\"\n";
    assert_eq!(
        installer_source.matches(declaration).count(),
        1,
        "the external PowerShell test expects one embedded extractor at upgrade/install.rs"
    );
    let extractor = installer_source
        .split_once(declaration)
        .unwrap()
        .1
        .split_once("\n\"#;")
        .unwrap()
        .0;
    assert!(extractor.contains("[System.IO.Compression.ZipFile]::OpenRead($ArchivePath)"));
    assert!(extractor.contains("$targetStream.Flush($true)"));

    let external_contract =
        include_str!("../../../scripts/test-windows-runtime-upgrade-extractor.ps1");
    assert!(external_contract.contains(r#"..\crates\ctx-cli\src\upgrade\install.rs"#));
    assert!(external_contract.contains("const EXTRACT_SCRIPT: &str = r#"));
}

#[cfg(unix)]
#[test]
fn upgrade_enable_and_disable_persist_private_config_with_analytics_disabled() {
    for (command_name, expected_mode) in [("enable", "apply"), ("disable", "off")] {
        let temp = tempdir();
        let first = temp.path().join(format!("{command_name}-state"));
        let second = first.join("nested");
        let data_root = second.join("ctx");

        ctx_with_umask(&temp, "022")
            .args(["upgrade", command_name])
            .env("CTX_DATA_ROOT", &data_root)
            .env("CTX_ANALYTICS_ENABLED", "false")
            .assert()
            .success();

        assert_mode(&first, 0o700);
        assert_mode(&second, 0o700);
        assert_mode(&data_root, 0o700);
        let config = data_root.join("config.toml");
        assert_mode(&config, 0o600);
        assert_eq!(
            fs::read_to_string(config).unwrap(),
            format!("[upgrade]\nauto = \"{expected_mode}\"\n")
        );
    }
}

#[cfg(unix)]
#[test]
fn upgrade_enable_and_disable_reject_insecure_config_without_repair() {
    use std::os::unix::fs::PermissionsExt as _;

    for command_name in ["enable", "disable"] {
        let temp = tempdir();
        let data_root = temp.path().join(format!("{command_name}-state"));
        let config = data_root.join("config.toml");
        let original = b"[search]\nsemantic = true\n";
        fs::create_dir(&data_root).unwrap();
        fs::set_permissions(&data_root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(&config, original).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o666)).unwrap();

        let stderr = failure_stderr(
            ctx(&temp)
                .args(["upgrade", command_name])
                .env("CTX_DATA_ROOT", &data_root)
                .env("CTX_ANALYTICS_ENABLED", "false"),
        );

        assert!(
            stderr.contains("private state path is not owner-only"),
            "{command_name}: {stderr}"
        );
        assert_eq!(fs::read(&config).unwrap(), original, "{command_name}");
        assert_mode(&config, 0o666);
    }
}

#[cfg(windows)]
#[test]
fn upgrade_enable_creates_protected_nested_root_and_config_on_windows() {
    use ctx_history_core::platform_security::{verify_private_directory, verify_private_file};

    let temp = tempdir();
    let first = temp.path().join("upgrade-state");
    let nested = first.join("nested");
    let data_root = nested.join("ctx");

    ctx(&temp)
        .args(["upgrade", "enable"])
        .env("CTX_DATA_ROOT", &data_root)
        .env("CTX_ANALYTICS_ENABLED", "false")
        .assert()
        .success();

    verify_private_directory(&first).unwrap();
    verify_private_directory(&nested).unwrap();
    verify_private_directory(&data_root).unwrap();
    verify_private_file(&data_root.join("config.toml")).unwrap();
}

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
    let checked_state: Value =
        serde_json::from_slice(&fs::read(scheduler_state_path(&release.target)).unwrap()).unwrap();
    assert!(checked_state["checked_at"].as_str().is_some());
    assert!(checked_state["last_checked_unix_s"].as_u64().is_some());
    assert_eq!(checked_state["metadata_url"], file_url(&release.metadata));
    assert_eq!(
        checked_state["artifact_url"],
        file_url(&release.metadata.parent().unwrap().join("ctx"))
    );

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
    assert_eq!(marker["installed_at"], release.installed_at);
}

#[cfg(unix)]
#[test]
fn upgrade_drops_expired_install_attribution_instead_of_reopening_it() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let marker_path = install_marker_path(&release.target);
    let mut marker: Value = serde_json::from_slice(&fs::read(&marker_path).unwrap()).unwrap();
    marker["installed_at"] = json!("2020-01-01T00:00:00Z");
    fs::write(&marker_path, serde_json::to_vec_pretty(&marker).unwrap()).unwrap();

    let applied = json_output(fake_release_env(
        ctx(&temp).args(["upgrade", "--json"]),
        &release,
    ));
    assert_eq!(applied["status"], "applied");

    let upgraded: Value = serde_json::from_slice(&fs::read(&marker_path).unwrap()).unwrap();
    assert!(upgraded.get("install_attempt_id").is_none());
    assert_ne!(upgraded["installed_at"], "2020-01-01T00:00:00Z");
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
    fs::create_dir(&data_root).unwrap();
    ctx_history_core::platform_security::restrict_private_directory(&data_root).unwrap();
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
    fs::create_dir(&selected_root).unwrap();
    ctx_history_core::platform_security::restrict_private_directory(&selected_root).unwrap();
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
fn v025_legacy_runtime_recovery_requires_and_honors_the_original_custom_root() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let custom_runtime = temp.path().join("legacy-custom-runtime");
    let platform = test_platform_key().replace('_', "-");
    let runtime_target = custom_runtime
        .join("onnxruntime")
        .join("1.27.0")
        .join(&platform);
    fs::create_dir_all(&runtime_target).unwrap();
    fs::write(runtime_target.join("VERSION_NUMBER"), b"1.27.0\n").unwrap();
    let transaction_id = "legacy-runtime";
    let binary_name = release.target.file_name().unwrap().to_str().unwrap();
    let marker_path = install_marker_path(&release.target);
    let marker_name = marker_path.file_name().unwrap().to_str().unwrap();
    let runtime_name = runtime_target.file_name().unwrap().to_str().unwrap();
    let journal = json!({
        "schema_version": 1,
        "transaction_id": transaction_id,
        "phase": "committed",
        "install_path": release.target,
        "paths": [
            {
                "label": "ONNX Runtime sidecar",
                "staged": runtime_target.with_file_name(format!(".{runtime_name}.ctx-upgrade-{transaction_id}.new")),
                "target": runtime_target,
                "backup": runtime_target.with_file_name(format!(".{runtime_name}.ctx-upgrade-{transaction_id}.runtime.previous")),
                "kind": "directory"
            },
            {
                "label": "ctx binary",
                "staged": release.target.with_file_name(format!(".ctx-upgrade-{transaction_id}.new")),
                "target": release.target,
                "backup": release.target.with_file_name(format!(".{binary_name}.ctx-upgrade-{transaction_id}.binary.previous")),
                "kind": "file"
            },
            {
                "label": "ctx install marker",
                "staged": marker_path.with_file_name(format!(".ctx-upgrade-{transaction_id}.install.json.new")),
                "target": marker_path,
                "backup": marker_path.with_file_name(format!(".{marker_name}.ctx-upgrade-{transaction_id}.marker.previous")),
                "kind": "file"
            }
        ]
    });
    let legacy_journal = temp.path().join("upgrade-install-transaction.json");
    fs::write(
        &legacy_journal,
        serde_json::to_vec_pretty(&journal).unwrap(),
    )
    .unwrap();

    let missing_root = fake_release_env(ctx(&temp).args(["upgrade", "check", "--json"]), &release)
        .output()
        .unwrap();
    assert!(!missing_root.status.success(), "{missing_root:?}");
    assert!(String::from_utf8_lossy(&missing_root.stderr).contains("invalid runtime paths"));
    assert!(legacy_journal.exists());

    let recovered = fake_release_env(ctx(&temp).args(["upgrade", "check", "--json"]), &release)
        .env("CTX_RUNTIME_DIR", &custom_runtime)
        .output()
        .unwrap();
    assert!(recovered.status.success(), "{recovered:?}");
    assert!(!legacy_journal.exists());
}

#[cfg(unix)]
#[test]
fn cross_root_recovery_uses_one_installation_state_and_the_validated_origin_root() {
    let owner = tempdir();
    let release = fake_release(&owner, "9.9.9");
    let _runtime = add_fake_release_runtime(&owner, &release);
    let origin_root = tempdir();
    let discovering_root = tempdir();

    let interrupted = fake_release_env(ctx(&origin_root).args(["upgrade", "--json"]), &release)
        .env("CTX_UPGRADE_ABORT_AFTER_BACKUP_FOR_TESTS", "binary")
        .output()
        .unwrap();
    assert!(!interrupted.status.success(), "{interrupted:?}");
    assert!(release
        .target
        .with_file_name(".ctx.upgrade-install-transaction.json")
        .exists());
    let state_path = scheduler_state_path(&release.target);
    assert!(state_path.exists());

    let discovered = fake_release_env(
        ctx(&discovering_root).args(["upgrade", "check", "--json"]),
        &release,
    )
    .output()
    .unwrap();
    assert!(discovered.status.success(), "{discovered:?}");
    assert!(!release
        .target
        .with_file_name(".ctx.upgrade-install-transaction.json")
        .exists());
    let state: Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    assert_eq!(state["attempt_source"], "upgrade_check");
    assert_eq!(state["status"], "available");
    assert!(!origin_root.path().join("upgrade-state.json").exists());
    assert!(!discovering_root.path().join("upgrade-state.json").exists());
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
        scheduler_state_path(&release.target),
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
fn upgrade_status_bridges_v025_data_root_state_read_only() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let legacy_path = temp.path().join("upgrade-state.json");
    let legacy = serde_json::to_vec_pretty(&json!({
        "schema_version": 1,
        "status": "available",
        "checked_at": "2026-07-10T12:00:00Z",
        "last_checked_unix_s": 1778500000_u64,
        "current_version": "0.25.0",
        "latest_version": "9.9.9",
        "update_available": true,
        "channel": "stable",
        "platform": test_platform_key().replace('_', "-"),
        "metadata_url": file_url(&release.metadata),
        "artifact_url": file_url(&release.metadata.parent().unwrap().join("ctx")),
        "install_path": release.target,
        "managed": true,
    }))
    .unwrap();
    fs::write(&legacy_path, &legacy).unwrap();

    let status = json_output(fake_release_env(
        ctx(&temp).args(["upgrade", "status", "--json"]),
        &release,
    ));

    assert_eq!(status["state"]["schema_version"], 1);
    assert_eq!(status["state"]["checked_at"], "2026-07-10T12:00:00Z");
    assert_eq!(status["state"]["last_checked_unix_s"], 1778500000_u64);
    assert_eq!(status["state"]["metadata_url"], file_url(&release.metadata));
    assert_eq!(
        status["state"]["artifact_url"],
        file_url(&release.metadata.parent().unwrap().join("ctx"))
    );
    assert_eq!(fs::read(&legacy_path).unwrap(), legacy);
    assert!(!scheduler_state_path(&release.target).exists());
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
        scheduler_state_path(&release.target),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
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
fn persistent_installation_lock_ignores_text_when_no_os_owner_exists() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let _runtime = add_fake_release_runtime(&temp, &release);
    let lock_path = installation_lock_path(&release.target);
    fs::write(&lock_path, "stale pid-looking text\n").unwrap();

    let dry_run = json_output(fake_release_env(
        ctx(&temp).args(["upgrade", "--dry-run", "--json"]),
        &release,
    ));

    assert_eq!(dry_run["status"], "dry_run");
    assert!(lock_path.exists());
}

#[cfg(unix)]
#[test]
fn upgrade_lock_rejects_a_live_os_lock_owner() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let lock_path = installation_lock_path(&release.target);
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(&lock_path)
        .unwrap();
    assert_eq!(
        unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&lock), libc::LOCK_EX) },
        0
    );

    let stderr = failure_stderr(fake_release_env(
        ctx(&temp).args(["upgrade", "--dry-run"]),
        &release,
    ));

    assert!(
        stderr.contains("ctx installation upgrade lock is held"),
        "{stderr}"
    );
    assert!(lock_path.exists());
    assert_eq!(
        unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&lock), libc::LOCK_UN) },
        0
    );
}

#[cfg(unix)]
#[test]
fn upgrade_rejects_unmanaged_install_before_network() {
    let temp = tempdir();
    let binary = managed_candidate(&temp, "ia_removed_unmanaged_marker");
    fs::remove_file(install_marker_path(&binary)).unwrap();
    let stderr = failure_stderr(
        ctx_from_binary(&temp, &binary)
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
            .env("CTX_UPGRADE_TEST_TARGET", &release.target)
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
            .env("CTX_UPGRADE_TEST_TARGET", &release.target)
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
