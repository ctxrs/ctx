// Adapted from Sift tests, MIT; see this crate’s NOTICE.
use serde_json::{Value, json};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "sift-discover-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        Self(dir)
    }
    fn write(&self, name: &str, records: &[Value]) {
        let text = records
            .iter()
            .map(|v| serde_json::to_string(v).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(self.0.join(name), text).unwrap();
    }
    fn run(&self, args: &[&str], stdin: &[u8]) -> Output {
        self.cli("discover", args, stdin)
    }
    fn cli(&self, subcommand: &str, args: &[&str], stdin: &[u8]) -> Output {
        let mut child =
            crate::test_support::command(std::iter::once(subcommand).chain(args.iter().copied()))
                .current_dir(&self.0)
                .env("SIFT_CONFIG_DIR", self.0.join("config"))
                .env("SIFT_STATE_DIR", self.0.join("state"))
                .env("HOME", &self.0)
                .env("USERPROFILE", &self.0)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
        child.stdin.take().unwrap().write_all(stdin).unwrap();
        child
            .wait_with_output()
            .map(crate::test_support::clean)
            .unwrap()
    }
    fn rewritten(&self, command: &str) -> String {
        let result = self.cli(
            "rewrite",
            &["--json", "--shell", "posix", "--", command],
            b"",
        );
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let report: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(report["changed"], true);
        report["command"].as_str().unwrap().replace(
            &std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .to_string(),
            "/opt/ctx",
        )
    }
    fn report(&self, args: &[&str]) -> Value {
        let result = self.run(args, b"");
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(!self.0.join("state").exists());
        assert!(!self.0.join("config").exists());
        serde_json::from_slice(&result.stdout).unwrap()
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn claude_call(id: &str, cmd: &str) -> Value {
    json!({"type":"assistant", "cwd":"/synthetic/project", "timestamp":"2026-09-17T10:00:00.000Z", "message":{"content":[{"type":"tool_use","name":"Bash","id":id,"input":{"command":cmd}}]}})
}
fn claude_result(id: &str, output: Value, error: bool) -> Value {
    json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":id,"content":output,"is_error":error}]}})
}
fn codex_call(id: &str, cmd: Value) -> Value {
    json!({"type":"response_item", "timestamp":"2026-09-17T10:00:00Z", "payload":{"type":"function_call","name":"exec_command","call_id":id,"arguments":json!({"cmd":cmd}).to_string()}})
}
fn codex_result(id: &str, output: Value) -> Value {
    json!({"type":"response_item", "payload":{"type":"function_call_output","call_id":id,"output":output}})
}
fn repeat() -> String {
    "synthetic completed compilation step\n".repeat(80)
}

#[test]
fn claude_pairs_by_id_counts_only_captured_outputs_and_omits_secrets() {
    let s = Sandbox::new();
    let records = vec![
        claude_call("private-session-id", "cargo build --token=SYNTHETIC_SECRET"),
        claude_call("missing", "git status"),
        claude_result("private-session-id", json!(repeat()), false),
        claude_call("sift", "'/synthetic/bin/sift' run cargo build"),
        claude_result("sift", json!(repeat()), false),
        claude_call("not-sift", "echo sift --token=SYNTHETIC_SECRET"),
        claude_result("not-sift", json!("ok"), false),
        claude_call("shell", "echo sift; touch should-not-exist"),
        claude_result("shell", json!("ok"), false),
        claude_result("orphan", json!(repeat()), false),
    ];
    s.write("session.jsonl", &records);
    let original = fs::read(s.0.join("session.jsonl")).unwrap();
    let report = s.report(&["--history", "session.jsonl", "--json"]);
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["rows"].as_array().unwrap().len(), 5);
    assert_eq!(report["sift_calls"], 1);
    assert_eq!(report["missed_opportunities"], 1);
    let rows = &report["rows"];
    assert_eq!(rows[0]["command"], "cargo build");
    assert!(rows[0]["potential_saved_tokens"].as_u64().unwrap() > 0);
    assert_eq!(rows[1]["output_status"], "missing");
    assert!(rows[1]["potential_saved_tokens"].is_null());
    assert_eq!(rows[2]["classification"], "sift_usage");
    assert!(rows[2]["potential_saved_tokens"].is_null());
    assert_eq!(rows[3]["command"], "echo");
    assert_eq!(rows[4]["command"], "unknown");
    let serialized = report.to_string();
    for secret in [
        "SYNTHETIC_SECRET",
        "private-session-id",
        "/synthetic",
        "compilation",
        "touch",
    ] {
        assert!(!serialized.contains(secret), "leaked {secret}");
    }
    assert_eq!(fs::read(s.0.join("session.jsonl")).unwrap(), original);
    assert!(!s.0.join("should-not-exist").exists());
}

#[test]
fn codex_canonical_results_ignore_mirrored_events_and_handle_envelopes() {
    let s = Sandbox::new();
    let text = repeat();
    s.write("session.jsonl", &[
        json!({"type":"session_meta","payload":{"cwd":"/synthetic/project"}}),
        codex_call("one", json!("cargo build")),
        json!({"type":"event_msg","payload":{"type":"exec_command_end","call_id":"one","aggregated_output":text,"exit_code":0}}),
        codex_result("one", json!(format!("Chunk ID: synthetic\nWall time: 1 seconds\nProcess exited with code 0\nFinal output:\n{text}"))),
        codex_result("one", json!("duplicate ignored")),
        codex_call("two", json!(["bash", "-lc", "sift run git status"])),
        codex_result("two", json!({"output":"ok","metadata":{"exit_code":0}}).to_string().into()),
        codex_call("three", json!("git diff")),
        codex_result("three", json!([{"type":"input_text","text":text}])),
        json!({"type":"response_item","payload":{"type":"function_call","name":"web_search","call_id":"web","arguments":"{}"}}),
        codex_result("web", json!(repeat())),
    ]);
    let report = s.report(&[
        "--history",
        "session.jsonl",
        "--json",
        "--project",
        "/synthetic/project",
    ]);
    assert_eq!(report["rows"].as_array().unwrap().len(), 3);
    assert_eq!(report["sift_calls"], 1);
    assert_eq!(report["missed_opportunities"], 2);
    assert_eq!(report["rows"][0]["captured_bytes"], text.len());
    assert_eq!(
        report["rows"][2]["input_tokens"],
        report["rows"][0]["input_tokens"]
    );
}

#[test]
fn suggestions_are_observed_pairs_with_source_counts_and_no_rule_writes() {
    let s = Sandbox::new();
    fs::create_dir(s.0.join("history")).unwrap();
    for file in ["history/a.jsonl", "history/b.jsonl"] {
        s.write(
            file,
            &[
                claude_call("bad", "cargo biuld --token=SYNTHETIC_SECRET"),
                claude_result("bad", json!("unknown command"), true),
                claude_call("good", "cargo build --token=SYNTHETIC_SECRET"),
                claude_result("good", json!("ok"), false),
                claude_call("changed-task", "cargo test --token=SYNTHETIC_SECRET"),
                claude_result("changed-task", json!("ok"), false),
            ],
        );
    }
    let plain = s.report(&["--history", "history", "--json"]);
    assert!(plain["suggestions"].as_array().unwrap().is_empty());
    let report = s.report(&["--history", "history", "--json", "--suggest"]);
    assert_eq!(report["suggestions"].as_array().unwrap().len(), 1);
    assert_eq!(
        report["suggestions"][0]["pattern"],
        "cargo biuld -> cargo build"
    );
    assert_eq!(report["suggestions"][0]["occurrences"], 2);
    assert_eq!(report["suggestions"][0]["source_count"], 2);
    assert!(!report.to_string().contains("SYNTHETIC_SECRET"));
    assert!(!s.0.join(".claude").exists());
    let second = s.report(&["--history", "history", "--json", "--suggest"]);
    assert_eq!(report, second);
}

#[test]
fn suggestions_need_status_and_do_not_cross_sources_or_unrelated_calls() {
    let s = Sandbox::new();
    fs::create_dir(s.0.join("history")).unwrap();
    s.write(
        "history/a.jsonl",
        &[
            codex_call("bad", json!("cargo biuld")),
            codex_result("bad", json!("error: command failed")), // prose is not exit metadata
            codex_call("good", json!("cargo build")),
            codex_result("good", json!("ok")),
            claude_call("c-bad", "git statsu"),
            claude_result("c-bad", json!("bad"), true),
        ],
    );
    s.write(
        "history/b.jsonl",
        &[
            claude_call("c-good", "git status"),
            claude_result("c-good", json!("ok"), false),
        ],
    );
    let report = s.report(&["--history", "history", "--json", "--suggest"]);
    assert!(report["suggestions"].as_array().unwrap().is_empty());
}

#[test]
fn filters_use_command_time_and_project_and_exclude_unknown_metadata() {
    let s = Sandbox::new();
    let mut older = claude_call("old", "cargo build");
    older["timestamp"] = json!("2026-09-16T23:59:59Z");
    let mut other = claude_call("other", "cargo build");
    other["cwd"] = json!("/synthetic/another");
    let mut unknown = claude_call("unknown", "cargo build");
    unknown.as_object_mut().unwrap().remove("timestamp");
    s.write(
        "session.jsonl",
        &[
            older,
            claude_result("old", json!(repeat()), false),
            other,
            claude_result("other", json!(repeat()), false),
            unknown,
            claude_result("unknown", json!(repeat()), false),
            claude_call("current", "cargo build"),
            claude_result("current", json!(repeat()), false),
        ],
    );
    let report = s.report(&[
        "--history",
        "session.jsonl",
        "--json",
        "--since",
        "2026-09-17",
        "--project",
        "/synthetic/project",
    ]);
    assert_eq!(report["rows"].as_array().unwrap().len(), 1);
    assert_eq!(report["excluded_rows"], 3);
    assert_eq!(report["rows"][0]["id"], "source-1:4");
    for date in [
        "2026-02-29",
        "2026-13-01",
        "yesterday",
        "2026-09-17T00:00:00Z",
    ] {
        assert!(
            !s.run(&["--history", "session.jsonl", "--since", date], b"")
                .status
                .success()
        );
    }
}

#[test]
fn replay_file_stdin_and_dash_filename_contract_is_unchanged() {
    let s = Sandbox::new();
    fs::write(s.0.join("--history"), repeat()).unwrap();
    let report = s.report(&["--json", "--", "--history"]);
    assert!(report.is_array());
    assert_eq!(report[0]["file"], "--history");
    assert!(report[0]["saved_tokens"].as_u64().unwrap() > 0);
    let result = s.run(&["--json"], repeat().as_bytes());
    assert!(result.status.success());
    let stdin: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(stdin[0]["file"], "-");
    assert_eq!(stdin[0]["saved_tokens"], report[0]["saved_tokens"]);
    assert!(!s.run(&["--suggest"], b"").status.success());
    assert!(!s.run(&["--history", "--json"], b"").status.success());
}

#[test]
fn saved_outputs_binary_and_malformed_history_do_not_invent_measurements() {
    let s = Sandbox::new();
    fs::create_dir(s.0.join("history")).unwrap();
    fs::write(s.0.join("history/a.txt"), repeat()).unwrap();
    fs::write(s.0.join("history/b.bin"), [0xff, 0xfe]).unwrap();
    fs::write(s.0.join("history/c.jsonl"), "{broken json\n{\"type\":\"user\",\"message\":{\"content\":\"do not measure transcript prose\"}}\n").unwrap();
    let report = s.report(&["--history", "history", "--json"]);
    assert_eq!(report["files_scanned"], 3);
    assert_eq!(report["malformed_records"], 1);
    assert_eq!(report["rows"].as_array().unwrap().len(), 2);
    assert_eq!(report["missed_opportunities"], 1);
    assert_eq!(report["rows"][1]["output_status"], "unsupported");
    assert!(report["rows"][1]["potential_saved_tokens"].is_null());
}

#[test]
fn oversized_output_and_mixed_content_are_unmeasured() {
    let s = Sandbox::new();
    s.write("session.jsonl", &[
        claude_call("big", "cargo build"),
        claude_result("big", json!("a".repeat(256 * 1024 + 1)), false),
        claude_call("mixed", "cargo test"),
        claude_result("mixed", json!([{"type":"text","text":repeat()},{"type":"image","source":{"type":"base64","data":"synthetic"}}]), false),
    ]);
    let report = s.report(&["--history", "session.jsonl", "--json"]);
    assert_eq!(report["rows"][0]["output_status"], "measurement_limit");
    assert_eq!(report["rows"][1]["output_status"], "unsupported");
    assert_eq!(report["potential_saved_tokens"], 0);
}

#[test]
fn bounded_directory_scan_skips_large_files() {
    let s = Sandbox::new();
    fs::create_dir(s.0.join("history")).unwrap();
    for n in 0..130 {
        let file = fs::File::create(s.0.join(format!("history/{n:03}.jsonl"))).unwrap();
        if n == 0 {
            file.set_len(16 * 1024 * 1024 + 1).unwrap();
        }
    }
    let report = s.report(&["--history", "history", "--json"]);
    assert_eq!(report["scan_limited"], true);
    assert_eq!(report["skipped_files"], 1);
    assert_eq!(report["files_scanned"], 127);
}

#[cfg(unix)]
#[test]
fn symlinks_and_special_files_are_not_followed() {
    use std::os::unix::fs::symlink;
    let s = Sandbox::new();
    fs::create_dir(s.0.join("history")).unwrap();
    fs::write(s.0.join("outside.txt"), repeat()).unwrap();
    symlink(s.0.join("outside.txt"), s.0.join("history/link")).unwrap();
    symlink(&s.0, s.0.join("history/loop")).unwrap();
    let fifo =
        std::ffi::CString::new(s.0.join("history/fifo").as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let report = s.report(&["--history", "history", "--json"]);
    assert_eq!(report["skipped_files"], 3);
    assert_eq!(report["files_scanned"], 0);
    assert!(report["rows"].as_array().unwrap().is_empty());
}

#[test]
fn suggestions_require_failure_before_retry_and_recognize_option_typos() {
    let s = Sandbox::new();
    s.write(
        "session.jsonl",
        &[
            claude_call("parallel-bad", "cargo biuld"),
            claude_call("parallel-good", "cargo build"),
            claude_result("parallel-bad", json!("unknown command"), true),
            claude_result("parallel-good", json!("ok"), false),
            codex_call("bad", json!("git status --quite")),
            codex_result(
                "bad",
                json!({"output":"invalid option", "metadata":{"exit_code":129}})
                    .to_string()
                    .into(),
            ),
            codex_call("good", json!("git status --quiet")),
            codex_result(
                "good",
                json!({"output":"ok", "metadata":{"exit_code":0}})
                    .to_string()
                    .into(),
            ),
        ],
    );
    let report = s.report(&["--history", "session.jsonl", "--json", "--suggest"]);
    assert_eq!(report["suggestions"].as_array().unwrap().len(), 1);
    assert_eq!(
        report["suggestions"][0]["pattern"],
        "git status: --quite -> --quiet"
    );
}

#[test]
fn record_and_depth_limits_are_reported() {
    let s = Sandbox::new();
    fs::write(
        s.0.join("many.jsonl"),
        "{\"type\":\"event_msg\"}\n".repeat(20_001),
    )
    .unwrap();
    let report = s.report(&["--history", "many.jsonl", "--json"]);
    assert_eq!(report["records_scanned"], 20_000);
    assert_eq!(report["scan_limited"], true);
    let mut deep = s.0.join("history");
    for _ in 0..10 {
        deep.push("nested");
    }
    fs::create_dir_all(&deep).unwrap();
    fs::write(deep.join("out.txt"), repeat()).unwrap();
    let report = s.report(&["--history", "history", "--json"]);
    assert_eq!(report["files_scanned"], 0);
    assert_eq!(report["scan_limited"], true);
}

#[test]
fn canonical_local_shell_and_out_of_order_results_pair_without_prose_capture() {
    let s = Sandbox::new();
    s.write("session.jsonl", &[
        codex_result("local", json!(repeat())),
        json!({"type":"response_item", "timestamp":"2026-09-17T10:00:00Z", "payload":{
            "type":"local_shell_call", "call_id":"local", "action":{"type":"exec", "command":["git","status"], "working_directory":"/synthetic/project"}}}),
        json!({"type":"assistant","message":{"content":[{"type":"text","text":repeat()}]}}),
    ]);
    let report = s.report(&[
        "--history",
        "session.jsonl",
        "--json",
        "--project",
        "/synthetic/project",
    ]);
    assert_eq!(report["rows"].as_array().unwrap().len(), 1);
    assert_eq!(report["rows"][0]["command"], "git status");
    assert_eq!(report["missed_opportunities"], 1);
}

#[test]
fn locators_use_physical_lines_and_blocks_and_resolve_suggestion_pairs() {
    let s = Sandbox::new();
    fs::create_dir_all(s.0.join("history/nested")).unwrap();
    let mut bad = claude_call("bad-private-id", "cargo biuld --token=SYNTHETIC_SECRET");
    bad["message"]["content"]
        .as_array_mut()
        .unwrap()
        .insert(0, json!({"type":"text","text":"private prose"}));
    let mut good = claude_call("good-private-id", "cargo build --token=SYNTHETIC_SECRET");
    good["message"]["content"]
        .as_array_mut()
        .unwrap()
        .push(claude_call("other-private-id", "git status")["message"]["content"][0].clone());
    let file = s.0.join("history/nested/session.jsonl");
    fs::write(
        &file,
        format!(
            "\nnot json\n{}\n\n{}\n{}\n{}\n",
            bad,
            claude_result("bad-private-id", json!("failure details"), true),
            good,
            claude_result("good-private-id", json!("success details"), false)
        ),
    )
    .unwrap();
    let args = [
        "--history",
        "history",
        "--json",
        "--suggest",
        "--since",
        "2026-09-17",
    ];
    let report = s.report(&args);
    assert_eq!(report["malformed_records"], 1);
    assert_eq!(
        report["sources"]["source-1"]["path"],
        PathBuf::from("nested")
            .join("session.jsonl")
            .to_str()
            .unwrap()
    );
    let rows = &report["rows"];
    assert_eq!(rows[0]["location"], json!({"line":3,"block_index":1}));
    assert_eq!(rows[1]["location"], json!({"line":6,"block_index":0}));
    assert_eq!(rows[2]["location"], json!({"line":6,"block_index":1}));
    assert_eq!(
        report["suggestions"][0]["evidence"],
        json!([{
            "source_id":"source-1", "failed":{"line":3,"block_index":1},
            "corrected":{"line":6,"block_index":0}
        }])
    );
    // Follow the emitted locators directly into the original file, without
    // reconstructing the scanner's recognized-call order.
    for row in rows.as_array().unwrap() {
        let source = &report["sources"][row["source_id"].as_str().unwrap()];
        let text =
            fs::read_to_string(s.0.join("history").join(source["path"].as_str().unwrap())).unwrap();
        let record: Value = serde_json::from_str(
            text.lines()
                .nth(row["location"]["line"].as_u64().unwrap() as usize - 1)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            record["message"]["content"][row["location"]["block_index"].as_u64().unwrap() as usize]
                ["type"],
            "tool_use"
        );
    }
    for secret in [
        "SYNTHETIC_SECRET",
        "private-id",
        "private prose",
        "failure details",
        "success details",
    ] {
        assert!(!report.to_string().contains(secret));
    }
    // An earlier-sorted source renumbers scan IDs, but the path/record locator
    // remains usable and unchanged.
    fs::write(s.0.join("history/aaa.txt"), "unrelated").unwrap();
    let reordered = s.report(&args);
    assert_eq!(reordered["rows"][0]["source_id"], "source-2");
    assert_eq!(
        reordered["sources"]["source-2"],
        report["sources"]["source-1"]
    );
    assert_eq!(reordered["rows"][0]["location"], rows[0]["location"]);
    assert_eq!(
        reordered["suggestions"][0]["evidence"][0]["source_id"],
        "source-2"
    );
}

#[test]
fn codex_and_pretty_record_locators_are_physical_and_saved_files_start_at_one() {
    let s = Sandbox::new();
    s.write(
        "codex.jsonl",
        &[
            json!({"type":"session_meta","payload":{"cwd":"/synthetic/project"}}),
            codex_result("out-of-order", json!("ok")),
            codex_call("out-of-order", json!("cargo build")),
        ],
    );
    let report = s.report(&["--history", "codex.jsonl", "--json"]);
    assert_eq!(
        report["rows"][0]["location"],
        json!({"line":3,"block_index":null})
    );
    assert_eq!(report["sources"]["source-1"]["path"], "codex.jsonl");
    let pretty =
        serde_json::to_string_pretty(&claude_call("call-private-id", "git status")).unwrap();
    fs::write(s.0.join("pretty.json"), format!("\n\n{pretty}\n")).unwrap();
    let report = s.report(&["--history", "pretty.json", "--json"]);
    assert_eq!(
        report["rows"][0]["location"],
        json!({"line":3,"block_index":0})
    );
    fs::write(s.0.join("output.txt"), "saved output").unwrap();
    let report = s.report(&["--history", "output.txt", "--json"]);
    assert_eq!(
        report["rows"][0]["location"],
        json!({"line":1,"block_index":null})
    );
}

#[cfg(unix)]
#[test]
fn local_filenames_are_json_resolvable_and_terminal_escaped_in_rows_and_suggestions() {
    let s = Sandbox::new();
    fs::create_dir(s.0.join("history")).unwrap();
    let filename = "session\n\u{1b}[31m\t\"\\\u{202e}\u{9b}🦀.jsonl";
    s.write(
        &format!("history/{filename}"),
        &[
            claude_call("bad-private-id", "cargo biuld --token=SYNTHETIC_SECRET"),
            claude_result("bad-private-id", json!("private error"), true),
            claude_call("good-private-id", "cargo build --token=SYNTHETIC_SECRET"),
            claude_result("good-private-id", json!("private output"), false),
        ],
    );
    let report = s.report(&["--history", "history", "--json", "--suggest"]);
    assert_eq!(report["sources"]["source-1"]["path"], filename);
    let json = s.run(&["--history", "history", "--json", "--suggest"], b"");
    assert!(json.status.success());
    assert!(json.stdout.is_ascii());
    assert!(!json.stdout.contains(&0x1b));
    let decoded: Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(decoded, report);
    let result = s.run(&["--history", "history", "--suggest"], b"");
    assert!(result.status.success());
    let text = String::from_utf8(result.stdout).unwrap();
    assert!(!text.contains(['\u{1b}', '\t', '\u{202e}', '\u{9b}']));
    assert!(text.contains(&format!("{filename:?}:1[block 0]")));
    assert!(text.contains(&format!("{filename:?}:3[block 0]")));
    assert_eq!(text.matches(&format!("{filename:?}:1[block 0]")).count(), 2);
    for secret in [
        "SYNTHETIC_SECRET",
        "private-id",
        "private error",
        "private output",
    ] {
        assert!(!text.contains(secret));
    }
}

#[cfg(unix)]
#[test]
fn non_utf8_filenames_keep_lossless_native_bytes_for_source_lookup() {
    use std::os::unix::ffi::OsStringExt;
    let s = Sandbox::new();
    fs::create_dir(s.0.join("history")).unwrap();
    let filename = std::ffi::OsString::from_vec(b"saved-\xff.txt".to_vec());
    fs::write(s.0.join("history").join(&filename), "saved output").unwrap();
    let report = s.report(&["--history", "history", "--json"]);
    let bytes: Vec<u8> =
        serde_json::from_value(report["sources"]["source-1"]["path_bytes"].clone()).unwrap();
    let restored = std::ffi::OsString::from_vec(bytes);
    assert_eq!(restored, filename);
    let result = s.run(&["--history", "history"], b"");
    assert!(result.status.success());
    assert!(
        String::from_utf8(result.stdout)
            .unwrap()
            .contains(r#"b"saved-\xff.txt":1"#)
    );
    assert_eq!(
        fs::read_to_string(s.0.join("history").join(restored)).unwrap(),
        "saved output"
    );
}

#[test]
fn actual_rewrite_output_and_command_prefixes_count_as_sift_without_execution() {
    let s = Sandbox::new();
    let originals = [
        "cargo build",
        "npm run dev",
        "  cd 'synthetic directory' && git status; cargo test",
        "git status && cargo build || git diff",
        "git status;\ncargo test",
        "rg 'sift; command true || cargo build' 'literal file'",
        r#"rg '$literal' 'quote'\''argument' file"#,
        "git status path\\ ",
        "git status \\\n",
        "cargo build; echo 'touch should-not-exist'",
    ];
    let mut records = Vec::new();
    for (n, original) in originals.iter().enumerate() {
        // This CLI returns a string. Never invoke the resulting command.
        let generated = s.rewritten(original);
        let id = n.to_string();
        records.push(codex_call(&id, json!(generated)));
        records.push(codex_result(&id, json!(repeat())));
    }
    fs::create_dir(s.0.join("config")).unwrap();
    fs::write(
        s.0.join("config/config.json"),
        r#"{"exclude_commands":["git"]}"#,
    )
    .unwrap();
    let excluded = s.rewritten("git status; cargo build");
    fs::remove_file(s.0.join("config/config.json")).unwrap();
    fs::remove_dir(s.0.join("config")).unwrap();
    records.extend([
        codex_call("excluded", json!(excluded)),
        codex_result("excluded", json!(repeat())),
    ]);
    let generated = s.rewritten("cargo build");
    records.extend([
        claude_call("claude", &generated),
        claude_result("claude", json!(repeat()), false),
        codex_call("shell-argv", json!(["bash", "-lc", generated])),
        codex_result("shell-argv", json!(repeat())),
    ]);
    for (n, direct) in [
        "sift run --capture -- cargo build",
        "command sift run --capture -- cargo build",
        "command '/synthetic/O'\"'\"'Brien tools/sift' run -- cargo build",
    ]
    .iter()
    .enumerate()
    {
        let id = format!("direct-{n}");
        records.push(codex_call(&id, json!(direct)));
        records.push(codex_result(&id, json!(repeat())));
    }
    s.write("session.jsonl", &records);
    let before = fs::read(s.0.join("session.jsonl")).unwrap();
    let report = s.report(&["--history", "session.jsonl", "--json"]);
    assert_eq!(report["sift_calls"], originals.len() + 6);
    assert_eq!(report["missed_opportunities"], 0);
    assert_eq!(report["potential_saved_tokens"], 0);
    for row in report["rows"].as_array().unwrap() {
        assert!(matches!(
            row["command"].as_str(),
            Some("sift run" | "ctx sift run" | "ctx output run")
        ));
        assert_eq!(row["classification"], "sift_usage");
        assert!(row["potential_saved_tokens"].is_null());
    }
    assert_eq!(fs::read(s.0.join("session.jsonl")).unwrap(), before);
    assert!(!s.0.join("should-not-exist").exists());
    assert!(!report.to_string().contains("touch"));
}

#[test]
fn wrapper_mentions_changed_guards_and_unsupported_syntax_are_not_sift_usage() {
    let s = Sandbox::new();
    let generated = s.rewritten("cargo build");
    let mismatched = generated.replacen("|| cargo build;", "|| git status;", 1);
    let changed_guard = generated.replacen("command true ||", "command false ||", 1);
    let no_space = generated.replacen("; ", ";", 1);
    let quoted = format!("echo {}", serde_json::to_string(&generated).unwrap());
    let cases = [
        quoted.as_str(),
        mismatched.as_str(),
        changed_guard.as_str(),
        no_space.as_str(),
        "echo sift",
        "command echo sift",
        "command -v sift",
        "command -V sift",
        "false && command sift run -- cargo build",
        "echo sift; touch should-not-exist",
        "command true || cargo build; echo 'command sift run --capture -- cargo build'",
        "command true || cargo build; command '/synthetic/sift' run --capture -- cargo build | cat",
        "command true || cargo build; command '/synthetic/sift' run --capture -- cargo build > output",
        "command true || cargo build; command '/synthetic/sift' run --capture -- $(touch should-not-exist)",
        "command true || cargo build;é", // No generated delimiter; must not index mid-codepoint.
        "command '/synthetic/sift-other' run -- cargo build",
    ];
    let mut records = Vec::new();
    for (n, command) in cases.iter().enumerate() {
        let id = n.to_string();
        records.push(codex_call(&id, json!(command)));
        records.push(codex_result(&id, json!(repeat())));
    }
    s.write("session.jsonl", &records);
    let report = s.report(&["--history", "session.jsonl", "--json"]);
    assert_eq!(report["sift_calls"], 0);
    assert_eq!(report["missed_opportunities"], cases.len());
    assert!(!s.0.join("should-not-exist").exists());
    assert!(!s.0.join("output").exists());
    assert!(!report.to_string().contains("touch"));
}

#[test]
fn ctx_sift_commands_are_distinct_from_history_and_graph() {
    let sandbox = Sandbox::new();
    let output_commands = [
        "ctx run -- git status",
        "ctx compact file.txt",
        "ctx restore --encoding raw file.bin",
        "ctx recall abc-123",
        "ctx output run --capture -- git status",
        "command /opt/ctx.exe output proxy -- git status",
        "ctx output semantic status",
    ];
    let unrelated = [
        "ctx graph status",
        "ctx search topic",
        "ctx blame file src.rs",
        "ctx output graph run --raw",
        "echo ctx output run",
    ];
    let mut records = Vec::new();
    for (i, command) in output_commands.iter().chain(unrelated.iter()).enumerate() {
        let id = i.to_string();
        records.push(codex_call(&id, json!(command)));
        records.push(codex_result(&id, json!(repeat())));
    }
    sandbox.write("session.jsonl", &records);
    let report = sandbox.report(&["--history", "session.jsonl", "--json"]);
    assert_eq!(report["sift_calls"], output_commands.len());
}
