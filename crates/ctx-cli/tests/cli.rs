use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::{json, Value};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::time::Duration;
use std::{
    fs,
    path::{Path, PathBuf},
};
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

fn git(cwd: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git failed: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
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

fn json_output(command: &mut Command) -> Value {
    let output = command.assert().success().get_output().stdout.clone();
    serde_json::from_slice(&output).unwrap()
}

fn write_json(temp: &TempDir, name: &str, value: &Value) -> String {
    let path = temp.path().join(name);
    fs::write(&path, serde_json::to_string_pretty(value).unwrap()).unwrap();
    path.to_str().unwrap().to_string()
}

fn provider_fixture(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/provider")
        .join(name)
        .to_str()
        .unwrap()
        .to_owned()
}

fn inbox_file_with_suffix(temp: &TempDir, suffix: &str) -> PathBuf {
    let inbox = temp.path().join("work-record").join("inbox");
    fs::read_dir(&inbox)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().ends_with(suffix))
                .unwrap_or(false)
        })
        .unwrap_or_else(|| panic!("missing inbox file ending with {suffix}"))
}

#[cfg(unix)]
fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

#[test]
fn vcs_inspect_json_reports_git_workspace_and_redacts_remote_tokens() {
    let temp = tempdir();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init"]);
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "https://x-access-token:ghp_secret@github.com/ctxrs/ctx.git",
        ],
    );

    let output = ctx(&temp)
        .current_dir(&repo)
        .args(["vcs", "inspect", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("ghp_secret").not())
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    let inspection = &payload["inspection"];

    assert_eq!(payload["schema_version"], 1);
    assert_eq!(inspection["git"]["available"], true);
    assert_eq!(
        inspection["git"]["workspace"]["primary_remote"]["normalized_url"],
        "https://github.com/ctxrs/ctx"
    );
    assert!(matches!(
        inspection["git"]["workspace"]["repo_fingerprint"]["source"].as_str(),
        Some("remote") | Some("remote_and_path")
    ));
    assert!(inspection["jj"]["available"].is_boolean());
}

