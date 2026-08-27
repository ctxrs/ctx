#![cfg(unix)]

mod support;

use fs2::FileExt as _;
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use std::{
    fs::{self, OpenOptions},
    os::unix::{ffi::OsStrExt as _, fs::PermissionsExt as _},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use support::{copied_ctx_binary, tempdir};

fn platform_key() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "linux-x64",
        ("linux", "aarch64") => "linux-aarch64",
        ("macos", "aarch64") => "macos-arm64",
        ("macos", "x86_64") => "macos-x64",
        ("freebsd", "x86_64") => "freebsd-x64",
        pair => panic!("unsupported hosted uninstall test platform: {pair:?}"),
    }
}

fn sha256(path: &Path) -> String {
    format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
}

fn installation_registration_root(home: &Path, binary: &Path) -> PathBuf {
    let canonical = fs::canonicalize(binary).unwrap();
    let namespace = format!("{:x}", Sha256::digest(canonical.as_os_str().as_bytes()));
    home.join(".ctx")
        .join("daemon-installations")
        .join(namespace)
        .join("daemon-quiescence-acks")
}

fn isolated_command(binary: &Path, root: &Path) -> Command {
    let mut command = Command::new(binary);
    for directory in [
        root.join("config"),
        root.join("data"),
        root.join("state"),
        root.join("runtime"),
        root.join("tmp"),
    ] {
        fs::create_dir_all(directory).unwrap();
    }
    command
        .env("HOME", root)
        .env("CTX_DATA_ROOT", root.join("canonical-root"))
        .env("CTX_ANALYTICS_ENABLED", "false")
        .env("CTX_LOCAL_USAGE_ENABLED", "false")
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_RUNTIME_DIR", root.join("runtime"))
        .env("TMPDIR", root.join("tmp"))
        .env_remove("CI")
        .env_remove("CTX_DAEMON_AUTOSTART_OFF")
        .env_remove("CTX_DAEMON_BACKGROUND_CHILD");
    command
}

