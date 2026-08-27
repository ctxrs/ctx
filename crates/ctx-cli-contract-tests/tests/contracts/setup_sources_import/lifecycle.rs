use super::{
    assert_daemon_process_running, assert_daemon_process_running_with_status,
    assert_no_daemon_autostart_mutation, ctx, support, support::*, wait_for_daemon_status,
    write_codex_setup_session,
};

#[path = "../support/setup_sources_import/lifecycle_helpers.rs"]
mod lifecycle_helpers;
use lifecycle_helpers::*;

#[cfg(target_os = "linux")]
fn install_managed_test_marker(binary: &std::path::Path) {
    use sha2::{Digest as _, Sha256};

    const MAX_MANAGED_BINARY_BYTES: u64 = 128 * 1024 * 1024;
    if fs::metadata(binary).unwrap().len() > MAX_MANAGED_BINARY_BYTES {
        let stripped = std::process::Command::new("strip")
            .arg(binary)
            .status()
            .expect("strip temporary managed setup binary");
        assert!(stripped.success(), "strip temporary managed setup binary");
    }
    let binary = fs::canonicalize(binary).unwrap();
    let body = fs::read(&binary).unwrap();
    assert!(
        body.len() <= MAX_MANAGED_BINARY_BYTES as usize,
        "managed setup acceptance binary exceeds the production marker bound"
    );
    let sha256 = Sha256::digest(&body)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let platform = match std::env::consts::ARCH {
        "x86_64" => "linux-x64",
        "aarch64" => "linux-aarch64",
        arch => panic!("unsupported Linux setup acceptance architecture {arch}"),
    };
    let mut marker = binary.as_os_str().to_os_string();
    marker.push(".install.json");
    fs::write(
        std::path::PathBuf::from(marker),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "manager": "ctx-hosted-installer",
            "install_attempt_id": "ia_empty_catalog_native_supervisor",
            "install_path": binary,
            "platform": platform,
            "channel": "stable",
            "version": env!("CARGO_PKG_VERSION"),
            "sha256": sha256,
            "metadata_url": null,
            "artifact_url": null,
            "installed_at": "2026-08-10T00:00:00Z",
        }))
        .unwrap(),
    )
    .unwrap();
}

#[cfg(target_os = "linux")]
struct FakeSystemdDaemon {
    pid_file: std::path::PathBuf,
    data_root: std::path::PathBuf,
    executable: std::path::PathBuf,
}

#[cfg(target_os = "linux")]
impl Drop for FakeSystemdDaemon {
    fn drop(&mut self) {
        let Some(pid) = fs::read_to_string(&self.pid_file)
            .ok()
            .and_then(|pid| pid.trim().parse::<u32>().ok())
        else {
            return;
        };
        let Some(lock) = fs::read(self.data_root.join("daemon/daemon.lock"))
            .ok()
            .and_then(|body| serde_json::from_slice::<Value>(&body).ok())
        else {
            return;
        };
        let recorded_binary = lock.get("binary").and_then(Value::as_str).map(Path::new);
        let process_binary = fs::read_link(format!("/proc/{pid}/exe")).ok();
        if lock.get("pid").and_then(Value::as_u64) != Some(u64::from(pid))
            || lock.get("data_root").and_then(Value::as_str) != self.data_root.to_str()
            || recorded_binary.and_then(|path| fs::canonicalize(path).ok())
                != fs::canonicalize(&self.executable).ok()
            || process_binary.and_then(|path| fs::canonicalize(path).ok())
                != fs::canonicalize(&self.executable).ok()
        {
            return;
        }
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
    }
}

