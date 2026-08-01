#![cfg(unix)]

use std::{
    fs,
    io::{self, Read as _},
    os::{
        fd::{FromRawFd as _, OwnedFd},
        raw::{c_char, c_int, c_void},
        unix::fs::PermissionsExt,
    },
    process::{ExitStatus, Stdio},
};

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

mod support;
use support::{initialize_current_query_store, write_blame_helper};

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
    assert_eq!(value["target"]["kind"], "commit");
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
            "✗ No indexed Pro resource matches the requested blame target.\nThe target is valid but is not present in the materialized Pro graph.\n",
        ));
}

#[test]
fn missing_blame_resource_keeps_stable_json_mode_code() {
    let (_root, mut command) = missing_resource_command();
    command
        .arg("--format=json")
        .assert()
        .failure()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::eq("Error: resource_not_found\n"));
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
        .stderr(predicate::str::contains("repository_unavailable"))
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
        &["facts", "commit", "abc"][..],
        &["timeline", "commit", "abc"][..],
        &["related", "commit", "abc"][..],
        &["show", "commit", "abc"][..],
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
        .args(["blame", "commit", "abc"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("key store is unavailable"))
        .stderr(predicate::str::contains("helper_crashed").not());
    assert!(!marker.exists());
}
