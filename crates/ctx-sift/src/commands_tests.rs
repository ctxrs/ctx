// Adapted from Sift tests, MIT; see this crate’s NOTICE.
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "sift-commands-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut command = crate::test_support::command(args);
        command
            .current_dir(&self.0)
            .env("SIFT_CONFIG_DIR", self.0.join("config"))
            .env("SIFT_STATE_DIR", self.0.join("state"));
        command
    }
    fn output(&self, args: &[&str], input: &[u8]) -> Output {
        let mut child = self
            .command(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        child
            .wait_with_output()
            .map(crate::test_support::clean)
            .unwrap()
    }
    fn settings(&self, json: &str) {
        fs::create_dir_all(self.0.join("config")).unwrap();
        fs::write(self.0.join("config/config.json"), json).unwrap();
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn explicit_read_json_and_filter_workflows_keep_their_contracts() {
    let root = Sandbox::new();
    let text = "worker completed a repeated checkpoint\r\n".repeat(80);
    let compact = root.output(&["compact"], text.as_bytes());
    let read = root.output(&["read"], text.as_bytes());
    assert!(read.status.success());
    assert_eq!(read.stdout, compact.stdout);
    let read = root.output(
        &["read", "--from", "2", "--lines", "1"],
        b"one\r\ntwo\r\nthree",
    );
    assert!(read.status.success());
    assert_eq!(read.stdout, b"two\r\n");
    let selected = root.output(
        &["json", "--pointer", "/rows", "--limit", "1", "--field", "n"],
        br#"{"rows":[{"n":-0e+9999,"x":true},{"n":2}]}"#,
    );
    assert!(
        selected.status.success(),
        "{}",
        String::from_utf8_lossy(&selected.stderr)
    );
    assert_eq!(selected.stdout, b"[{\"n\":-0e+9999}]\n");
    let filtered = root.output(&["filter", "--capture"], text.as_bytes());
    assert!(filtered.status.success());
    assert_eq!(filtered.stdout, compact.stdout);
}

#[cfg(unix)]
#[test]
fn automatic_status_keeps_every_path_and_measures_the_selected_view() {
    use std::os::unix::fs::PermissionsExt;
    let root = Sandbox::new();
    let mut original = String::from("On branch main\nChanges to be committed:\n");
    let mut presentation = String::from("## main\n");
    for i in 0..1000 {
        let status = if i < 10 || i % 2 != 0 {
            "modified:   "
        } else {
            "deleted:    "
        };
        original.push_str(&format!(
            "\t{status}src/path/common/ordinary_case_{i:05}.rs\n"
        ));
        let code = if i < 10 || i % 2 != 0 { "M" } else { "D" };
        presentation.push_str(&format!(
            "{code}  src/path/common/ordinary_case_{i:05}.rs\n"
        ));
    }
    fs::write(root.0.join("status-output"), &original).unwrap();
    let program = root.0.join("git");
    fs::write(
        &program,
        "#!/bin/sh\n[ \"$1\" = status ] || exit 2\nprintf invoked >> marker\ncat status-output\n",
    )
    .unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
    let compactor = sift::Compactor::new().unwrap();
    let expected = compactor.compact(&presentation);
    assert!(expected.output_tokens < compactor.compact(&original).output_tokens);
    let output = root.output(&["run", "--", program.to_str().unwrap(), "status"], b"");
    assert!(output.status.success());
    assert_eq!(output.stdout, expected.text.as_bytes());
    assert_eq!(
        sift::restore(expected.encoding, &expected.text).unwrap(),
        presentation
    );
    assert_eq!(fs::read(root.0.join("marker")).unwrap(), b"invoked");
    let gain = root.output(&["gain", "--json", "--history"], b"");
    let gain: serde_json::Value = serde_json::from_slice(&gain.stdout).unwrap();
    let event = &gain["history"][0];
    assert_eq!(event["input_tokens"], compactor.count_tokens(&original));
    assert_eq!(event["output_tokens"], expected.output_tokens);
    assert_eq!(event["source"], "run-view");
}

#[cfg(unix)]
#[test]
fn automatic_test_summary_records_actual_tokens_and_keeps_recovery_and_status() {
    use std::os::unix::fs::PermissionsExt;
    let root = Sandbox::new();
    root.settings(r#"{"keep_originals":true}"#);
    let mut original = String::from("running 101 tests\n");
    for i in 0..100 {
        original.push_str(&format!("test successful_case_{i} ... ok\n"));
    }
    original.push_str("test rejected_input ... FAILED\n\nfailures:\n\n---- rejected_input stdout ----\nsrc/input.rs:17: expected 200, actual 503\n\nfailures:\n    rejected_input\n\ntest result: FAILED. 100 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n");
    fs::write(root.0.join("test-output"), &original).unwrap();
    let program = root.0.join("cargo");
    fs::write(&program, "#!/bin/sh\n[ \"$1\" = test ] || exit 2\nprintf invoked >> marker\ncat test-output\nprintf 'warning preserved\\n' >&2\nexit 17\n").unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
    let output = root.output(&["run", "--", program.to_str().unwrap(), "test"], b"");
    assert_eq!(output.status.code(), Some(17));
    assert_eq!(fs::read(root.0.join("marker")).unwrap(), b"invoked");
    assert_eq!(output.stderr, b"warning preserved\n");
    let text = String::from_utf8(output.stdout.clone()).unwrap();
    assert!(text.contains("src/input.rs:17: expected 200, actual 503"));
    assert!(text.contains("test rejected_input ... FAILED"));
    assert!(text.contains("100 passed; 1 failed"));
    assert!(!text.contains("successful_case_99"));
    let tokenizer = tiktoken_rs::o200k_base().unwrap();
    let input_tokens = tokenizer.encode_ordinary(&original).len()
        + tokenizer.encode_ordinary("warning preserved\n").len();
    let output_tokens = tokenizer.encode_ordinary(&text).len()
        + tokenizer.encode_ordinary("warning preserved\n").len();
    assert!(output_tokens < input_tokens);
    let gain = root.output(&["gain", "--json", "--history"], b"");
    let gain: serde_json::Value = serde_json::from_slice(&gain.stdout).unwrap();
    let event = &gain["history"][0];
    assert_eq!(event["input_tokens"], input_tokens);
    assert_eq!(event["output_tokens"], output_tokens);
    assert_eq!(event["source"], "run-view");
    let id = event["original_id"].as_str().unwrap();
    let restored = root.output(&["recall", id], b"");
    assert!(restored.status.success());
    assert_eq!(restored.stdout, original.as_bytes());
    assert_eq!(fs::read(root.0.join("marker")).unwrap(), b"invoked");
}

#[cfg(unix)]
#[test]
fn command_views_execute_once_and_keep_both_streams_and_exit_status() {
    let root = Sandbox::new();
    let result = root.output(&["test", "--context", "0", "--", "sh", "-c", "printf 'run\\n' >> marker; printf 'prefix\\nFAIL example\\nfooter\\n'; printf 'warning detail\\n' >&2; exit 17"], b"");
    assert_eq!(result.status.code(), Some(17));
    assert_eq!(fs::read(root.0.join("marker")).unwrap(), b"run\n");
    let stdout = String::from_utf8(result.stdout).unwrap();
    assert!(stdout.contains("FAIL example"));
    assert!(!stdout.contains("prefix"));
    assert_eq!(result.stderr, b"warning detail\n");
    let stats = root.output(&["gain", "--json", "--history"], b"");
    assert!(stats.status.success());
    // Explicit excerpt output is not assigned lossless compaction token counts.
    let stats: serde_json::Value = serde_json::from_slice(&stats.stdout).unwrap();
    assert!(stats["history"][0]["input_tokens"].is_null());
    assert_eq!(stats["history"][0]["source"], "view");
    let normal = root.output(&["run", "--", "test", "-f", "marker"], b"");
    assert!(normal.status.success());
}

#[test]
fn plugin_protocol_respects_disabled_recording_and_config_without_changing_wire_contract() {
    let root = Sandbox::new();
    root.settings(r#"{"enabled":false,"record_usage":false}"#);
    let text = "repeated completed step\n".repeat(100);
    let request = serde_json::to_vec(&serde_json::json!({"version":1,"text":text})).unwrap();
    let result = root.output(
        &["compact", "--protocol=json-v1", "--record-source", "pi"],
        &request,
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let response: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(response["text"], text);
    assert_eq!(response["encoding"], "raw");
    assert_eq!(response["input_tokens"], response["output_tokens"]);
    assert!(!root.0.join("state").exists());
    let explicit = root.output(&["compact"], text.as_bytes());
    assert!(explicit.status.success());
    assert_ne!(explicit.stdout, text.as_bytes()); // Explicit compact remains usable.
}

#[test]
fn plugin_tool_exclusions_preserve_output_and_record_the_actual_tool_label() {
    let root = Sandbox::new();
    root.settings(r#"{"exclude_commands":["bash"]}"#);
    let text = "repeated completed step\n".repeat(100);
    let request = serde_json::to_vec(&serde_json::json!({"version":1,"text":text})).unwrap();
    for (tool, excluded) in [("bash", true), ("powershell", false)] {
        let result = root.output(
            &[
                "compact",
                "--protocol=json-v1",
                "--record-source",
                "pi",
                "--record-tool",
                tool,
            ],
            &request,
        );
        assert!(result.status.success());
        let response: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(response["encoding"] == "raw", excluded);
        if excluded {
            assert_eq!(response["text"], text);
        }
    }
    let gain = root.output(&["gain", "--json", "--history"], b"");
    let gain: serde_json::Value = serde_json::from_slice(&gain.stdout).unwrap();
    let labels: Vec<_> = gain["history"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["command"].as_str().unwrap())
        .collect();
    assert_eq!(labels.len(), 2);
    assert!(labels.contains(&"bash") && labels.contains(&"powershell"));
    assert!(
        !root
            .output(
                &["compact", "--protocol=json-v1", "--record-tool", "bash"],
                b""
            )
            .status
            .success()
    );
}

#[test]
fn undelivered_plugin_response_does_not_record_savings() {
    let root = Sandbox::new();
    let mut child = root
        .command(&["compact", "--protocol=json-v1", "--record-source", "pi"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    crate::test_support::close_stdout(&mut child);
    let text = "repeated result\n".repeat(100);
    let request = serde_json::to_vec(&serde_json::json!({"version":1,"text":text})).unwrap();
    child.stdin.take().unwrap().write_all(&request).unwrap();
    let _ = child.wait().unwrap();
    assert!(!root.0.join("state").exists());
}

#[test]
fn undelivered_native_hook_response_does_not_record_savings() {
    let root = Sandbox::new();
    let mut child = root
        .command(&["hook", "claude"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    crate::test_support::close_stdout(&mut child);
    let request = serde_json::to_vec(&serde_json::json!({
        "hook_event_name":"PostToolUse", "tool_name":"Bash",
        "tool_response":{"stdout":"repeated result\n".repeat(100), "stderr":""}
    }))
    .unwrap();
    child.stdin.take().unwrap().write_all(&request).unwrap();
    assert!(child.wait().unwrap().success());
    assert!(!root.0.join("state").exists());
}

#[cfg(unix)]
#[test]
fn run_records_exact_savings_and_recalls_original_without_rerunning() {
    let root = Sandbox::new();
    root.settings(r#"{"keep_originals":true}"#);
    let script = "printf invoked >> marker; i=0; while [ $i -lt 300 ]; do printf 'repeated diagnostic preserves all information\\n'; i=$((i+1)); done; exit 7";
    let result = root.output(&["run", "--", "sh", "-c", script], b"");
    assert_eq!(result.status.code(), Some(7));
    assert!(result.stderr.is_empty());
    let original = "repeated diagnostic preserves all information\n".repeat(300);
    let expected = sift::Compactor::new().unwrap().compact(&original);
    assert_eq!(result.stdout, expected.text.as_bytes());
    let gain = root.output(&["gain", "--json", "--history"], b"");
    assert!(gain.status.success());
    let gain: serde_json::Value = serde_json::from_slice(&gain.stdout).unwrap();
    assert_eq!(gain["totals"]["input_tokens"], expected.input_tokens);
    assert_eq!(gain["totals"]["output_tokens"], expected.output_tokens);
    let id = gain["history"][0]["original_id"].as_str().unwrap();
    let recalled = root.output(&["recall", id], b"");
    assert_eq!(recalled.stdout, original.as_bytes());
    assert_eq!(fs::read(root.0.join("marker")).unwrap(), b"invoked");
}

#[cfg(unix)]
#[test]
fn excluded_and_invalid_configuration_leave_command_output_raw() {
    let root = Sandbox::new();
    root.settings(r#"{"exclude_commands":["sh"]}"#);
    let script =
        "i=0; while [ $i -lt 100 ]; do printf 'unchanged line\\n'; i=$((i+1)); done; exit 3";
    let expected = "unchanged line\n".repeat(100);
    let first = root.output(&["run", "--", "sh", "-c", script], b"");
    assert_eq!(first.status.code(), Some(3));
    assert_eq!(first.stdout, expected.as_bytes());
    root.settings("invalid JSON");
    let second = root.output(&["run", "--", "sh", "-c", script], b"");
    assert_eq!(second.status.code(), Some(3));
    assert_eq!(second.stdout, expected.as_bytes());
    assert!(String::from_utf8_lossy(&second.stderr).contains("passing command output through"));
}

#[test]
fn discover_reports_saved_output_without_execution_or_usage_mutations() {
    let root = Sandbox::new();
    let text = "echo do-not-execute >> marker\n".repeat(200);
    fs::write(root.0.join("-output.txt"), &text).unwrap();
    fs::write(root.0.join("binary"), [0xff, 0]).unwrap();
    let output = root.output(&["discover", "--json", "--", "-output.txt", "binary"], b"");
    assert!(output.status.success());
    let rows: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let compact = sift::Compactor::new().unwrap().compact(&text);
    assert_eq!(rows[0]["input_tokens"], compact.input_tokens);
    assert_eq!(
        rows[0]["saved_tokens"],
        compact.input_tokens - compact.output_tokens
    );
    assert!(rows[1]["input_tokens"].is_null());
    assert!(!root.0.join("marker").exists());
    assert!(!root.0.join("state").exists());
}
