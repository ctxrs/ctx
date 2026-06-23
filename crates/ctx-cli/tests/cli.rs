use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::{json, Value};
use std::fs;
#[cfg(unix)]
use std::time::Duration;
use tempfile::{Builder, TempDir};
use uuid::Uuid;

fn tempdir() -> TempDir {
    let root = std::env::current_dir().unwrap().join("target/test-data");
    fs::create_dir_all(&root).unwrap();
    Builder::new().prefix("ctx-test-").tempdir_in(root).unwrap()
}

fn ctx(temp: &TempDir) -> Command {
    let mut command = Command::cargo_bin("ctx").unwrap();
    command.env("CTX_DATA_ROOT", temp.path());
    command
}

fn record(temp: &TempDir, title: &str, body: &str, tags: &[&str]) -> Value {
    let mut command = ctx(temp);
    command.args(["record", "--title", title, "--body", body, "--json"]);
    for tag in tags {
        command.args(["--tag", tag]);
    }
    let output = command.assert().success().get_output().stdout.clone();
    serde_json::from_slice::<Value>(&output).unwrap()["record"].clone()
}

fn write_json(temp: &TempDir, name: &str, value: &Value) -> String {
    let path = temp.path().join(name);
    fs::write(&path, serde_json::to_string_pretty(value).unwrap()).unwrap();
    path.to_str().unwrap().to_string()
}