#[test]
fn pr_parse_json_reports_confidence_labeled_link() {
    let temp = tempdir();

    let output = ctx(&temp)
        .args([
            "pr",
            "parse",
            "https://gitlab.example.com/platform/team/ctx/-/merge_requests/7/diffs",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    let parsed = &payload["pull_request"];

    assert_eq!(payload["schema_version"], 1);
    assert_eq!(parsed["provider"], "gitlab");
    assert_eq!(parsed["owner"], "platform/team");
    assert_eq!(parsed["repo"], "ctx");
    assert_eq!(parsed["number"], 7);
    assert_eq!(parsed["confidence"], "explicit");
    assert_eq!(parsed["link"]["target_type"], "pull_request");
    assert_eq!(parsed["link"]["confidence"], "explicit");
}

#[cfg(unix)]
#[test]
fn shim_install_env_uninstall_are_local_and_reversible() {
    let temp = tempdir();
    let shim_dir = temp.path().join("shims");

    ctx(&temp)
        .args(["shim", "install", "--dir", shim_dir.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("git"))
        .stdout(predicate::str::contains("jj"))
        .stdout(predicate::str::contains("gh"));
    assert!(shim_dir.join("git").exists());
    assert!(shim_dir.join("jj").exists());
    assert!(shim_dir.join("gh").exists());

    ctx(&temp)
        .args(["shim", "env", "--dir", shim_dir.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("export PATH="))
        .stdout(predicate::str::contains(shim_dir.to_str().unwrap()));

    ctx(&temp)
        .args(["shim", "uninstall", "--dir", shim_dir.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("removed"));
    assert!(!shim_dir.join("git").exists());
    assert!(!shim_dir.join("jj").exists());
    assert!(!shim_dir.join("gh").exists());
}

#[cfg(unix)]
#[test]
fn shim_install_refuses_to_overwrite_unrecognized_files() {
    let temp = tempdir();
    let shim_dir = temp.path().join("shims");
    fs::create_dir_all(&shim_dir).unwrap();
    fs::write(shim_dir.join("git"), "#!/bin/sh\n").unwrap();

    ctx(&temp)
        .args(["shim", "install", "--dir", shim_dir.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "refusing to overwrite unrecognized file",
        ));
}

#[cfg(unix)]
#[test]
fn installed_git_shim_runs_real_command_and_spools_capture() {
    let temp = tempdir();
    let shim_dir = temp.path().join("shims");
    let real_dir = temp.path().join("real");
    fs::create_dir_all(&real_dir).unwrap();
    write_executable(
        &real_dir.join("git"),
        r#"#!/bin/sh
echo "fake git stdout $*"
echo "fake git stderr" >&2
exit 7
"#,
    );

    ctx(&temp)
        .args(["shim", "install", "--dir", shim_dir.to_str().unwrap()])
        .assert()
        .success();

    let path = format!(
        "{}:{}:{}",
        shim_dir.display(),
        real_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = std::process::Command::new(shim_dir.join("git"))
        .arg("status")
        .arg("--short")
        .env("PATH", path)
        .env("CTX_DATA_ROOT", temp.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "fake git stdout status --short\n"
    );
    assert_eq!(String::from_utf8_lossy(&output.stderr), "fake git stderr\n");

    ctx(&temp)
        .args(["status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("spool_pending: 1"));

    ctx(&temp)
        .args(["capture", "import", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"imported_records\": 1"))
        .stdout(predicate::str::contains("\"imported_evidence\": 1"));

    ctx(&temp)
        .args(["export"])
        .assert()
        .success()
        .stdout(predicate::str::contains("fake git stdout"))
        .stdout(predicate::str::contains("fake git stderr"));

    ctx(&temp)
        .args(["context", "git status", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("git command: git status --short"))
        .stdout(predicate::str::contains("\"status\": \"failed\""));
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
fn normal_work_commands_auto_import_pending_capture_spool() {
    let temp = tempdir();
    ctx(&temp)
        .args([
            "capture",
            "write-fixture",
            "--title",
            "Auto imported fixture",
            "--body",
            "normal work commands should import pending captures",
            "--dedupe-key",
            "auto-import-fixture",
        ])
        .assert()
        .success();

    ctx(&temp)
        .args(["list"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .stdout(predicate::str::contains("Auto imported fixture"));

    ctx(&temp)
        .args(["status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("spool_pending: 0"))
        .stdout(predicate::str::contains("spool_done: 1"));
}

#[test]
fn provider_fixture_import_json_reports_counts_and_summary_record() {
    let temp = tempdir();
    let fixture = provider_fixture("codex.jsonl");

    let mut command = ctx(&temp);
    command.args([
        "capture",
        "import-provider",
        "--provider",
        "codex",
        "--input",
        &fixture,
        "--json",
    ]);
    let payload = json_output(&mut command);

    assert_eq!(payload["schema_version"], 1);
    assert_eq!(payload["provider"], "codex");
    assert_eq!(payload["import"]["imported_sessions"], 2);
    assert_eq!(payload["import"]["imported_events"], 3);
    assert_eq!(payload["import"]["imported_edges"], 1);
    assert_eq!(payload["import"]["failed"], 0);
    assert_eq!(payload["import"]["redacted"], 0);
    assert_eq!(
        payload["record"]["title"],
        "Imported codex provider fixture"
    );

    let mut second = ctx(&temp);
    second.args([
        "capture",
        "import-provider",
        "--provider",
        "codex",
        "--input",
        &fixture,
        "--json",
    ]);
    let second_payload = json_output(&mut second);
    assert_eq!(second_payload["import"]["imported_sessions"], 0);
    assert_eq!(second_payload["import"]["imported_events"], 0);
    assert_eq!(second_payload["record"], Value::Null);
}

#[test]
fn dashboard_export_uses_rich_summary_sections_after_provider_import() {
    let temp = tempdir();
    let fixture = provider_fixture("codex.jsonl");
    ctx(&temp)
        .args([
            "capture",
            "import-provider",
            "--provider",
            "codex",
            "--input",
            &fixture,
        ])
        .assert()
        .success();

    let output_dir = temp.path().join("provider-dashboard");
    ctx(&temp)
        .args([
            "dashboard",
            "export",
            "--output",
            output_dir.to_str().unwrap(),
        ])
        .assert()
        .success();

    let html = fs::read_to_string(output_dir.join("index.html")).unwrap();
    assert!(html.contains("Summaries"));
    assert!(html.contains("imported_provider_summary"));
    assert!(html.contains("Provider fixture import for codex"));
    assert!(html.contains("Implement provider import foundations."));
}

#[test]
fn search_and_context_find_provider_import_events() {
    let temp = tempdir();
    let fixture = provider_fixture("codex.jsonl");
    ctx(&temp)
        .args([
            "capture",
            "import-provider",
            "--provider",
            "codex",
            "--input",
            &fixture,
        ])
        .assert()
        .success();

    let mut search = ctx(&temp);
    search.args(["search", "Subagent reported", "--json"]);
    let packet = json_output(&mut search);
    assert_eq!(packet["results"].as_array().unwrap().len(), 1);
    assert_eq!(
        packet["results"][0]["title"],
        "Imported codex provider fixture"
    );
    assert!(packet["results"][0]["snippet"]
        .as_str()
        .unwrap()
        .contains("Subagent reported changed files."));

    let mut context = ctx(&temp);
    context.args(["context", "exec_command", "--json"]);
    let packet = json_output(&mut context);
    assert_eq!(packet["results"].as_array().unwrap().len(), 1);
    assert!(packet["results"][0]["summary"]
        .as_str()
        .unwrap()
        .contains("exec_command"));
}

#[test]
fn doctor_and_repair_retry_failed_capture_spool_files() {
    let temp = tempdir();
    ctx(&temp)
        .args([
            "capture",
            "write-fixture",
            "--title",
            "Repairable fixture",
            "--body",
            "failed capture can be retried",
            "--dedupe-key",
            "repairable-fixture",
        ])
        .assert()
        .success();

    let pending = inbox_file_with_suffix(&temp, ".jsonl");
    let failed = pending.with_file_name(format!(
        "{}.failed",
        pending.file_name().unwrap().to_string_lossy()
    ));
    fs::rename(&pending, &failed).unwrap();
    fs::write(
        failed.with_file_name(format!(
            "{}.error.json",
            failed.file_name().unwrap().to_string_lossy()
        )),
        "{}\n",
    )
    .unwrap();

    ctx(&temp)
        .args(["doctor"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "1 failed capture spool file(s) need retry or inspection",
        ));

    ctx(&temp)
        .args(["repair", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"retried_files\": 1"))
        .stdout(predicate::str::contains("\"imported_records\": 1"));

    ctx(&temp)
        .args(["list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Repairable fixture"));
}

#[test]
fn doctor_privacy_reports_local_storage_spool_and_permissions() {
    let temp = tempdir();
    record(&temp, "Privacy doctor", "local storage check", &["privacy"]);

    ctx(&temp)
        .args(["doctor", "--privacy"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Privacy health"))
        .stdout(predicate::str::contains("storage: local_only"))
        .stdout(predicate::str::contains("hosted_sync: disabled"))
        .stdout(predicate::str::contains("validation: valid"))
        .stdout(predicate::str::contains("spool_pending: 0"))
        .stdout(predicate::str::contains("permissions_work_record_dir:"))
        .stdout(predicate::str::contains("permissions_database:"))
        .stdout(predicate::str::contains("permissions_inbox:"));
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
        .stdout(predicate::str::contains("\"results\""))
        .stdout(predicate::str::contains("\"snippet\""))
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
fn publish_pr_comment_dry_run_renders_marker_bounded_redacted_markdown() {
    let temp = tempdir();
    let item = record(
        &temp,
        "Publish token=ghp_1234567890abcdef",
        "finished product password=hunter2 under /home/daddy/code/private",
        &["publish", "secret=shhh"],
    );
    let id = item["id"].as_str().unwrap();

    ctx(&temp)
        .args(["link-pr", id, "https://github.com/ctxrs/ctx/pull/42/files"])
        .assert()
        .success();

    ctx(&temp)
        .args(["evidence", "run", "--record", id, "rustc", "--version"])
        .assert()
        .success();

    let output = ctx(&temp)
        .args(["publish", "pr-comment", id, "--dry-run"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let markdown = String::from_utf8(output).unwrap();

    assert!(markdown.starts_with("<!-- ctx-work-record:finished-product:start -->"));
    assert!(markdown
        .trim_end()
        .ends_with("<!-- ctx-work-record:finished-product:end -->"));
    assert!(markdown.contains("## Work Recorder Finished Product"));
    assert!(markdown.contains("https://github.com/ctxrs/ctx/pull/42"));
    assert!(markdown.contains("token=[REDACTED_SECRET]"));
    assert!(markdown.contains("password=[REDACTED_SECRET]"));
    assert!(markdown.contains("[REDACTED_PATH]"));
    assert!(markdown.contains("Transcript redacted by default"));
    assert!(!markdown.contains("ghp_123456"));
    assert!(!markdown.contains("hunter2"));
    assert!(!markdown.contains("/home/daddy/code/private"));
    assert!(!markdown.contains("secret=shhh"));
}

#[test]
fn publish_pr_comment_raw_transcript_requires_explicit_flag() {
    let temp = tempdir();
    let item = record(
        &temp,
        "Publish raw transcript",
        "raw note password=hunter2",
        &["publish"],
    );
    let id = item["id"].as_str().unwrap();

    ctx(&temp)
        .args(["link-pr", id, "https://github.com/ctxrs/ctx/pull/43"])
        .assert()
        .success();

    ctx(&temp)
        .args(["evidence", "run", "--record", id, "rustc", "--version"])
        .assert()
        .success();

    ctx(&temp)
        .args([
            "publish",
            "pr-comment",
            id,
            "--dry-run",
            "--include-raw-transcript",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Transcript mode: raw opt-in"))
        .stdout(predicate::str::contains(
            "raw note password=[REDACTED_SECRET]",
        ))
        .stdout(predicate::str::contains("stdout:"))
        .stdout(predicate::str::contains("rustc"));
}

#[test]
fn publish_pr_comment_live_publish_reports_missing_token_or_client() {
    let temp = tempdir();
    let item = record(&temp, "Publish live", "body", &["publish"]);
    let id = item["id"].as_str().unwrap();

    ctx(&temp)
        .args(["link-pr", id, "https://github.com/ctxrs/ctx/pull/44"])
        .assert()
        .success();

    ctx(&temp)
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .args(["publish", "pr-comment", id])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "requires GITHUB_TOKEN or GH_TOKEN",
        ));

    ctx(&temp)
        .env("GITHUB_TOKEN", "ghp_testtoken")
        .args(["publish", "pr-comment", id])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "requires an HTTP client integration that is not available yet",
        ));
}

#[test]
fn dashboard_export_writes_static_local_html_report() {
    let temp = tempdir();
    let first = record(
        &temp,
        "Render dashboard token=ghp_1234567890abcdef",
        "include recent records and search context cues password=hunter2 cwd=/tmp/work",
        &["dashboard", "secret=shhh"],
    );
    let id = first["id"].as_str().unwrap();

    ctx(&temp)
        .args(["link-pr", id, "https://github.com/ctxrs/ctx/pull/77"])
        .assert()
        .success();

    ctx(&temp)
        .args(["evidence", "run", "--record", id, "rustc", "--version"])
        .assert()
        .success();

    let output_dir = temp.path().join("dashboard");
    ctx(&temp)
        .args([
            "dashboard",
            "export",
            "--output",
            output_dir.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("index.html"));

    let html = fs::read_to_string(output_dir.join("index.html")).unwrap();
    assert!(html.contains("Work Records"));
    assert!(html.contains("Static local export"));
    assert!(html.contains("Render dashboard token=[REDACTED_SECRET]"));
    assert!(html.contains("https://github.com/ctxrs/ctx/pull/77"));
    assert!(html.contains("Evidence Previews"));
    assert!(html.contains("ctx search &lt;query&gt; --json"));
    assert!(html.contains("password=[REDACTED_SECRET]"));
    assert!(html.contains("[REDACTED_PATH]"));
    assert!(!html.contains("ghp_123456"));
    assert!(!html.contains("hunter2"));
    assert!(!html.contains("/tmp/work"));
    assert!(!html.contains("secret=shhh"));
    assert!(!html.contains("<script"));
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
fn context_json_returns_agent_packet_shape() {
    let temp = tempdir();
    let first = record(
        &temp,
        "Fix checkout retry",
        "checkout retry failed after a transient package index error",
        &["checkout"],
    );
    let id = first["id"].as_str().unwrap();

    ctx(&temp)
        .args([
            "link-pr",
            id,
            "https://github.com/ctxrs/ctx/pull/123",
            "--json",
        ])
        .assert()
        .success();

    let mut command = ctx(&temp);
    command
        .env("CTX_DASHBOARD_URL", "http://127.0.0.1:3000")
        .args(["context", "checkout retry", "--json"]);
    let packet = json_output(&mut command);

    assert_eq!(packet["schema_version"], 1);
    assert_eq!(packet["query"], "checkout retry");
    assert_eq!(packet["budget"]["max_tokens"], 12_000);
    assert!(packet["budget"]["estimated_tokens"].as_u64().unwrap() > 0);
    assert_eq!(packet["results"][0]["record_id"], id);
    assert_eq!(packet["results"][0]["title"], "Fix checkout retry");
    assert_eq!(packet["results"][0]["visibility"], "local_only");
    assert_eq!(
        packet["results"][0]["links"]["dashboard"],
        format!("http://127.0.0.1:3000/records/{id}")
    );
    assert_eq!(
        packet["results"][0]["links"]["pr"],
        "https://github.com/ctxrs/ctx/pull/123"
    );
    assert_eq!(packet["pagination"]["has_more"], false);
    assert_eq!(packet["truncation"]["truncated"], false);
}

#[test]
fn context_json_omits_dashboard_url_when_not_share_safe() {
    let temp = tempdir();
    record(&temp, "Deploy dashboard", "deploy context", &["deploy"]);

    let mut command = ctx(&temp);
    command
        .env("CTX_DASHBOARD_URL", "https://secret@example.test")
        .args(["context", "deploy", "--json"]);
    let packet = json_output(&mut command);

    assert!(packet["results"][0]["links"].get("dashboard").is_none());
}

#[test]
fn context_json_includes_why_matched_citations_and_evidence() {
    let temp = tempdir();
    let record = record(
        &temp,
        "Debug rustc failure",
        "rustc failed while checking the work recorder CLI",
        &["compiler"],
    );
    let id = record["id"].as_str().unwrap();

    ctx(&temp)
        .args([
            "evidence",
            "run",
            "--record",
            id,
            "rustc",
            "--definitely-not-a-real-rustc-flag",
        ])
        .assert()
        .failure();

    let mut command = ctx(&temp);
    command.args(["context", "rustc", "--json"]);
    let packet = json_output(&mut command);
    let result = &packet["results"][0];
    let why = result["why_matched"].as_array().unwrap();
    assert!(why.iter().any(|value| value == "title"));
    assert!(why.iter().any(|value| value == "failed_command"));
    assert!(result["citations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|citation| citation["type"] == "evidence"));
    assert_eq!(result["evidence"][0]["kind"], "manual");
    assert_eq!(result["evidence"][0]["status"], "failed");
    assert_eq!(result["evidence"][0]["freshness"], "unbound");
    assert!(packet.get("stdout").is_none());
    assert!(packet.get("stderr").is_none());
}

#[test]
fn context_json_reports_token_budget_truncation() {
    let temp = tempdir();
    for index in 0..6 {
        record(
            &temp,
            &format!("Budget context record {index}"),
            &format!(
                "budget context body {index} {}",
                "long searchable detail ".repeat(30)
            ),
            &["budget"],
        );
    }

    let mut command = ctx(&temp);
    command.args([
        "context",
        "budget",
        "--json",
        "--limit",
        "6",
        "--max-tokens",
        "90",
    ]);
    let packet = json_output(&mut command);

    assert_eq!(packet["budget"]["max_tokens"], 90);
    assert!(packet["budget"]["estimated_tokens"].as_u64().unwrap() <= 90);
    assert_eq!(packet["truncation"]["truncated"], true);
    assert!(packet["truncation"]["omitted_results"].as_u64().unwrap() > 0);
    assert_eq!(packet["truncation"]["reason"], "token_budget");
    assert_eq!(packet["pagination"]["has_more"], true);
}

#[test]
fn search_json_redacts_secret_like_snippets() {
    let temp = tempdir();
    record(
        &temp,
        "Deploy token cleanup",
        "deploy failed with token=ghp_1234567890abcdef1234567890abcdef and password=hunter2",
        &["deploy"],
    );

    let mut command = ctx(&temp);
    command.args(["search", "password", "--json"]);
    let packet = json_output(&mut command);
    let snippet = packet["results"][0]["snippet"].as_str().unwrap();

    assert!(snippet.contains("[REDACTED_SECRET]"));
    assert!(!snippet.contains("ghp_123456"));
    assert!(!snippet.contains("hunter2"));

    let mut command = ctx(&temp);
    command.args(["context", "deploy", "--json"]);
    let packet = json_output(&mut command);
    let summary = packet["results"][0]["summary"].as_str().unwrap();

    assert!(summary.contains("[REDACTED_SECRET]"));
    assert!(!summary.contains("ghp_123456"));
    assert!(!summary.contains("hunter2"));
}

#[test]
fn search_and_context_json_include_evidence_output_only_matches() {
    let temp = tempdir();
    let stdout_path = temp.path().join("stdout.txt");
    let stderr_path = temp.path().join("stderr.txt");
    fs::write(
        &stdout_path,
        "stdout-only-needle token=ghp_1234567890abcdef cwd=/home/daddy/code/project",
    )
    .unwrap();
    fs::write(&stderr_path, "").unwrap();

    ctx(&temp)
        .args([
            "capture",
            "write-shim-command",
            "--provider",
            "git",
            "--exit-code",
            "0",
            "--stdout-file",
            stdout_path.to_str().unwrap(),
            "--stderr-file",
            stderr_path.to_str().unwrap(),
            "--started-at",
            "2026-06-22T12:00:00Z",
            "--duration-ms",
            "7",
            "git",
            "status",
        ])
        .assert()
        .success();

    ctx(&temp).args(["capture", "import"]).assert().success();

    let mut command = ctx(&temp);
    command.args(["search", "stdout-only-needle", "--json"]);
    let packet = json_output(&mut command);
    assert_eq!(packet["results"].as_array().unwrap().len(), 1);
    let result = &packet["results"][0];
    assert!(result["why_matched"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == "evidence_output"));
    let snippet = result["snippet"].as_str().unwrap();
    assert!(snippet.contains("stdout-only-needle"));
    assert!(snippet.contains("token=[REDACTED_SECRET]"));
    assert!(snippet.contains("[REDACTED_PATH]"));
    assert!(!snippet.contains("ghp_123456"));
    assert!(!snippet.contains("/home/daddy/code/project"));

    let mut command = ctx(&temp);
    command.args(["context", "stdout-only-needle", "--json"]);
    let packet = json_output(&mut command);
    assert_eq!(packet["results"].as_array().unwrap().len(), 1);
    let summary = packet["results"][0]["summary"].as_str().unwrap();
    assert!(summary.contains("stdout-only-needle"));
    assert!(summary.contains("token=[REDACTED_SECRET]"));
    assert!(summary.contains("[REDACTED_PATH]"));
    assert!(!summary.contains("ghp_123456"));
    assert!(!summary.contains("/home/daddy/code/project"));
}

#[test]
fn context_markdown_redacts_record_fields_and_commands() {
    let temp = tempdir();
    record(
        &temp,
        "Deploy token=ghp_1234567890abcdef",
        "password=hunter2 under /home/daddy/code/project",
        &["secret=shhh"],
    );

    ctx(&temp)
        .args(["context", "password"])
        .assert()
        .success()
        .stdout(predicate::str::contains("token=[REDACTED_SECRET]"))
        .stdout(predicate::str::contains("password=[REDACTED_SECRET]"))
        .stdout(predicate::str::contains("[REDACTED_PATH]"))
        .stdout(predicate::str::contains("hunter2").not())
        .stdout(predicate::str::contains("ghp_123456").not())
        .stdout(predicate::str::contains("/home/daddy/code/project").not())
        .stdout(predicate::str::contains("secret=shhh").not());
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
    assert!(!evidence["evidence"]["stdout"]
        .as_str()
        .unwrap()
        .contains("version"));
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
    let archive_json: Value =
        serde_json::from_str(&std::fs::read_to_string(&archive).unwrap()).unwrap();
    assert_eq!(archive_json["schema_version"], 1);
    assert_eq!(archive_json["version"], 1);

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
        .stderr(predicate::str::contains("record not found"));

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
