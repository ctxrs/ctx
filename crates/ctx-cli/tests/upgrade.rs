mod support;

#[cfg(any(unix, windows))]
use support::*;

#[path = "support/upgrade/runtime_publication.rs"]
mod runtime_publication;

#[path = "upgrade/release_validation.rs"]
mod release_validation;

#[cfg(unix)]
use std::{
    net::TcpListener,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    thread,
};

#[cfg(unix)]
fn scheduler_state_path(binary: &Path) -> PathBuf {
    binary.with_file_name(".ctx.upgrade-state.json")
}

#[cfg(unix)]
struct MetadataRequestProbe {
    endpoint: String,
    requests: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

#[cfg(unix)]
impl MetadataRequestProbe {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_requests = Arc::clone(&requests);
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    worker_requests.fetch_add(1, Ordering::SeqCst);
                    let _ = stream.write_all(
                        b"HTTP/1.1 404 Not Found\r\n\
                          Content-Length: 0\r\n\
                          Connection: close\r\n\r\n",
                    );
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if worker_stop.load(Ordering::SeqCst) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                Err(_) => break,
            }
        });
        Self {
            endpoint,
            requests,
            stop,
            worker: Some(worker),
        }
    }

    fn finish(mut self) -> usize {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
        self.requests.load(Ordering::SeqCst)
    }
}

#[cfg(unix)]
impl Drop for MetadataRequestProbe {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(unix)]
const INSTALLER_UPGRADE_ENV: &[&str] = &[
    "CTX_ALLOW_CUSTOM_RELEASE_BASE_URL",
    "CTX_FUNCTIONS_BASE",
    "CTX_RELEASE_METADATA_PUBLIC_KEY_PEM",
    "CTX_RELEASE_METADATA_SIGNATURE_URL",
    "CTX_RELEASE_METADATA_URL",
    "CTX_RELEASE_SKIP_SIGNATURE_VERIFY_FOR_TESTS",
    "CTX_UPGRADE_AUTO",
    "CTX_UPGRADE_CHANNEL",
    "CTX_UPGRADE_FUNCTIONS_BASE",
];

#[cfg(unix)]
fn remove_installer_upgrade_env(command: &mut Command) {
    for name in INSTALLER_UPGRADE_ENV {
        command.env_remove(name);
    }
}

#[cfg(unix)]
fn write_probe_config(temp: &TempDir) {
    fs::write(
        temp.path().join("config.toml"),
        "[analytics]\n\
         enabled = false\n\
         [upgrade]\n\
         auto = \"apply\"\n\
         channel = \"stable\"\n",
    )
    .unwrap();
}