fn successful_json(mut command: Command) -> Value {
    let output = command.output().expect("run isolated ctx command");
    assert!(
        output.status.success(),
        "ctx command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("parse isolated ctx JSON output")
}

fn registration_count(root: &Path) -> usize {
    fs::read_dir(root)
        .map(|entries| entries.filter_map(Result::ok).count())
        .unwrap_or(0)
}

fn wait_for_fenced_exit(child: &mut Child, registration_root: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if child
            .try_wait()
            .expect("observe fresh daemon attempt")
            .is_some()
        {
            return;
        }
        if registration_count(registration_root) != 0 {
            let _ = child.kill();
            let _ = child.wait();
            panic!("fresh custom-root daemon published an installation lease");
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("fresh custom-root daemon did not resolve its admission check");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn assert_fresh_daemon_is_fenced(binary: &Path, environment_root: &Path, data_root: &Path) {
    let mut fresh = isolated_command(binary, environment_root);
    fresh.arg("--data-root").arg(data_root).args([
        "daemon",
        "run",
        "--force",
        "--loop-interval-seconds",
        "60",
        "--format=json",
    ]);
    let registration_root = installation_registration_root(environment_root, binary);
    let mut fresh = fresh.spawn().expect("attempt fresh custom-root daemon");
    wait_for_fenced_exit(&mut fresh, &registration_root, Duration::from_secs(30));
    assert_eq!(
        registration_count(&registration_root),
        0,
        "fresh daemon published an installation lease"
    );
}

fn install_supervisor_command_probe(root: &Path) -> (PathBuf, PathBuf) {
    let probe_bin = root.join("supervisor-probe-bin");
    fs::create_dir_all(&probe_bin).unwrap();
    let body = b"#!/bin/sh\nprobe_dir=${0%/*}\nprintf '%s\\n' \"$*\" >> \"$probe_dir/invocations\"\nexit 97\n";
    for command in ["systemctl", "launchctl"] {
        let path = probe_bin.join(command);
        fs::write(&path, body).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let log = probe_bin.join("invocations");
    (probe_bin, log)
}

fn install_supervisor_command_stub(root: &Path) -> PathBuf {
    let stub_bin = root.join("supervisor-stub-bin");
    fs::create_dir_all(&stub_bin).unwrap();
    let body = b"#!/bin/sh\ncase \"$*\" in\n  *is-enabled*|*is-active*|*print*) exit 1 ;;\n  *) exit 0 ;;\nesac\n";
    for command in ["systemctl", "launchctl"] {
        let path = stub_bin.join(command);
        fs::write(&path, body).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    stub_bin
}

fn assert_autostart_supervisor_is_fenced(
    binary: &Path,
    environment_root: &Path,
    data_root: &Path,
    probe_bin: &Path,
    probe_log: &Path,
    expected_receipt: Option<&[u8]>,
) {
    let mut attempt = isolated_command(binary, environment_root);
    attempt
        .env("PATH", probe_bin)
        .arg("--data-root")
        .arg(data_root)
        .args(["daemon", "enable", "--format=json"]);
    let output = attempt.output().expect("attempt daemon autostart");
    assert!(
        !output.status.success(),
        "autostart entered uninstall window"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("hosted_uninstall_active"),
        "unexpected autostart failure: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let receipt = data_root.join("daemon").join("supervisor.json");
    assert_eq!(
        fs::read(receipt).ok().as_deref(),
        expected_receipt,
        "autostart mutated the supervisor receipt"
    );
    assert_supervisor_artifacts_unchanged(environment_root, probe_log);
}

fn assert_supervisor_artifacts_unchanged(environment_root: &Path, probe_log: &Path) {
    assert!(
        !probe_log.exists(),
        "autostart invoked the native supervisor"
    );
    for artifact in [
        environment_root
            .join("config")
            .join("systemd")
            .join("user")
            .join("ctx.service"),
        environment_root
            .join("Library")
            .join("LaunchAgents")
            .join("rs.ctx.daemon.plist"),
    ] {
        assert!(
            !artifact.exists(),
            "autostart recreated native supervisor artifact {}",
            artifact.display()
        );
    }
}

fn wait_for_marker(child: &mut Child, marker: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if marker.exists() {
            return;
        }
        if let Some(status) = child.try_wait().expect("observe supervisor lock waiter") {
            panic!("supervisor lock waiter exited before blocking: {status}");
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("supervisor admission did not reach the installation lock");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn supervisor_waiter_rechecks_uninstall_fence_after_installation_lock() {
    let temp = tempdir();
    let (supervisor_probe_bin, supervisor_probe_log) =
        install_supervisor_command_probe(temp.path());
    let install = copied_ctx_binary(&temp);
    let marker = install.with_file_name(format!(
        "{}.install.json",
        install.file_name().unwrap().to_string_lossy()
    ));
    fs::write(
        &marker,
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "manager": "ctx-hosted-installer",
            "install_attempt_id": "ia_supervisor_lock_recheck",
            "install_path": install,
            "platform": platform_key(),
            "channel": "stable",
            "version": "1.0.0",
            "sha256": sha256(&install),
        }))
        .unwrap(),
    )
    .unwrap();
    fs::set_permissions(&marker, fs::Permissions::from_mode(0o600)).unwrap();

    let canonical_root = temp.path().join("canonical-root");
    let daemon_root = canonical_root.join("daemon");
    fs::create_dir_all(&daemon_root).unwrap();
    let installation_lock_path = daemon_root.join("supervisor-installation.lock");
    let installation_lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&installation_lock_path)
        .unwrap();
    installation_lock.lock_exclusive().unwrap();

    let waiting_marker = temp.path().join("supervisor-lock-waiting");
    let mut attempt = isolated_command(&install, temp.path());
    attempt
        .env("PATH", &supervisor_probe_bin)
        .env("CTX_SUPERVISOR_LOCK_WAITING_FOR_TESTS", &waiting_marker)
        .args(["daemon", "enable", "--format=json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut attempt = attempt.spawn().expect("start supervisor lock waiter");
    wait_for_marker(&mut attempt, &waiting_marker, Duration::from_secs(15));

    let mut prepare = isolated_command(&install, temp.path());
    prepare.args([
        "upgrade",
        "--hosted-transaction",
        "uninstall-prepare",
        "--install-path",
        install.to_str().unwrap(),
        "--attempt-id",
        "ia_supervisor_lock_recheck",
    ]);
    assert_eq!(successful_json(prepare)["daemon_admission_fenced"], true);

    installation_lock.unlock().unwrap();
    drop(installation_lock);
    let output = attempt
        .wait_with_output()
        .expect("finish supervisor waiter");
    assert!(
        !output.status.success(),
        "supervisor waiter crossed uninstall fence"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("fenced by hosted uninstall"),
        "unexpected supervisor waiter failure: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !canonical_root.join("daemon/supervisor.json").exists(),
        "supervisor waiter wrote a receipt after uninstall fencing"
    );
    assert_supervisor_artifacts_unchanged(temp.path(), &supervisor_probe_log);
}

#[test]
fn prepare_uninstall_waits_for_indexing_control_before_disabling_and_cleanup() {
    let temp = tempdir();
    let supervisor_stub_bin = install_supervisor_command_stub(temp.path());
    let install = copied_ctx_binary(&temp);
    let marker = install.with_file_name(format!(
        "{}.install.json",
        install.file_name().unwrap().to_string_lossy()
    ));
    fs::write(
        &marker,
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "manager": "ctx-hosted-installer",
            "install_attempt_id": "ia_indexing_control_uninstall_race",
            "install_path": install,
            "platform": platform_key(),
            "channel": "stable",
            "version": "1.0.0",
            "sha256": sha256(&install),
        }))
        .unwrap(),
    )
    .unwrap();
    fs::set_permissions(&marker, fs::Permissions::from_mode(0o600)).unwrap();

    let requested_root = temp.path().join("requested-indexing-race-root");
    let mut initial = isolated_command(&install, temp.path());
    initial
        .env("PATH", &supervisor_stub_bin)
        .arg("--data-root")
        .arg(&requested_root)
        .args(["daemon", "disable", "--format=json"]);
    assert_eq!(successful_json(initial)["daemon_enabled"], false);

    let gate = requested_root.join(".block-daemon-automatic-indexing-after-config-for-test");
    let blocked = requested_root.join(".daemon-automatic-indexing-blocked-after-config-for-test");
    fs::write(&gate, b"block\n").unwrap();
    let mut enable = isolated_command(&install, temp.path());
    enable
        .env("PATH", &supervisor_stub_bin)
        .arg("--data-root")
        .arg(&requested_root)
        .args(["daemon", "enable", "--format=json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut enable = enable.spawn().expect("start blocked indexing command");
    wait_for_marker(&mut enable, &blocked, Duration::from_secs(15));

    let mut fence = isolated_command(&install, temp.path());
    fence.args([
        "upgrade",
        "--hosted-transaction",
        "uninstall-prepare",
        "--install-path",
        install.to_str().unwrap(),
        "--attempt-id",
        "ia_indexing_control_uninstall_race",
    ]);
    assert_eq!(successful_json(fence)["daemon_admission_fenced"], true);

    let mut teardown = isolated_command(&install, temp.path());
    teardown
        .env("PATH", &supervisor_stub_bin)
        .arg("--data-root")
        .arg(&requested_root)
        .args(["daemon", "disable", "--prepare-uninstall", "--format=json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut teardown = teardown.spawn().expect("start prepare-uninstall waiter");
    thread::sleep(Duration::from_millis(250));
    assert!(
        teardown.try_wait().unwrap().is_none(),
        "prepare-uninstall bypassed indexing-control ownership"
    );
    let in_flight_config = fs::read_to_string(requested_root.join("config.toml")).unwrap();
    assert!(
        in_flight_config.contains("mode = \"auto\""),
        "prepare-uninstall changed indexing before acquiring control: {in_flight_config}"
    );

    fs::remove_file(&gate).unwrap();
    let enable_output = enable
        .wait_with_output()
        .expect("finish fenced indexing command");
    assert!(!enable_output.status.success());
    assert!(
        String::from_utf8_lossy(&enable_output.stderr).contains("hosted_uninstall_active"),
        "unexpected indexing failure: {}",
        String::from_utf8_lossy(&enable_output.stderr)
    );

    let teardown_output = teardown
        .wait_with_output()
        .expect("finish serialized prepare-uninstall");
    assert!(
        teardown_output.status.success(),
        "prepare-uninstall failed: {}",
        String::from_utf8_lossy(&teardown_output.stderr)
    );
    let proof: Value = serde_json::from_slice(&teardown_output.stdout).unwrap();
    assert_eq!(proof["coordination_state_removed"], true, "{proof:#}");
    for root in [&requested_root, &temp.path().join("canonical-root")] {
        assert!(
            !root.join("daemon/lifecycle-control.lock").exists(),
            "successful uninstall retained control lock for {}",
            root.display()
        );
        assert!(
            !root.join("daemon/lifecycle-transition.lock").exists(),
            "successful uninstall retained transition lock for {}",
            root.display()
        );
    }
}

#[test]
fn prepare_uninstall_discovers_and_quiesces_a_finite_custom_root_worker() {
    let temp = tempdir();
    let supervisor_stub_bin = install_supervisor_command_stub(temp.path());
    let install = copied_ctx_binary(&temp);
    let marker = install.with_file_name(format!(
        "{}.install.json",
        install.file_name().unwrap().to_string_lossy()
    ));
    fs::write(
        &marker,
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "manager": "ctx-hosted-installer",
            "install_attempt_id": "ia_finite_worker_uninstall",
            "install_path": install,
            "platform": platform_key(),
            "channel": "stable",
            "version": "1.0.0",
            "sha256": sha256(&install),
        }))
        .unwrap(),
    )
    .unwrap();
    fs::set_permissions(&marker, fs::Permissions::from_mode(0o600)).unwrap();

    let custom_root = temp.path().join("finite-custom-root");
    let mut finite = isolated_command(&install, temp.path());
    finite
        .env("CTX_DAEMON_BACKGROUND_CHILD", "1")
        .arg("--data-root")
        .arg(&custom_root)
        .args([
            "daemon",
            "run",
            "--finite-core-worker",
            "--force",
            "--format=json",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut finite = finite.spawn().expect("start finite custom-root worker");
    let registration_root = installation_registration_root(temp.path(), &install);
    let registration_deadline = Instant::now() + Duration::from_secs(15);
    let (registration_path, registration) = loop {
        if let Ok(entries) = fs::read_dir(&registration_root) {
            if let Some(path) = entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .find(|path| {
                    path.extension().and_then(|extension| extension.to_str()) == Some("json")
                })
            {
                let value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                break (path, value);
            }
        }
        if let Some(status) = finite.try_wait().unwrap() {
            panic!("finite worker exited before registration: {status}");
        }
        if Instant::now() >= registration_deadline {
            let _ = finite.kill();
            let _ = finite.wait();
            panic!("finite worker did not publish installation registration");
        }
        thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(registration["status"], "live", "{registration:#}");
    assert_eq!(registration["persistent"], false, "{registration:#}");
    assert_eq!(
        registration["data_root"],
        custom_root.to_string_lossy().as_ref(),
        "{registration:#}"
    );

    let requested_root = temp.path().join("requested-finite-uninstall-root");
    let mut teardown = isolated_command(&install, temp.path());
    teardown
        .env("PATH", &supervisor_stub_bin)
        .arg("--data-root")
        .arg(&requested_root)
        .args(["daemon", "disable", "--prepare-uninstall", "--format=json"]);
    let proof = successful_json(teardown);
    assert_eq!(proof["installation_quiescent"], true, "{proof:#}");
    assert_eq!(proof["coordination_state_removed"], true, "{proof:#}");
    assert!(
        proof["quiesced_roots"]
            .as_array()
            .unwrap()
            .iter()
            .any(|root| root.as_str() == Some(custom_root.to_string_lossy().as_ref())),
        "finite worker root was not discovered: {proof:#}"
    );

    let output = finite
        .wait_with_output()
        .expect("finish finite custom-root worker");
    assert!(
        output.status.success(),
        "finite worker failed during uninstall: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!registration_path.exists());
    assert_eq!(registration_count(&registration_root), 0);
    let config = fs::read_to_string(custom_root.join("config.toml")).unwrap();
    assert!(config.contains("mode = \"manual\""), "{config}");
}

#[test]
fn fresh_custom_root_daemon_cannot_enter_after_all_root_proof_before_helper_commit() {
    let temp = tempdir();
    let (supervisor_probe_bin, supervisor_probe_log) =
        install_supervisor_command_probe(temp.path());
    let supervisor_stub_bin = install_supervisor_command_stub(temp.path());
    let install = copied_ctx_binary(&temp);
    let marker = install.with_file_name(format!(
        "{}.install.json",
        install.file_name().unwrap().to_string_lossy()
    ));
    fs::write(
        &marker,
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "manager": "ctx-hosted-installer",
            "install_attempt_id": "ia_hosted_uninstall_fence",
            "install_path": install,
            "platform": platform_key(),
            "channel": "stable",
            "version": "1.0.0",
            "sha256": sha256(&install),
        }))
        .unwrap(),
    )
    .unwrap();
    fs::set_permissions(&marker, fs::Permissions::from_mode(0o600)).unwrap();

    let mut prepare = isolated_command(&install, temp.path());
    prepare.args([
        "upgrade",
        "--hosted-transaction",
        "uninstall-prepare",
        "--install-path",
        install.to_str().unwrap(),
        "--attempt-id",
        "ia_hosted_uninstall_fence",
    ]);
    let prepared = successful_json(prepare);
    assert_eq!(prepared["schema_version"], 2);
    assert_eq!(prepared["daemon_admission_fenced"], true);
    let helper = PathBuf::from(prepared["helper_path"].as_str().unwrap());

    let requested_root = temp.path().join("requested-custom-root");
    let mut teardown = isolated_command(&install, temp.path());
    teardown
        .env("PATH", &supervisor_stub_bin)
        .arg("--data-root")
        .arg(&requested_root)
        .args(["daemon", "disable", "--prepare-uninstall", "--format=json"]);
    let proof = successful_json(teardown);
    assert_eq!(proof["installation_quiescent"], true);
    assert_eq!(proof["coordination_state_removed"], true);
    assert_eq!(proof["binary_retained"], true);
    let journal = install.with_file_name(format!(
        ".{}.hosted-install-transaction.json",
        install.file_name().unwrap().to_string_lossy()
    ));
    assert!(
        journal.is_file(),
        "all-root proof removed the uninstall fence"
    );
    let canonical_root = temp.path().join("canonical-root");
    let supervisor_receipt = fs::read(canonical_root.join("daemon/supervisor.json")).ok();

    assert_fresh_daemon_is_fenced(
        &install,
        temp.path(),
        &temp.path().join("fresh-custom-root"),
    );
    assert_fresh_daemon_is_fenced(
        &helper,
        temp.path(),
        &temp.path().join("fresh-helper-custom-root"),
    );
    assert_autostart_supervisor_is_fenced(
        &install,
        temp.path(),
        &canonical_root,
        &supervisor_probe_bin,
        &supervisor_probe_log,
        supervisor_receipt.as_deref(),
    );
    assert_autostart_supervisor_is_fenced(
        &helper,
        temp.path(),
        &canonical_root,
        &supervisor_probe_bin,
        &supervisor_probe_log,
        supervisor_receipt.as_deref(),
    );

    let mut arm = isolated_command(&helper, temp.path());
    arm.args([
        "upgrade",
        "--hosted-transaction",
        "uninstall-arm",
        "--install-path",
        install.to_str().unwrap(),
    ]);
    assert_eq!(successful_json(arm)["status"], "armed");

    let mut commit = isolated_command(&helper, temp.path());
    commit.args([
        "upgrade",
        "--hosted-transaction",
        "uninstall-commit",
        "--install-path",
        install.to_str().unwrap(),
    ]);
    let committed = successful_json(commit);
    assert_eq!(committed["status"], "committed");
    assert!(!install.exists());
    assert!(!marker.exists());
    assert!(!journal.exists());
    assert!(helper.exists());
    assert_fresh_daemon_is_fenced(
        &helper,
        temp.path(),
        &temp.path().join("post-commit-helper-custom-root"),
    );
    assert_autostart_supervisor_is_fenced(
        &helper,
        temp.path(),
        &canonical_root,
        &supervisor_probe_bin,
        &supervisor_probe_log,
        supervisor_receipt.as_deref(),
    );
}
