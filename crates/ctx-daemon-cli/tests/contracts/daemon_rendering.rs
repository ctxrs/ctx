mod support;

use std::{fs, fs::OpenOptions, process};

use fs2::FileExt as _;
use serde_json::json;

use support::*;

#[test]
fn all_daemon_status_is_read_only_for_an_uninitialized_installation() {
    let temp = tempdir();
    let root = data_root(&temp);
    let listed = json_output(&mut ctx(&temp).args(["daemon", "status", "--all", "--format=json"]));
    assert_eq!(listed["schema_version"], 1);
    assert_eq!(listed["daemons"], json!([]));
    assert!(!root.exists(), "status created an uninitialized data root");
    ctx(&temp)
        .args(["daemon", "status", "--all"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No registered daemons"));
    assert!(!root.exists());
    ctx(&temp)
        .args(["daemon", "run", "--all"])
        .assert()
        .failure();
}

#[test]
fn human_daemon_failure_is_rendered_once_by_final_dispatch() {
    let temp = tempdir();
    let root = data_root(&temp);
    let daemon_root = root.join("daemon");
    fs::create_dir_all(&daemon_root).unwrap();
    fs::write(
        root.join("config.toml"),
        "[analytics]\nenabled = false\n\n[upgrade]\nauto = \"off\"\n\n[daemon]\nenabled = true\nmode = \"source-refresh-only\"\n\n[search]\nsemantic = false\n",
    )
    .unwrap();
    let failure = "final dispatch rendered failure oracle";
    fs::write(
        daemon_root.join("status.json"),
        serde_json::to_vec(&json!({
            "schema_version": 1,
            "status": "failed",
            "pid": process::id(),
            "start_mode": "manual",
            "last_error": failure,
            "semantic_runtime_active": false,
        }))
        .unwrap(),
    )
    .unwrap();

    // Holding the runtime's advisory guard makes `daemon run` retain the
    // already-failed lifecycle report without linking this final-binary
    // contract back into daemon implementation crates.
    let guard = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(daemon_root.join("daemon.guard"))
        .unwrap();
    guard.lock_exclusive().unwrap();

    let output = ctx(&temp).args(["daemon", "run"]).output().unwrap();
    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let rendered = format!("{stdout}{stderr}");
    assert_eq!(rendered.matches(failure).count(), 1, "{rendered}");
    assert_eq!(rendered.matches("Daemon failed").count(), 1, "{rendered}");
    assert!(
        !rendered.contains("CLI error was already rendered"),
        "final dispatch rendered a second generic failure:\n{rendered}"
    );
}
