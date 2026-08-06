#![cfg(unix)]

use std::{
    fs,
    io::{self, Read as _},
    os::{
        fd::{FromRawFd as _, OwnedFd},
        raw::{c_char, c_int, c_void},
        unix::fs::PermissionsExt,
    },
    path::Path,
    process::{Child, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use tempfile::tempdir;

mod support;
use support::{
    copied_ctx_binary, ctx, ctx_from_binary, daemon_test_root, data_root,
    initialize_current_query_store, initialize_empty_current_query_store, write_blame_helper,
    write_core_materialization_helper,
};

#[repr(C)]
struct TestWinsize {
    row: u16,
    column: u16,
    xpixel: u16,
    ypixel: u16,
}

#[link(name = "util")]
unsafe extern "C" {
    fn openpty(
        master: *mut c_int,
        slave: *mut c_int,
        name: *mut c_char,
        termios: *const c_void,
        winsize: *const TestWinsize,
    ) -> c_int;
}

fn index_dashboard_fixture_args(case: &str, columns: u16) -> Vec<String> {
    vec![
        "_index-dashboard-renderer-fixture".to_owned(),
        "--case".to_owned(),
        case.to_owned(),
        "--columns".to_owned(),
        columns.to_string(),
        "--rows".to_owned(),
        "24".to_owned(),
        "--clock".to_owned(),
        "2026-06-23T12:00:00Z".to_owned(),
        "--random-seed".to_owned(),
        "ctx-cli-ux-core-v1".to_owned(),
        "--color=always".to_owned(),
    ]
}

fn run_with_stdout_pty(args: &[String], columns: u16) -> (ExitStatus, Vec<u8>, Vec<u8>) {
    let mut master = -1;
    let mut slave = -1;
    let size = TestWinsize {
        row: 24,
        column: columns,
        xpixel: 0,
        ypixel: 0,
    };
    // SAFETY: openpty initializes both descriptors on success. Each descriptor
    // is immediately transferred to one owning File/OwnedFd.
    let result = unsafe {
        openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            &size,
        )
    };
    assert_eq!(result, 0, "openpty failed: {}", io::Error::last_os_error());

    // SAFETY: successful openpty returned distinct, live descriptors.
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };
    let prepared = Command::cargo_bin("ctx").unwrap();
    let mut command = std::process::Command::new(prepared.get_program());
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
    let mut child = command
        .args(args)
        .env("TERM", "xterm-256color")
        .stdin(Stdio::null())
        .stdout(Stdio::from(slave))
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(command);
    let mut stderr = child.stderr.take().unwrap();
    let status = child.wait().unwrap();
    let mut stderr_bytes = Vec::new();
    stderr.read_to_end(&mut stderr_bytes).unwrap();

    // SAFETY: successful openpty returned a live master descriptor not owned
    // by any other value.
    let mut master = unsafe { fs::File::from_raw_fd(master) };
    let mut stdout_bytes = Vec::new();
    match master.read_to_end(&mut stdout_bytes) {
        Ok(_) => {}
        Err(error) if error.raw_os_error() == Some(5) => {}
        Err(error) => panic!("read stdout PTY: {error}"),
    }
    (status, stdout_bytes, stderr_bytes)
}

struct LiveProDaemon {
    child: Child,
}

impl LiveProDaemon {
    fn is_running(&mut self) -> bool {
        self.child.try_wait().unwrap().is_none()
    }
}