#[cfg(target_os = "linux")]
fn fake_operational_systemd_user_manager(
    temp: &TempDir,
    binary: &std::path::Path,
    managed_root: &std::path::Path,
    clean_exit_before_manager_restart: bool,
) -> (std::path::PathBuf, FakeSystemdDaemon) {
    use std::os::unix::fs::PermissionsExt as _;

    let manager_bin = temp.path().join("fake-systemd-bin");
    fs::create_dir(&manager_bin).unwrap();
    let systemctl = manager_bin.join("systemctl");
    let pid_file = temp.path().join("fake-systemd-main.pid");
    let enabled_file = temp.path().join("fake-systemd-enabled");
    let stdout_file = temp.path().join("fake-systemd-daemon.stdout");
    let stderr_file = temp.path().join("fake-systemd-daemon.stderr");
    let unit_file = temp.path().join(".config/systemd/user/ctx.service");
    fs::write(
        &systemctl,
        format!(
            r#"#!/bin/sh
pid_file='{pid_file}'
enabled_file='{enabled_file}'
unit_file='{unit_file}'
clean_exit_before_manager_restart='{clean_exit_before_manager_restart}'
case "$*" in
  "--user show --property=Version --value")
    printf '255\n'
    exit 0
    ;;
  "--user daemon-reload")
    exit 0
    ;;
  "--user enable ctx.service")
    : > "$enabled_file"
    exit 0
    ;;
  "--user start ctx.service")
    if [ -s "$pid_file" ] && kill -0 "$(sed -n '1p' "$pid_file")" 2>/dev/null; then
      exit 0
    fi
    if [ "$clean_exit_before_manager_restart" = 1 ]; then
      rm -f "$pid_file"
      if grep -Fxq 'Restart=always' "$unit_file"; then
        (
          sleep 0.1
          '{binary}' --data-root '{managed_root}' daemon run --format=json >'{stdout_file}' 2>'{stderr_file}' &
          printf '%s\n' "$!" > "$pid_file"
        ) &
      fi
      exit 0
    fi
    '{binary}' --data-root '{managed_root}' daemon run --format=json >'{stdout_file}' 2>'{stderr_file}' &
    printf '%s\n' "$!" > "$pid_file"
    exit 0
    ;;
  "--user is-enabled ctx.service")
    if [ -f "$enabled_file" ]; then printf 'enabled\n'; exit 0; fi
    exit 1
    ;;
  "--user is-active ctx.service")
    if [ -s "$pid_file" ] && kill -0 "$(sed -n '1p' "$pid_file")" 2>/dev/null; then
      printf 'active\n'
      exit 0
    fi
    exit 1
    ;;
  "--user show ctx.service --property=MainPID --value")
    sed -n '1p' "$pid_file"
    exit 0
    ;;
  "--user disable --now ctx.service")
    if [ -s "$pid_file" ]; then kill "$(sed -n '1p' "$pid_file")" 2>/dev/null || true; fi
    rm -f "$pid_file" "$enabled_file"
    exit 0
    ;;
esac
printf 'unexpected fake systemctl invocation: %s\n' "$*" >&2
exit 2
"#,
            pid_file = pid_file.display(),
            enabled_file = enabled_file.display(),
            unit_file = unit_file.display(),
            clean_exit_before_manager_restart =
                if clean_exit_before_manager_restart { 1 } else { 0 },
            binary = binary.display(),
            managed_root = managed_root.display(),
            stdout_file = stdout_file.display(),
            stderr_file = stderr_file.display(),
        ),
    )
    .unwrap();
    fs::set_permissions(&systemctl, fs::Permissions::from_mode(0o700)).unwrap();
    (
        manager_bin,
        FakeSystemdDaemon {
            pid_file: pid_file.clone(),
            data_root: managed_root.to_path_buf(),
            executable: binary.to_path_buf(),
        },
    )
}