#[test]
fn root_setup_status_schema_and_validate_work() {
    let temp = tempdir();
    ctx(&temp)
        .args(["setup"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Work Recorder workspace ready"));

    ctx(&temp)
        .args(["status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("initialized: true"))
        .stdout(predicate::str::contains("blob_dir:"))
        .stdout(predicate::str::contains("inbox_dir:"))
        .stdout(predicate::str::contains("device_path:"))
        .stdout(predicate::str::contains("spool_pending: 0"))
        .stdout(predicate::str::contains("spool_processing: 0"))
        .stdout(predicate::str::contains("spool_failed: 0"));

    ctx(&temp)
        .args(["schema"])
        .assert()
        .success()
        .stdout(predicate::str::contains("work_records"));

    ctx(&temp)
        .args(["validate"])
        .assert()
        .success()
        .stdout(predicate::str::contains("valid"));
}

#[test]
fn capture_write_fixture_is_quiet_and_imports_from_spool() {
    let temp = tempdir();
    ctx(&temp)
        .args([
            "capture",
            "write-fixture",
            "--title",
            "Spooled fixture",
            "--body",
            "captured from spool",
            "--dedupe-key",
            "quiet-fixture",
        ])
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::is_empty());

    let inbox = temp.path().join("work-record").join("inbox");
    let pending = fs::read_dir(&inbox)
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".jsonl")
        })
        .count();
    assert_eq!(pending, 1);

    ctx(&temp)
        .args(["status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("spool_pending: 1"));

    ctx(&temp)
        .args(["capture", "import", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"schema_version\": 1"))
        .stdout(predicate::str::contains("\"import\""))
        .stdout(predicate::str::contains("\"imported_records\": 1"));

    ctx(&temp)
        .args(["list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Spooled fixture"));
}

#[test]
fn validate_reports_failed_and_processing_capture_spool_files() {
    let temp = tempdir();
    let inbox = temp.path().join("work-record").join("inbox");
    fs::create_dir_all(&inbox).unwrap();
    fs::write(inbox.join("capture-one.jsonl.failed"), "{}\n").unwrap();
    fs::write(inbox.join("capture-two.jsonl.processing"), "{}\n").unwrap();

    ctx(&temp)
        .args(["validate"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "1 failed capture spool file(s) need retry or inspection",
        ))
        .stdout(predicate::str::contains(
            "1 capture spool file(s) are still marked processing",
        ));
}

#[test]
fn root_record_show_search_context_report_and_link_pr_work() {
    let temp = tempdir();
    let first = record(
        &temp,
        "Implement search",
        "full text enough for local notes",
        &["search"],
    );
    record(&temp, "Write report", "summarize work records", &["report"]);
    let id = first["id"].as_str().unwrap();

    ctx(&temp)
        .args(["search", "local", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"schema_version\": 1"))
        .stdout(predicate::str::contains("\"records\""))
        .stdout(predicate::str::contains("Implement search"));

    ctx(&temp)
        .args(["show", id, "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"schema_version\": 1"))
        .stdout(predicate::str::contains("\"record\""))
        .stdout(predicate::str::contains("Implement search"));

    ctx(&temp)
        .args([
            "link-pr",
            id,
            "https://github.com/ctxrs/ctx/pull/42",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"schema_version\": 1"))
        .stdout(predicate::str::contains("pull/42"));

    ctx(&temp)
        .args(["context", "report"])
        .assert()
        .success()
        .stdout(predicate::str::contains("# Work Context"))
        .stdout(predicate::str::contains("Write report"));

    ctx(&temp)
        .args(["report", "--format", "json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"schema_version\": 1"))
        .stdout(predicate::str::contains("\"summary\""))
        .stdout(predicate::str::contains("\"record_count\": 2"));
}

#[test]
fn evidence_run_is_recorded() {
    let temp = tempdir();
    let item = record(&temp, "Run tests", "capture command output", &["evidence"]);
    let id = item["id"].as_str().unwrap();

    ctx(&temp)
        .args(["evidence", "run", "--record", id, "rustc", "--version"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"schema_version\": 1"))
        .stdout(predicate::str::contains("\"evidence\""))
        .stdout(predicate::str::contains("\"exit_code\": 0"));

    ctx(&temp)
        .args(["context", "Run", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"schema_version\": 1"))
        .stdout(predicate::str::contains("\"results\""))
        .stdout(predicate::str::contains("Run tests"));
}

#[test]
fn evidence_run_truncates_stdout_to_output_cap() {
    let temp = tempdir();

    let output = ctx(&temp)
        .args([
            "evidence",
            "run",
            "--max-output-bytes",
            "4",
            "rustc",
            "--version",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let evidence: Value = serde_json::from_slice(&output).unwrap();

    assert_eq!(evidence["schema_version"], 1);
    assert_eq!(evidence["evidence"]["exit_code"], 0);
    assert_eq!(evidence["evidence"]["stdout"].as_str().unwrap(), "rust");
}

#[cfg(unix)]
#[test]
fn evidence_timeout_kills_descendant_process_group() {
    let temp = tempdir();

    let output = ctx(&temp)
        .timeout(Duration::from_secs(5))
        .args([
            "evidence",
            "run",
            "--timeout-seconds",
            "1",
            "sh",
            "-c",
            "sleep 10 & wait",
        ])
        .assert()
        .failure()
        .stdout(predicate::str::contains("\"exit_code\": 124"))
        .stdout(predicate::str::contains(
            "command timed out after 1 seconds",
        ))
        .stderr(predicate::str::contains("evidence command exited with 124"))
        .get_output()
        .stdout
        .clone();

    let evidence: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(evidence["schema_version"], 1);
    assert_eq!(evidence["evidence"]["exit_code"], 124);
}

#[test]
fn export_and_import_round_trip() {
    let source = tempdir();
    record(&source, "Portable record", "export me", &["archive"]);

    let archive = source.path().join("archive.json");
    ctx(&source)
        .args(["export", "--output", archive.to_str().unwrap()])
        .assert()
        .success();

    let dest = tempdir();
    ctx(&dest)
        .args(["import", "--input", archive.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("imported 1 records"));

    ctx(&dest)
        .args(["list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Portable record"));
}

#[test]
fn import_rejects_unsupported_archive_version() {
    let temp = tempdir();
    let archive = write_json(
        &temp,
        "unsupported.json",
        &json!({
            "version": 2,
            "records": [],
            "evidence": []
        }),
    );

    ctx(&temp)
        .args(["import", "--input", &archive])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "unsupported work record archive version: 2",
        ));

    ctx(&temp)
        .args(["validate"])
        .assert()
        .success()
        .stdout(predicate::str::contains("valid"));
}

#[test]
fn failed_import_is_atomic() {
    let temp = tempdir();
    let record = record(&temp, "Exported", "body", &["atomic"]);
    let record_id = record["id"].as_str().unwrap();
    let archive = write_json(
        &temp,
        "bad-fk.json",
        &json!({
            "version": 1,
            "records": [record],
            "evidence": [{
                "id": Uuid::new_v4(),
                "record_id": Uuid::new_v4(),
                "command": "cargo test",
                "exit_code": 0,
                "stdout": "",
                "stderr": "",
                "started_at": "2026-01-01T00:00:00Z",
                "duration_ms": 1
            }]
        }),
    );

    let dest = tempdir();
    ctx(&dest)
        .args(["import", "--input", &archive])
        .assert()
        .failure()
        .stderr(predicate::str::contains("FOREIGN KEY constraint failed"));

    ctx(&dest)
        .args(["show", record_id])
        .assert()
        .failure()
        .stderr(predicate::str::contains("record not found"));
}

#[test]
fn import_rejects_conflicts_and_overwrites_when_explicit() {
    let temp = tempdir();
    let mut record = record(&temp, "Original", "body", &["conflict"]);
    let archive = write_json(
        &temp,
        "original.json",
        &json!({
            "version": 1,
            "records": [record.clone()],
            "evidence": []
        }),
    );

    let dest = tempdir();
    ctx(&dest)
        .args(["import", "--input", &archive])
        .assert()
        .success();

    record["title"] = json!("Replacement");
    let replacement = write_json(
        &temp,
        "replacement.json",
        &json!({
            "version": 1,
            "records": [record.clone()],
            "evidence": []
        }),
    );

    ctx(&dest)
        .args(["import", "--input", &replacement])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "archive conflicts with existing record",
        ));
    ctx(&dest)
        .args(["list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Original"))
        .stdout(predicate::str::contains("Replacement").not());

    ctx(&dest)
        .args(["import", "--input", &replacement, "--overwrite"])
        .assert()
        .success();
    ctx(&dest)
        .args(["list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Replacement"));
}

#[test]
fn root_uninstall_removes_product_data() {
    let temp = tempdir();
    record(&temp, "Delete me", "body", &[]);
    ctx(&temp)
        .args(["uninstall", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("removed"));
    ctx(&temp)
        .args(["status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("initialized: false"));
}

#[test]
fn nested_workspace_and_work_commands_remain_compatibility_aliases() {
    let temp = tempdir();
    ctx(&temp)
        .args(["workspace", "setup"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Work Recorder workspace ready"));
    ctx(&temp)
        .args(["workspace", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("initialized: true"));

    let output = ctx(&temp)
        .args([
            "work",
            "record",
            "--title",
            "Nested alias",
            "--body",
            "compatibility",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let record: Value = serde_json::from_slice(&output).unwrap();
    let id = record["record"]["id"].as_str().unwrap();

    ctx(&temp)
        .args(["work", "show", id])
        .assert()
        .success()
        .stdout(predicate::str::contains("Nested alias"));
    ctx(&temp)
        .args(["work", "validate"])
        .assert()
        .success()
        .stdout(predicate::str::contains("valid"));

    ctx(&temp)
        .args(["workspace", "uninstall", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("removed"));
}