impl Drop for LiveProDaemon {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn start_live_pro_daemon(temp: &tempfile::TempDir, helper: &Path) -> LiveProDaemon {
    let binary = copied_ctx_binary(temp);
    let prepared = ctx_from_binary(temp, &binary);
    let mut command = std::process::Command::new(prepared.get_program());
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
    let child = command
        .args([
            "daemon",
            "run",
            "--force",
            "--idle-exit-seconds",
            "600",
            "--loop-interval-seconds",
            "600",
        ])
        .env("CTX_PRO_HELPER", helper)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    LiveProDaemon { child }
}

fn daemon_pid(temp: &tempfile::TempDir) -> Option<u64> {
    let output = ctx(temp)
        .args(["daemon", "status", "--format=json"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value: Value = serde_json::from_slice(&output.stdout).ok()?;
    (value["daemon"]["running"] == true)
        .then(|| value["daemon"]["pid"].as_u64())
        .flatten()
}

fn wait_until(mut condition: impl FnMut() -> bool, detail: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {detail}");
        thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn live_daemon_rebuilds_replaced_helper_without_a_new_core_generation() {
    let temp = daemon_test_root();
    let data_root = data_root(&temp);
    fs::create_dir_all(&data_root).unwrap();
    fs::write(
        data_root.join("config.toml"),
        "[daemon]\nenabled = true\nmode = \"full\"\n\n[search]\nsemantic = false\n",
    )
    .unwrap();
    let generation = initialize_empty_current_query_store(&data_root);
    let helper = temp.path().join("ctx-pro-materializer");
    let helper_state = temp.path().join("materializer-state.json");
    let helper_log = temp.path().join("materializer-log.txt");
    write_core_materialization_helper(&helper, "revision-v1", &helper_state, &helper_log);

    let mut daemon = start_live_pro_daemon(&temp, &helper);
    wait_until(|| daemon_pid(&temp).is_some(), "live Pro daemon");
    let original_pid = daemon_pid(&temp).unwrap();
    wait_until(
        || fs::read_to_string(&helper_log).is_ok_and(|log| log.contains("finish:revision-v1")),
        "initial helper materialization",
    );
    assert!(daemon.is_running());

    let staged_helper = temp.path().join("ctx-pro-materializer.next");
    write_core_materialization_helper(&staged_helper, "revision-v2", &helper_state, &helper_log);
    let target_helper_sha256 = format!("{:x}", Sha256::digest(fs::read(&staged_helper).unwrap()));
    let output = ctx(&temp)
        .args([
            "pro",
            "_test-publish-helper-recheck",
            "--target-helper-sha256",
            &target_helper_sha256,
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}\nhelper log:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        fs::read_to_string(&helper_log).unwrap_or_default()
    );
    let recheck_path = data_root.join("daemon/jobs/pro-catch-up-recheck.json");
    let recheck: Value = serde_json::from_slice(&fs::read(&recheck_path).unwrap()).unwrap();
    assert_eq!(recheck["target_helper_sha256"], target_helper_sha256);

    // Mirror the production transaction: the intent is durable while the old
    // helper is still visible, then the exact target becomes visible, then the
    // daemon is woken. An early wake must not let the old helper clear it.
    ctx(&temp)
        .args(["pro", "_test-wake-helper-recheck"])
        .assert()
        .success();
    assert!(recheck_path.exists());
    assert!(!fs::read_to_string(&helper_log)
        .unwrap()
        .contains("finish:revision-v2"));
    fs::rename(&staged_helper, &helper).unwrap();
    ctx(&temp)
        .args(["pro", "_test-wake-helper-recheck"])
        .assert()
        .success();
    wait_until(
        || fs::read_to_string(&helper_log).is_ok_and(|log| log.contains("finish:revision-v2")),
        "replacement helper materialization",
    );
    let catch_up_path = data_root.join("daemon/jobs/pro-catch-up.json");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let catch_up_bytes = fs::read(&catch_up_path).unwrap_or_default();
        let catch_up = serde_json::from_slice::<Value>(&catch_up_bytes).ok();
        if catch_up.as_ref().is_some_and(|status| {
            status["status"] == "completed" && status["receipt_core_generation_id"] == generation
        }) && !recheck_path.exists()
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for replacement catch-up receipt; status={}; recheck={}; log={}",
            String::from_utf8_lossy(&catch_up_bytes),
            fs::read_to_string(&recheck_path).unwrap_or_default(),
            fs::read_to_string(&helper_log).unwrap_or_default(),
        );
        thread::sleep(Duration::from_millis(25));
    }

    assert_eq!(daemon_pid(&temp), Some(original_pid));
    assert!(daemon.is_running());
    let final_state: Value = serde_json::from_slice(&fs::read(&helper_state).unwrap()).unwrap();
    assert_eq!(
        final_state["core_generation_id"], generation,
        "unexpected helper state: {final_state}"
    );
    assert_eq!(final_state["materializer_revision"], "revision-v2");
    let catch_up: Value =
        serde_json::from_slice(&fs::read(data_root.join("daemon/jobs/pro-catch-up.json")).unwrap())
            .unwrap();
    assert_eq!(catch_up["core_generation_id"], generation);
    assert_eq!(catch_up["receipt_core_generation_id"], generation);
    assert_eq!(catch_up["status"], "completed");
    assert!(!data_root
        .join("daemon/jobs/pro-catch-up-recheck.json")
        .exists());
    assert_eq!(
        fs::read_to_string(&helper_log)
            .unwrap()
            .lines()
            .filter(|line| line.starts_with("finish:"))
            .collect::<Vec<_>>(),
        vec!["finish:revision-v1", "finish:revision-v2"]
    );
}

#[test]
fn stamped_test_host_runs_the_index_dashboard_fixture_through_a_real_pty() {
    let args = index_dashboard_fixture_args("semantic-failure", 32);
    let (status, stdout, stderr) = run_with_stdout_pty(&args, 32);
    assert_eq!(status.code(), Some(1));
    assert!(stderr.is_empty(), "{:?}", String::from_utf8_lossy(&stderr));

    let stdout = String::from_utf8(stdout).unwrap();
    assert!(stdout.contains("\u{1b}["), "{stdout:?}");
    assert!(stdout.contains("\r\u{1b}[2K"), "{stdout:?}");
    assert!(stdout.contains("Semantic search needs"), "{stdout:?}");
    assert!(stdout.contains("ctx doctor"), "{stdout:?}");
}

#[test]
fn stamped_test_host_fixture_is_unavailable_without_a_terminal() {
    Command::cargo_bin("ctx")
        .unwrap()
        .args(index_dashboard_fixture_args("ready", 80))
        .assert()
        .failure()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains(
            "index dashboard fixture requires stdout to be a terminal",
        ));
}

#[test]
fn ctx_status_reports_when_pro_helper_is_missing() {
    let root = tempdir().unwrap();
    Command::cargo_bin("ctx")
        .unwrap()
        .env("CTX_PRO_CHANNEL", "staging")
        .args([
            "--data-root",
            root.path().to_str().unwrap(),
            "status",
            "--format=json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"installed\": false"))
        .stdout(predicate::str::contains("pro_not_installed"));
}

#[test]
fn blame_commit_negotiates_the_exact_protocol_and_returns_typed_json() {
    let root = tempdir().unwrap();
    initialize_current_query_store(root.path());
    let helper = root.path().join("ctx-pro-blame");
    write_blame_helper(&helper);

    let output = Command::cargo_bin("ctx")
        .unwrap()
        .env("CTX_PRO_HELPER", &helper)
        .args([
            "--data-root",
            root.path().to_str().unwrap(),
            "blame",
            "commit",
            "0123456789abcdef",
            "--format=json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["snapshot"]["kind"], "core");
    assert_eq!(
        value["snapshot"]["receipt"]["materializer_revision"],
        "pro-query-fixture-v1"
    );
    assert_eq!(value["target"]["kind"], "commit");
    assert_eq!(
        value["outcome"],
        serde_json::json!({
            "attribution": "proven",
            "coverage": {
                "unit": "commit_fact",
                "evaluated": 1,
                "proven": 1,
                "possible": 0,
                "conflicting": 0,
                "none": 0,
            },
        })
    );
    assert_eq!(value["freshness"]["state"], "current");
    assert_eq!(value["matches"][0]["kind"], "commit");
    assert_eq!(value["matches"][0]["value"]["predicate"], "produced_by");
    assert_eq!(value["evidence"].as_array().map(Vec::len), Some(1));
    assert!(value.get("payload_type").is_none());
    assert!(value.get("summary").is_none());
    assert!(value.get("suggested_next_commands").is_none());
}

fn missing_resource_command() -> (tempfile::TempDir, Command) {
    let root = tempdir().unwrap();
    initialize_current_query_store(root.path());
    let helper = root.path().join("ctx-pro-blame-missing-resource");
    support::write_blame_error_helper(&helper, "resource_not_found");
    let mut command = Command::cargo_bin("ctx").unwrap();
    command.env("CTX_PRO_HELPER", &helper).args([
        "--data-root",
        root.path().to_str().unwrap(),
        "blame",
        "commit",
        "0123456789abcdef",
    ]);
    (root, command)
}

#[test]
fn missing_blame_resource_has_trusted_human_diagnostic() {
    let (_root, mut command) = missing_resource_command();
    command
        .assert()
        .failure()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::eq(
            "✗ No indexed Pro resource matches the requested blame target.\n\nHint: Try:\n\nNext\n  ctx search 0123456789abcdef --refresh off\n",
        ));
}

#[test]
fn missing_blame_resource_keeps_stable_json_mode_code() {
    let (_root, mut command) = missing_resource_command();
    let output = command.arg("--format=json").output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let diagnostic: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(diagnostic["error"], "resource_not_found");
    assert_eq!(diagnostic["error_code"], "resource_not_found");
    assert_eq!(diagnostic["reason"], "target_not_indexed");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("Error:"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("/secret/graph/path"));
}

#[test]
fn commit_blame_human_output_preserves_production_grouping() {
    let root = tempdir().unwrap();
    initialize_current_query_store(root.path());
    let helper = root.path().join("ctx-pro-blame");
    write_blame_helper(&helper);

    Command::cargo_bin("ctx")
        .unwrap()
        .env("CTX_PRO_HELPER", &helper)
        .args([
            "--data-root",
            root.path().to_str().unwrap(),
            "blame",
            "commit",
            "0123456789abcdef",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Produced by\n  session session-producer\n    id",
        ))
        .stdout(predicate::str::contains("state         asserted"))
        .stdout(predicate::str::contains(
            "Evidence\n  [1]  ctx show event d863cb84-6bd3-8071-abdb-5326c44c896a",
        ))
        .stdout(predicate::str::contains("\u{1b}[").not())
        .stdout(predicate::str::contains("Also recorded").not());
}

#[test]
fn shorthand_blame_preserves_explicit_human_and_json_output() {
    let root = tempdir().unwrap();
    initialize_current_query_store(root.path());
    let helper = root.path().join("ctx-pro-blame");
    write_blame_helper(&helper);

    for (explicit, shorthand) in [
        (
            vec!["blame", "commit", "0123456789abcdef"],
            vec!["blame", "0123456789abcdef"],
        ),
        (
            vec!["blame", "pr", "https://github.com/ctxrs/ctx/pull/42"],
            vec!["blame", "https://github.com/ctxrs/ctx/pull/42"],
        ),
    ] {
        for format in [None, Some("--format=json")] {
            let run = |args: &[&str]| {
                let mut command = Command::cargo_bin("ctx").unwrap();
                command
                    .env("CTX_PRO_HELPER", &helper)
                    .arg("--data-root")
                    .arg(root.path())
                    .args(args);
                if let Some(format) = format {
                    command.arg(format);
                }
                command.output().unwrap()
            };
            let explicit_output = run(&explicit);
            let shorthand_output = run(&shorthand);
            assert!(
                explicit_output.status.success(),
                "{}",
                String::from_utf8_lossy(&explicit_output.stderr)
            );
            assert!(
                shorthand_output.status.success(),
                "{}",
                String::from_utf8_lossy(&shorthand_output.stderr)
            );
            assert_eq!(shorthand_output.stdout, explicit_output.stdout);
            assert_eq!(shorthand_output.stderr, explicit_output.stderr);
            if format.is_some() {
                let value: serde_json::Value =
                    serde_json::from_slice(&shorthand_output.stdout).unwrap();
                assert!(matches!(
                    value["target"]["kind"].as_str(),
                    Some("commit" | "pull_request")
                ));
            } else {
                assert!(!shorthand_output.stdout.is_empty());
            }
        }
    }
}

#[test]
fn commit_and_pr_blame_do_not_require_git_but_file_blame_does() {
    let root = tempdir().unwrap();
    initialize_current_query_store(root.path());
    let helper = root.path().join("ctx-pro-blame");
    write_blame_helper(&helper);
    let empty_path = root.path().join("empty-path");
    fs::create_dir(&empty_path).unwrap();

    for args in [
        vec!["blame", "commit", "0123456789abcdef", "--format=json"],
        vec![
            "blame",
            "pr",
            "https://github.com/ctxrs/ctx/pull/42",
            "--format=json",
        ],
    ] {
        Command::cargo_bin("ctx")
            .unwrap()
            .env("CTX_PRO_HELPER", &helper)
            .env("PATH", &empty_path)
            .arg("--data-root")
            .arg(root.path())
            .args(args)
            .assert()
            .success();
    }

    let marker = root.path().join("file-helper-started");
    let must_not_start = root.path().join("ctx-pro-must-not-start");
    fs::write(
        &must_not_start,
        format!(
            "#!/bin/sh\nprintf invoked > '{}'\nexit 91\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&must_not_start, fs::Permissions::from_mode(0o700)).unwrap();
    Command::cargo_bin("ctx")
        .unwrap()
        .env("CTX_PRO_HELPER", &must_not_start)
        .env("PATH", &empty_path)
        .arg("--data-root")
        .arg(root.path())
        .args(["blame", "file", "src/lib.rs"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "The repository required for this blame request is unavailable",
        ))
        .stderr(predicate::str::contains("helper_crashed").not());
    assert!(!marker.exists());
}

#[test]
fn numeric_pr_selector_requires_repository_before_helper_access() {
    let root = tempdir().unwrap();
    let marker = root.path().join("helper-started");
    let helper = root.path().join("ctx-pro-must-not-start");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nprintf invoked > '{}'\nexit 91\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();

    Command::cargo_bin("ctx")
        .unwrap()
        .env("CTX_PRO_HELPER", &helper)
        .arg("--data-root")
        .arg(root.path())
        .args(["blame", "pr", "42"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "pull request number requires a repository selector",
        ));
    assert!(!marker.exists());
}

#[test]
fn obsolete_public_pro_query_commands_have_no_compatibility_aliases() {
    for args in [
        &["facts", "commit", "abcd"][..],
        &["timeline", "commit", "abcd"][..],
        &["related", "commit", "abcd"][..],
        &["show", "commit", "abcd"][..],
        &["locate", "file", "src/lib.rs"][..],
    ] {
        Command::cargo_bin("ctx")
            .unwrap()
            .args(args)
            .assert()
            .failure()
            .stderr(predicate::str::contains("unrecognized subcommand"));
    }
}

#[test]
fn blame_without_an_installation_identity_fails_before_starting_the_helper() {
    let root = tempdir().unwrap();
    let marker = root.path().join("helper-started");
    let helper = root.path().join("ctx-pro-must-not-start");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nprintf invoked > '{}'\nexit 91\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();

    Command::cargo_bin("ctx")
        .unwrap()
        .env("CTX_PRO_HELPER", &helper)
        .arg("--data-root")
        .arg(root.path())
        .args(["blame", "commit", "abcd"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "The source data required for this blame request is unavailable",
        ))
        .stderr(predicate::str::contains("helper_crashed").not());
    assert!(!marker.exists());
}