#[test]
fn setup_does_not_migrate_legacy_shim_directory() {
    let temp = daemon_test_root();
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
    let temp = daemon_test_root();
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
fn setup_semantic_allows_manual_indexing_without_starting_a_daemon() {
    let temp = tempdir();
    let config_path = data_root(&temp).join("config.toml");
    fs::create_dir_all(data_root(&temp)).unwrap();
    let original = "[indexing]\nmode = \"manual\"\n";
    fs::write(&config_path, original).unwrap();

    let setup = json_output(ctx(&temp).args([
        "setup",
        "--semantic",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(setup["semantic"]["enabled"], true, "{setup:#}");
    assert_eq!(setup["daemon_autostart"]["requested"], false, "{setup:#}");
    assert_eq!(
        setup["daemon_autostart"]["reason"], "daemon_disabled",
        "{setup:#}"
    );
    let configured = fs::read_to_string(config_path).unwrap();
    assert!(configured.contains(original), "{configured}");
    assert!(
        configured.contains("[search]\nsemantic = true\n"),
        "{configured}"
    );
    assert!(!data_root(&temp).join("search").exists());
    assert!(!data_root(&temp).join("relational.sqlite").exists());
    assert!(!data_root(&temp).join("catalogs").exists());
    assert_no_daemon_autostart_mutation(&temp);

    let explicit_opt_out = tempdir();
    let setup = json_output(ctx(&explicit_opt_out).args([
        "setup",
        "--semantic",
        "--no-daemon",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(setup["semantic"]["enabled"], true, "{setup:#}");
    assert_eq!(setup["daemon_autostart"]["requested"], false, "{setup:#}");
    assert_eq!(
        setup["daemon_autostart"]["reason"], "explicit_opt_out",
        "{setup:#}"
    );
    let configured = fs::read_to_string(data_root(&explicit_opt_out).join("config.toml")).unwrap();
    assert!(
        configured.contains("[search]\nsemantic = true\n"),
        "{configured}"
    );
    let status = json_output(ctx(&explicit_opt_out).args(["status", "--format=json"]));
    assert_eq!(status["daemon"]["enabled"], true, "{status:#}");
    assert_eq!(status["semantic"]["enabled"], true, "{status:#}");
    assert!(!data_root(&explicit_opt_out).join("search").exists());
    assert!(!data_root(&explicit_opt_out)
        .join("relational.sqlite")
        .exists());
    assert!(!data_root(&explicit_opt_out).join("catalogs").exists());
    assert_no_daemon_autostart_mutation(&explicit_opt_out);
}

#[test]
fn setup_semantic_clean_cache_queues_daemon_without_foreground_download() {
    let temp = daemon_test_root();
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
fn semantic_namespace_is_explicit_readable_and_retains_downloaded_assets() {
    let temp = tempdir();

    let initial = json_output(ctx(&temp).args(["semantic", "status", "--format=json"]));
    assert_eq!(initial["operation"], "status", "{initial:#}");
    assert_eq!(initial["enabled"], false, "{initial:#}");
    assert_eq!(initial["status"], "disabled", "{initial:#}");
    assert_eq!(initial["read_only"], true, "{initial:#}");
    assert!(!data_root(&temp).exists());

    fs::create_dir_all(data_root(&temp).join("semantic-model-cache")).unwrap();
    fs::write(
        data_root(&temp).join("config.toml"),
        "# retained setting\n[indexing]\nmode = \"manual\"\n",
    )
    .unwrap();
    let retained_asset = data_root(&temp)
        .join("semantic-model-cache")
        .join("retained-model.bin");
    fs::write(&retained_asset, b"retained").unwrap();

    let enabled = json_output(ctx(&temp).args(["semantic", "enable", "--format=json"]));
    assert_eq!(enabled["operation"], "enable", "{enabled:#}");
    assert_eq!(enabled["enabled"], true, "{enabled:#}");
    assert_eq!(enabled["indexing"]["mode"], "manual", "{enabled:#}");
    assert_eq!(enabled["read_only"], false, "{enabled:#}");
    let configured = fs::read_to_string(data_root(&temp).join("config.toml")).unwrap();
    assert!(configured.contains("# retained setting"), "{configured}");
    assert!(
        configured.contains("[search]\nsemantic = true\n"),
        "{configured}"
    );

    let status = json_output(ctx(&temp).args(["semantic", "status", "--format=json"]));
    assert_eq!(status["enabled"], true, "{status:#}");
    assert_eq!(status["read_only"], true, "{status:#}");

    let disabled = json_output(ctx(&temp).args(["semantic", "disable", "--format=json"]));
    assert_eq!(disabled["operation"], "disable", "{disabled:#}");
    assert_eq!(disabled["enabled"], false, "{disabled:#}");
    assert_eq!(disabled["status"], "disabled", "{disabled:#}");
    assert_eq!(disabled["read_only"], false, "{disabled:#}");
    assert_eq!(fs::read(&retained_asset).unwrap(), b"retained");
}

#[test]
fn semantic_wait_rejects_manual_mode_before_persisting_opt_in() {
    let temp = tempdir();
    fs::create_dir_all(data_root(&temp)).unwrap();
    fs::write(
        data_root(&temp).join("config.toml"),
        "[indexing]\nmode = \"manual\"\n",
    )
    .unwrap();

    ctx(&temp)
        .args(["semantic", "enable", "--wait"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("ctx index mode auto"));

    let configured = fs::read_to_string(data_root(&temp).join("config.toml")).unwrap();
    assert!(!configured.contains("semantic"), "{configured}");
}

#[test]
fn semantic_disable_does_not_claim_success_under_an_enabling_process_override() {
    let temp = tempdir();
    fs::create_dir_all(data_root(&temp)).unwrap();
    fs::write(
        data_root(&temp).join("config.toml"),
        "[indexing]\nmode = \"manual\"\n\n[search]\nsemantic = true\n",
    )
    .unwrap();

    ctx(&temp)
        .args(["semantic", "disable"])
        .env("CTX_SEARCH_SEMANTIC", "true")
        .assert()
        .failure()
        .stderr(predicate::str::contains("active process override"));

    let configured = fs::read_to_string(data_root(&temp).join("config.toml")).unwrap();
    assert!(
        configured.contains("[search]\nsemantic = false\n"),
        "{configured}"
    );
}

#[test]
fn semantic_enable_persists_opt_in_but_reports_a_disabling_process_override() {
    let temp = tempdir();
    fs::create_dir_all(data_root(&temp)).unwrap();
    fs::write(
        data_root(&temp).join("config.toml"),
        "[indexing]\nmode = \"manual\"\n",
    )
    .unwrap();

    ctx(&temp)
        .args(["semantic", "enable"])
        .env("CTX_SEARCH_SEMANTIC", "false")
        .assert()
        .failure()
        .stderr(predicate::str::contains("active process override"));

    let configured = fs::read_to_string(data_root(&temp).join("config.toml")).unwrap();
    assert!(
        configured.contains("[search]\nsemantic = true\n"),
        "{configured}"
    );
    let status = json_output(
        ctx(&temp)
            .args(["semantic", "status", "--format=json"])
            .env("CTX_SEARCH_SEMANTIC", "false"),
    );
    assert_eq!(status["enabled"], false, "{status:#}");
    assert_eq!(status["config_source"], "environment", "{status:#}");
    assert_no_daemon_autostart_mutation(&temp);
}

#[test]
fn setup_semantic_alias_uses_the_same_process_override_validation() {
    let temp = tempdir();

    ctx(&temp)
        .args(["setup", "--semantic", "--progress", "none"])
        .env("CTX_SEARCH_SEMANTIC", "false")
        .assert()
        .failure()
        .stderr(predicate::str::contains("active process override"));

    let configured = fs::read_to_string(data_root(&temp).join("config.toml")).unwrap();
    assert!(
        configured.contains("[search]\nsemantic = true\n"),
        "{configured}"
    );
    assert!(!data_root(&temp).join("search").exists());
    assert!(!data_root(&temp).join("relational.sqlite").exists());
    assert!(!data_root(&temp).join("catalogs").exists());
    assert_no_daemon_autostart_mutation(&temp);
}

#[test]
fn semantic_enable_auto_starts_the_existing_daemon_acquisition_path() {
    let temp = daemon_test_root();

    let enabled = json_output(ctx(&temp).args(["semantic", "enable", "--format=json"]));
    assert_eq!(enabled["operation"], "enable", "{enabled:#}");
    assert_eq!(enabled["enabled"], true, "{enabled:#}");
    assert_eq!(enabled["indexing"]["mode"], "auto", "{enabled:#}");

    let status = wait_for_daemon_status(&temp, "running", true, "semantic");
    assert_eq!(
        status["daemon"]["jobs"]["semantic_index"]["semantic_enabled"], true,
        "{status:#}"
    );
    assert!(fs::read_to_string(data_root(&temp).join("config.toml"))
        .unwrap()
        .contains("[search]\nsemantic = true\n"));

    let waited = json_output(ctx(&temp).args(["semantic", "enable", "--wait", "--format=json"]));
    assert_eq!(waited["status"], "ready", "{waited:#}");
    assert_eq!(waited["selection"]["semantic"], true, "{waited:#}");
    assert_eq!(waited["read_only"], false, "{waited:#}");

    let disabled = json_output(ctx(&temp).args(["semantic", "disable", "--format=json"]));
    assert_eq!(disabled["enabled"], false, "{disabled:#}");
    assert_eq!(disabled["status"], "disabling", "{disabled:#}");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = json_output(ctx(&temp).args(["daemon", "status", "--format=json"]));
        let semantic = &status["daemon"]["jobs"]["semantic_index"];
        if semantic["semantic_enabled"] == false && semantic["runtime_active"] == false {
            assert_eq!(status["daemon"]["running"], true, "{status:#}");
            break;
        }
        assert!(Instant::now() < deadline, "{status:#}");
        std::thread::sleep(Duration::from_millis(20));
    }
    let disabled = json_output(ctx(&temp).args(["semantic", "status", "--format=json"]));
    assert_eq!(disabled["status"], "disabled", "{disabled:#}");
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

    let output = success_stdout(ctx(&temp).arg("status").env("CTX_DATA_ROOT", &data_root));
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
    let temp = daemon_test_root();
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
    let temp = daemon_test_root();
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
    let temp = daemon_test_root();
    ctx(&temp)
        .args(["--quiet", "setup", "--catalog-only"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());

    let temp = daemon_test_root();
    ctx(&temp)
        .args(["setup", "--quiet", "--catalog-only", "--progress", "none"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());

    let temp = daemon_test_root();
    ctx(&temp)
        .args(["setup", "--catalog-only", "--progress", "none"])
        .env("CTX_QUIET", "1")
        .assert()
        .success()
        .stdout(predicate::str::is_empty());

    let temp = daemon_test_root();
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
    let temp = daemon_test_root();
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

    let human_temp = daemon_test_root();
    write_codex_setup_session(&human_temp);
    let human_setup =
        success_stdout(ctx(&human_temp).args(["setup", "--wait", "--progress", "none"]));
    assert!(human_setup.contains("History is ready to search"));
    assert!(human_setup.contains("  ctx search \"test failure\""));
}

#[test]
fn setup_wait_imports_discovered_codex_prompt_history() {
    let temp = daemon_test_root();
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

    let human_setup =
        success_stdout(ctx(&temp).args(["setup", "--no-daemon", "--progress", "none"]));
    assert!(
        human_setup.contains("Background  skipped because --no-daemon was used"),
        "{human_setup}"
    );
}

#[test]
fn setup_import_isolates_empty_codex_session_file() {
    let temp = daemon_test_root();
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
    let temp = daemon_test_root();
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

fn validate_empty_catalog_refresh_request(refresh_request: &Value) -> Result<(), &'static str> {
    if refresh_request.get("source_count").and_then(Value::as_u64) != Some(0) {
        return Err("empty-catalog refresh request must have zero sources");
    }
    let Some(receipt) = refresh_request.get("receipt") else {
        return Err("empty-catalog refresh request must include receipt");
    };
    match refresh_request.get("status").and_then(Value::as_str) {
        Some("published")
            if receipt["successful_route_total"].as_u64().is_some()
                && receipt["successful_route_total"].as_u64()
                    == receipt["selected_route_total"].as_u64()
                && receipt["source_failure_total"].as_u64() == Some(0) =>
        {
            Ok(())
        }
        Some("published") => Err("published empty-catalog refresh must have terminal totals"),
        Some("admission_pending" | "queued" | "running") if receipt.is_null() => Ok(()),
        Some("admission_pending" | "queued" | "running") => {
            Err("active empty-catalog refresh must not have a receipt")
        }
        _ => Err("unknown empty-catalog refresh request status"),
    }
}

fn assert_empty_catalog_default_background_setup(setup: &Value) {
    assert_eq!(setup["mode"], "ready", "{setup:#}");
    assert_eq!(setup["lexical"]["status"], "ready", "{setup:#}");
    assert_eq!(setup["lexical"]["certified_sources"], 0, "{setup:#}");
    assert_eq!(setup["lexical"]["indexed_documents"], 0, "{setup:#}");
    if let Err(error) = validate_empty_catalog_refresh_request(&setup["refresh_request"]) {
        panic!("{error}: {setup:#}");
    }
}

#[test]
fn empty_catalog_default_background_oracle_is_status_sensitive() {
    let request = |status: &str, receipt: Value| json!({ "status": status, "source_count": 0, "receipt": receipt });
    let terminal_receipt = json!({
        "selected_route_total": 0,
        "successful_route_total": 0,
        "source_failure_total": 0,
    });

    assert_eq!(
        validate_empty_catalog_refresh_request(&request("published", terminal_receipt.clone())),
        Ok(())
    );
    assert!(validate_empty_catalog_refresh_request(&request("published", Value::Null)).is_err());
    for status in ["admission_pending", "queued", "running"] {
        assert_eq!(
            validate_empty_catalog_refresh_request(&request(status, Value::Null)),
            Ok(())
        );
        assert!(
            validate_empty_catalog_refresh_request(&request(status, terminal_receipt.clone()))
                .is_err()
        );
    }
    assert!(validate_empty_catalog_refresh_request(&request("unknown", Value::Null)).is_err());
    assert!(validate_empty_catalog_refresh_request(&json!({
        "status": "running",
        "source_count": 1,
        "receipt": null,
    }))
    .is_err());
}

#[path = "lifecycle/additional.rs"]
mod additional;