#[cfg(unix)]
fn mark_staging_dogfood(binary: &Path) {
    let marker_path = install_marker_path(binary);
    let mut marker: Value = serde_json::from_slice(&fs::read(&marker_path).unwrap()).unwrap();
    marker["channel"] = json!("dogfood-persistent-isolation");
    marker["staging_dogfood"] = json!(true);
    fs::write(marker_path, serde_json::to_vec_pretty(&marker).unwrap()).unwrap();
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
        ctx(&temp).args(["upgrade", "status", "--format=json"]),
        &release,
    ));
    assert_eq!(status["schema_version"], 1);
    assert_eq!(status["install"]["managed"], true);

    let check = json_output(fake_release_env(
        ctx(&temp).args(["upgrade", "check", "--format=json"]),
        &release,
    ));
    assert_eq!(check["status"], "available");
    assert_eq!(check["latest_version"], "9.9.9");
    assert_eq!(check["managed"], true);
    let checked_state: Value =
        serde_json::from_slice(&fs::read(scheduler_state_path(&release.target)).unwrap()).unwrap();
    assert_eq!(checked_state["schema_version"], 1);
    assert_eq!(checked_state["status"], "available");
    assert_eq!(checked_state["attempt_source"], "upgrade_check");
    assert!(checked_state["last_attempt_at"].as_str().is_some());
    assert!(checked_state["last_attempt_finished_at"].as_str().is_some());
    assert!(checked_state["checked_at"].as_str().is_some());
    assert!(checked_state["last_checked_unix_s"].as_u64().is_some());
    assert_eq!(checked_state["metadata_url"], file_url(&release.metadata));
    assert_eq!(
        checked_state["artifact_url"],
        file_url(&release.metadata.parent().unwrap().join("ctx"))
    );

    let dry_run = json_output(fake_release_env(
        ctx(&temp).args(["upgrade", "--dry-run", "--format=json"]),
        &release,
    ));
    assert_eq!(dry_run["status"], "dry_run");
    assert_eq!(dry_run["applied"], false);

    let applied = json_output(fake_release_env(
        ctx(&temp).args(["upgrade", "--format=json"]),
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
        ctx(&temp).args(["upgrade", "--format=json"]),
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
        ctx(&temp).args(["upgrade", "--format=json"]),
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
        ctx(&temp).args(["upgrade", "--format=json"]),
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
fn upgrade_integrity_failure_has_safe_human_receipt_and_unchanged_machine_error() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let before = fs::read(&release.target).unwrap();
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

    let human_output = fake_release_env(ctx(&temp).args(["upgrade"]), &release)
        .assert()
        .failure()
        .get_output()
        .clone();
    assert_eq!(human_output.status.code(), Some(1));
    let human_stderr = String::from_utf8(human_output.stderr).unwrap();
    assert!(
        human_stderr.contains("Upgrade integrity check failed"),
        "{human_stderr}"
    );
    assert!(
        human_stderr.contains("did not match signed release metadata"),
        "{human_stderr}"
    );
    assert!(
        human_stderr.contains("installed ctx version was not changed"),
        "{human_stderr}"
    );
    assert!(human_stderr.contains("ctx upgrade"), "{human_stderr}");
    assert!(!human_stderr.contains("file://"), "{human_stderr}");
    assert!(!human_stderr.contains(&"f".repeat(64)), "{human_stderr}");
    assert!(
        !human_stderr.contains(&release.artifact_sha),
        "{human_stderr}"
    );
    assert_eq!(fs::read(&release.target).unwrap(), before);

    let machine_output = fake_release_env(ctx(&temp).args(["upgrade", "--format=json"]), &release)
        .assert()
        .failure()
        .get_output()
        .clone();
    assert_eq!(machine_output.status.code(), Some(1));
    let machine_stderr = String::from_utf8(machine_output.stderr).unwrap();
    assert!(
        machine_stderr.contains("artifact checksum mismatch"),
        "{machine_stderr}"
    );
    assert!(machine_stderr.contains("expected"), "{machine_stderr}");
    assert!(
        !machine_stderr.contains("Upgrade integrity check failed"),
        "{machine_stderr}"
    );
    assert_eq!(fs::read(&release.target).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn upgrade_status_accepts_current_legacy_metadata_without_sidecar_fields() {
    let temp = tempdir();
    let release = fake_legacy_release(&temp, env!("CARGO_PKG_VERSION"));

    let outcome = json_output(fake_release_env(
        ctx(&temp).args(["upgrade", "--format=json"]),
        &release,
    ));

    assert_eq!(outcome["status"], "up_to_date");
    assert!(!data_root(&temp).join("runtime").exists());
}

#[cfg(unix)]
#[test]
fn upgrade_refuses_newer_legacy_metadata_without_sidecar_fields() {
    let temp = tempdir();
    let release = fake_legacy_release(&temp, "9.9.9");

    let stderr = failure_stderr(fake_release_env(
        ctx(&temp).args(["upgrade", "--format=json"]),
        &release,
    ));

    assert!(
        stderr.contains("has no complete ONNX Runtime sidecar metadata"),
        "{stderr}"
    );
    assert!(!data_root(&temp).join("runtime").exists());
}

#[cfg(unix)]
#[test]
fn upgrade_installs_future_runtime_version_from_target_metadata() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let runtime = add_fake_release_runtime_version(&temp, &release, "1.28.0");

    let applied = json_output(fake_release_env(
        ctx(&temp).args(["upgrade", "--format=json"]),
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
fn runtime_installs_at_semantic_discovery_roots() {
    let explicit = tempdir();
    let release = fake_release(&explicit, "9.9.9");
    let _runtime = add_fake_release_runtime(&explicit, &release);
    let runtime_root = explicit.path().join("custom-runtime");
    let applied = json_output(
        fake_release_env(ctx(&explicit).args(["upgrade", "--format=json"]), &release)
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
        fake_release_env(
            ctx(&custom_data).args(["upgrade", "--format=json"]),
            &release,
        )
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
            "--format=json",
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
fn v025_data_root_journal_is_ignored_without_binary_or_runtime_reexecution() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let old_runtime_root = temp.path().join("old-runtime");
    let platform = test_platform_key().replace('_', "-");
    let runtime_target = old_runtime_root
        .join("onnxruntime")
        .join("1.27.0")
        .join(&platform);
    fs::create_dir_all(&runtime_target).unwrap();
    fs::write(runtime_target.join("VERSION_NUMBER"), b"old runtime\n").unwrap();
    let transaction_id = "v025-runtime";
    let binary_name = release.target.file_name().unwrap().to_str().unwrap();
    let marker_path = install_marker_path(&release.target);
    let marker_name = marker_path.file_name().unwrap().to_str().unwrap();
    let runtime_name = runtime_target.file_name().unwrap().to_str().unwrap();
    let binary_backup = release.target.with_file_name(format!(
        ".{binary_name}.ctx-upgrade-{transaction_id}.binary.previous"
    ));
    fs::write(&binary_backup, b"v0.25 binary backup").unwrap();
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
                "backup": binary_backup,
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
    let journal_bytes = serde_json::to_vec_pretty(&journal).unwrap();
    fs::write(&legacy_journal, &journal_bytes).unwrap();
    let binary_before = fs::read(&release.target).unwrap();

    let checked = fake_release_env(
        ctx(&temp).args(["upgrade", "check", "--format=json"]),
        &release,
    )
    .output()
    .unwrap();
    assert!(checked.status.success(), "{checked:?}");
    assert_eq!(fs::read(&legacy_journal).unwrap(), journal_bytes);
    assert_eq!(fs::read(&binary_backup).unwrap(), b"v0.25 binary backup");
    assert_eq!(fs::read(&release.target).unwrap(), binary_before);
    assert_eq!(
        fs::read(runtime_target.join("VERSION_NUMBER")).unwrap(),
        b"old runtime\n"
    );
}

#[cfg(unix)]
#[test]
fn cross_root_recovery_uses_one_installation_state_and_the_validated_origin_root() {
    let owner = tempdir();
    let release = fake_release(&owner, "9.9.9");
    let _runtime = add_fake_release_runtime(&owner, &release);
    let origin_root = tempdir();
    let discovering_root = tempdir();

    let interrupted = fake_release_env(
        ctx(&origin_root).args(["upgrade", "--format=json"]),
        &release,
    )
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
        ctx(&discovering_root).args(["upgrade", "check", "--format=json"]),
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
    assert_eq!(state["schema_version"], 1);
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
            .args(["upgrade", "--format=json"])
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
            ctx(&temp).args(["upgrade", "--format=json"]),
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
        fake_release_env(ctx(&temp).args(["upgrade", "--format=json"]), &release)
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
        fake_release_env(ctx(&temp).args(["upgrade", "--format=json"]), &release)
            .env("PATH", &empty_path),
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
        stdout.contains("Upgrade needs attention"),
        "error outcome should be present: {stdout}"
    );
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("State") && line.ends_with("error")),
        "structured error state should be present: {stdout}"
    );
    assert!(
        stdout.contains("download artifact: connection refused"),
        "error details should appear in text output: {stdout}"
    );
}

#[cfg(unix)]
#[test]
fn upgrade_status_ignores_v025_data_root_state() {
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
        ctx(&temp).args(["upgrade", "status", "--format=json"]),
        &release,
    ));

    assert_eq!(status["state"]["schema_version"], 1);
    assert_eq!(status["state"]["status"], "never_checked");
    assert!(status["state"].get("current_version").is_none());
    assert!(status["state"].get("update_available").is_none());
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
        ctx(&temp).args(["upgrade", "status", "--format=json"]),
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
        .args(["upgrade", "status", "--format=json"])
        .env("PATH", &path);
    let status = json_output(fake_release_env(&mut command, &release));

    assert_eq!(status["schema_version"], 1);
    assert_eq!(status["current_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        status["path"]["entries"][0]["path"],
        shadow_ctx.display().to_string()
    );
    assert!(status["path"]["entries"][0]["version"].is_null());
    assert_eq!(status["path"]["resolver_status"], "shadowed");
    assert_eq!(status["path"]["background_apply"]["allowed"], false);
    assert_eq!(
        status["path"]["background_apply"]["reason"],
        "path_shadowed"
    );
    assert_eq!(status["warnings"], status["path"]["warnings"]);
    assert_eq!(status["warnings"].as_array().unwrap().len(), 2);
    assert!(status["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|warning| { warning.as_str().unwrap().contains("PATH resolves ctx to") }));

    let managed_ctx = status["path"]["current_exe"].as_str().unwrap();
    let mut command = ctx(&temp);
    command.args(["upgrade", "status"]).env("PATH", &path);
    let human_output = fake_release_env(&mut command, &release)
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8(human_output.stdout).unwrap();
    let stderr = String::from_utf8(human_output.stderr).unwrap();
    assert!(
        stdout.contains("A different ctx takes precedence on PATH"),
        "{stdout}"
    );
    assert!(
        stdout.contains(&shadow_ctx.display().to_string()),
        "{stdout}"
    );
    assert!(stdout.contains(managed_ctx), "{stdout}");
    assert!(
        stdout.contains("Automatic upgrades are blocked"),
        "{stdout}"
    );
    assert!(
        stdout.contains("shell will keep running the shadowing ctx"),
        "{stdout}"
    );
    assert!(stdout.contains("ctx upgrade enable"), "{stdout}");
    assert!(stderr.is_empty(), "{stderr}");
    assert!(!stdout.contains("PATH resolves ctx to"), "{stdout}");
    assert!(
        !stdout.contains("multiple ctx binaries are on PATH"),
        "{stdout}"
    );
}

#[cfg(unix)]
#[test]
fn upgrade_status_preserves_non_shadow_path_warning() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let empty_path = temp.path().join("empty-path");
    fs::create_dir_all(&empty_path).unwrap();

    let mut command = ctx(&temp);
    command.args(["upgrade", "status"]).env("PATH", &empty_path);
    let output = fake_release_env(&mut command, &release)
        .assert()
        .success()
        .get_output()
        .clone();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("not discoverable on PATH"), "{stderr}");
}

#[cfg(unix)]
#[test]
fn upgrade_commands_do_not_execute_hanging_shadow_path_ctx() {
    for args in [
        ["upgrade", "status", "--format=json"].as_slice(),
        ["upgrade", "check", "--format=json"].as_slice(),
        ["upgrade", "--format=json"].as_slice(),
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
        ctx(&temp).args(["upgrade", "--dry-run", "--format=json"]),
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
        .truncate(false)
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

#[cfg(all(unix, not(debug_assertions)))]
#[test]
fn ordinary_release_binary_ignores_upgrade_test_harness_authority() {
    let Some(binary) = std::env::var_os("CTX_ORDINARY_RELEASE_BINARY").map(PathBuf::from) else {
        return;
    };
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let stderr = failure_stderr(fake_release_env(
        ctx_from_binary(&temp, &binary).args(["upgrade", "check"]),
        &release,
    ));
    let ordinary_marker = hosted_install_marker_path(&fs::canonicalize(&binary).unwrap());

    assert!(
        stderr.contains(&format!(
            "read ctx install marker {}",
            ordinary_marker.display()
        )),
        "{stderr}"
    );
    assert!(
        !stderr.contains(&install_marker_path(&release.target).display().to_string())
            && !stderr.contains("download release metadata")
            && !stderr.contains("metadata signature"),
        "ordinary release binary accepted test-harness target authority: {stderr}"
    );
}

#[cfg(unix)]
#[test]
fn staging_dogfood_marker_survives_fresh_process_and_blocks_stable_metadata() {
    let temp = tempdir();
    let binary = managed_candidate(&temp, "ia_staging_dogfood_isolation");
    mark_staging_dogfood(&binary);
    let probe = MetadataRequestProbe::start();
    write_probe_config(&temp);

    let mut status_command = ctx_from_binary(&temp, &binary);
    remove_installer_upgrade_env(&mut status_command);
    let status = json_output(status_command.args(["upgrade", "status", "--format=json"]));
    assert_eq!(status["install"]["managed"], true);
    assert_eq!(status["auto_upgrade"]["mode"], "off");
    assert_eq!(status["auto_upgrade"]["enabled"], false);

    let mut check_command = ctx_from_binary(&temp, &binary);
    remove_installer_upgrade_env(&mut check_command);
    let stderr = failure_stderr(
        check_command
            .args(["upgrade", "check"])
            .env("CTX_RELEASE_METADATA_URL", &probe.endpoint),
    );
    assert!(
        stderr.contains("staging dogfood ctx installation is isolated from release upgrades"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("download release metadata"),
        "staging isolation must fail before metadata download: {stderr}"
    );
    assert_eq!(
        probe.finish(),
        0,
        "staging isolation contacted the configured stable metadata endpoint"
    );
}

#[cfg(unix)]
#[test]
fn ordinary_hosted_install_marker_keeps_production_upgrade_behavior() {
    let temp = tempdir();
    let binary = managed_candidate(&temp, "ia_ordinary_hosted_install");
    let probe = MetadataRequestProbe::start();
    write_probe_config(&temp);

    let mut status_command = ctx_from_binary(&temp, &binary);
    remove_installer_upgrade_env(&mut status_command);
    let status = json_output(status_command.args(["upgrade", "status", "--format=json"]));
    assert_eq!(status["install"]["managed"], true);
    assert_eq!(status["auto_upgrade"]["mode"], "apply");
    assert_eq!(status["auto_upgrade"]["enabled"], true);

    let mut check_command = ctx_from_binary(&temp, &binary);
    remove_installer_upgrade_env(&mut check_command);
    let stderr = failure_stderr(
        check_command
            .args(["upgrade", "check"])
            .env("CTX_RELEASE_METADATA_URL", &probe.endpoint),
    );
    assert!(
        stderr.contains("download release metadata"),
        "ordinary hosted installs must retain normal metadata planning: {stderr}"
    );
    assert!(
        probe.finish() > 0,
        "ordinary hosted install did not request stable metadata"
    );
}
