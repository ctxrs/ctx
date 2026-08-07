#![cfg(unix)]

mod support;

use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    process::{Child, Command},
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
        "--idle-exit-seconds",
        "60",
        "--loop-interval-seconds",
        "60",
        "--format=json",
    ]);
    let registration_root = binary.with_file_name(format!(
        ".{}.daemon-quiescence-acks",
        binary.file_name().unwrap().to_string_lossy()
    ));
    let mut fresh = fresh.spawn().expect("attempt fresh custom-root daemon");
    wait_for_fenced_exit(&mut fresh, &registration_root, Duration::from_secs(15));
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

#[test]
fn fresh_custom_root_daemon_cannot_enter_after_all_root_proof_before_helper_commit() {
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
    teardown.arg("--data-root").arg(&requested_root).args([
        "daemon",
        "disable",
        "--prepare-uninstall",
        "--format=json",
    ]);
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
