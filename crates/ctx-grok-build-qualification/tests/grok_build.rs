use assert_cmd::cargo::CommandCargoExt as _;
use serde_json::{json, Value};
use std::{
    ffi::OsStr,
    fs::{self, File, Metadata, OpenOptions},
    io::{BufRead as _, BufReader, Write as _},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime},
};
use tempfile::TempDir;

const FIXTURE: &str = "grok-build/v1.0.3/sessions/synthetic-workspace/01990000-0000-7000-8000-000000000001/updates.jsonl";
const FIXTURE_SESSION_ID: &str = "01990000-0000-7000-8000-000000000001";
const EXPLICIT_ORACLE: &str = "pocket-calculator-fixture";

struct Harness {
    root: TempDir,
    home: PathBuf,
    data_root: PathBuf,
    grok_home: Option<PathBuf>,
    ctx_binary: PathBuf,
}

impl Harness {
    fn new(grok_home: Option<PathBuf>) -> Self {
        let root = tempfile::Builder::new()
            .prefix("ctx-grok-build-qualification-")
            .tempdir()
            .expect("create isolated qualification root");
        let home = root.path().join("home");
        let data_root = root.path().join("ctx-data");
        for path in [
            &home,
            &data_root,
            &root.path().join("xdg-config"),
            &root.path().join("xdg-data"),
            &root.path().join("xdg-state"),
            &root.path().join("runtime"),
        ] {
            fs::create_dir_all(path).expect("create isolated command root");
        }
        let built_ctx_binary = PathBuf::from(
            Command::cargo_bin("ctx")
                .expect("resolve the real ctx binary")
                .get_program(),
        );
        let ctx_binary = root.path().join(if cfg!(windows) {
            "ctx-qualification.exe"
        } else {
            "ctx-qualification"
        });
        fs::copy(&built_ctx_binary, &ctx_binary).unwrap_or_else(|error| {
            panic!(
                "copy real ctx binary {} to {}: {error}",
                built_ctx_binary.display(),
                ctx_binary.display()
            )
        });
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            fs::set_permissions(&ctx_binary, fs::Permissions::from_mode(0o700))
                .expect("make copied ctx binary executable");
        }
        Self {
            root,
            home,
            data_root,
            grok_home,
            ctx_binary,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.ctx_binary);
        command
            .env("CTX_DATA_ROOT", &self.data_root)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("XDG_CONFIG_HOME", self.root.path().join("xdg-config"))
            .env("XDG_DATA_HOME", self.root.path().join("xdg-data"))
            .env("XDG_STATE_HOME", self.root.path().join("xdg-state"))
            .env("CTX_RUNTIME_DIR", self.root.path().join("runtime"))
            .env("CTX_ANALYTICS_ENABLED", "false")
            .env("CTX_LOCAL_USAGE_ENABLED", "false")
            .env("CTX_DAEMON_AUTOSTART_OFF", "1")
            .env("CTX_UPGRADE_OFF", "1")
            .env_remove("CI")
            .env_remove("GITHUB_ACTIONS")
            .env_remove("BUILDKITE")
            .env_remove("BUILDKITE_BUILD_ID");
        for name in [
            "AGENT",
            "AGENT_SESSION_ID",
            "AI_AGENT",
            "ASTRBOT_ROOT",
            "CLAUDE_CONFIG_DIR",
            "CLAUDE_CODE_SESSION_ID",
            "CODEX_THREAD_ID",
            "CODEX_HOME",
            "COPILOT_HOME",
            "DSH_SESSION_ID",
            "DSH_SHELL",
            "FORGE_CONFIG",
            "GOOSE_TERMINAL",
            "GROK_SESSION_ID",
            "HERMES_AGENT",
            "HERMES_HOME",
            "HERMES_SESSION_ID",
            "KILO_DB",
            "MIMOCODE_CONFIG_DIR",
            "MIMOCODE_DB",
            "MIMOCODE_HOME",
            "MUX_RUNTIME",
            "MUX_WORKSPACE_ID",
            "OPENCLAW_STATE_DIR",
            "PI_CODING_AGENT",
            "PI_SESSION_ID",
            "QWEN_CODE",
            "QWEN_CODE_SESSION_ID",
            "SHELLEY_CONVERSATION_ID",
            "VIBE_HOME",
        ] {
            command.env_remove(name);
        }
        if let Some(grok_home) = &self.grok_home {
            command.env("GROK_HOME", grok_home);
        } else {
            command.env_remove("GROK_HOME");
        }
        command
    }

    fn json<I, S>(&self, args: I) -> Value
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.json_with_env(args, &[])
    }

    fn json_with_env<I, S>(&self, args: I, environment: &[(&str, &str)]) -> Value
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = self.command();
        for (name, value) in environment {
            command.env(name, value);
        }
        let output = command
            .args(args)
            .output()
            .expect("run isolated ctx command");
        assert!(
            output.status.success(),
            "ctx command failed with {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "ctx returned invalid JSON: {error}\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        })
    }

    fn explicit_import(&self, path: &Path, resume: bool) -> Value {
        let mut args = vec![
            "import".to_owned(),
            "--provider".to_owned(),
            "grok-build".to_owned(),
            "--path".to_owned(),
            path.display().to_string(),
            "--no-daemon".to_owned(),
            "--format=json".to_owned(),
            "--progress".to_owned(),
            "none".to_owned(),
        ];
        if resume {
            args.push("--resume".to_owned());
        }
        self.json(args)
    }

    fn start_daemon(&self) -> DaemonGuard {
        let mut command = self.command();
        let child = command
            .args(["daemon", "run", "--force", "--loop-interval-seconds", "600"])
            .env("CTX_DAEMON_MODE", "full")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start isolated ctx daemon");
        let mut daemon = DaemonGuard { child: Some(child) };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = daemon
                .child
                .as_mut()
                .expect("daemon child")
                .try_wait()
                .expect("inspect daemon child")
            {
                panic!("isolated ctx daemon exited before readiness: {status}");
            }
            let status = self.json(["daemon", "status", "--format=json"]);
            if status["daemon"]["running"] == true
                && status["daemon"]["core_refresh_endpoint"]["available"] == true
            {
                return daemon;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for isolated ctx daemon readiness: {status:#}"
            );
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn wait_for_generation(&self, report: &Value) {
        let generation = report["sources"][0]["published_generation"]
            .as_str()
            .unwrap_or_else(|| panic!("import omitted published generation: {report:#}"));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let status = self.json(["status", "--format=json"]);
            if status["lexical"]["status"] == "ready"
                && status["lexical"]["generation_id"] == generation
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for lexical generation {generation}: {status:#}"
            );
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn search(&self, query: &str) -> Value {
        self.json([
            "search",
            query,
            "--provider",
            "grok-build",
            "--events",
            "--refresh",
            "off",
            "--format=json",
        ])
    }

    fn search_as_active_session(&self, query: &str, include_current_session: bool) -> Value {
        let mut args = vec![
            "search",
            query,
            "--provider",
            "grok-build",
            "--events",
            "--refresh",
            "off",
            "--format=json",
        ];
        if include_current_session {
            args.push("--include-current-session");
        }
        self.json_with_env(args, &[("GROK_SESSION_ID", FIXTURE_SESSION_ID)])
    }
}

struct DaemonGuard {
    child: Option<Child>,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        match child.try_wait() {
            Ok(Some(_)) => {}
            Ok(None) => {
                child.kill().expect("terminate isolated ctx daemon");
                child.wait().expect("reap isolated ctx daemon");
            }
            Err(error) => panic!("inspect isolated ctx daemon during teardown: {error}"),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct FileSnapshot {
    bytes: Vec<u8>,
    len: u64,
    modified: SystemTime,
    identity: Option<(u64, u64)>,
}

impl FileSnapshot {
    fn capture(path: &Path) -> Self {
        let metadata = fs::metadata(path).expect("inspect provider source");
        Self {
            bytes: fs::read(path).expect("read provider source"),
            len: metadata.len(),
            modified: metadata.modified().expect("provider source mtime"),
            identity: file_identity(&metadata),
        }
    }

    fn assert_unchanged(&self, path: &Path) {
        assert_eq!(
            &Self::capture(path),
            self,
            "ctx modified provider source {}",
            path.display()
        );
    }
}

#[cfg(unix)]
fn file_identity(metadata: &Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;

    Some((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn file_identity(_metadata: &Metadata) -> Option<(u64, u64)> {
    None
}

fn public_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/provider-history")
        .join(FIXTURE)
}

fn copy_public_fixture(destination: &Path) {
    fs::create_dir_all(destination.parent().expect("fixture parent"))
        .expect("create fixture destination");
    fs::copy(public_fixture(), destination).expect("copy public sanitized Grok fixture");
}

fn clone_public_fixture_session(destination: &Path, native_session_id: &str, oracle: &str) {
    fs::create_dir_all(destination.parent().expect("session fixture parent"))
        .expect("create session fixture destination");
    let source = File::open(public_fixture()).expect("open public sanitized Grok fixture");
    let mut output = File::create(destination).expect("create cloned Grok fixture");
    let mut replaced = false;
    for (ordinal, line) in BufReader::new(source).lines().enumerate() {
        let mut record: Value = serde_json::from_str(&line.expect("read fixture record"))
            .expect("parse fixture record");
        record["params"]["sessionId"] = json!(native_session_id);
        record["params"]["_meta"]["eventId"] =
            json!(format!("{native_session_id}-{}", ordinal + 1));
        if record
            .pointer("/params/update/sessionUpdate")
            .and_then(Value::as_str)
            == Some("agent_message_chunk")
        {
            record["params"]["update"]["content"] = json!({"type": "text", "text": oracle});
            replaced = true;
        }
        serde_json::to_writer(&mut output, &record).expect("write cloned fixture record");
        output
            .write_all(b"\n")
            .expect("terminate cloned fixture record");
    }
    assert!(replaced, "public Grok fixture has no assistant message");
}

fn append_jsonl(path: &Path, record: &Value) {
    let mut output = OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open Grok source for test-authored append");
    serde_json::to_writer(&mut output, record).expect("append Grok JSONL record");
    output
        .write_all(b"\n")
        .expect("terminate Grok JSONL record");
}

fn append_agent_message(path: &Path, session_id: Option<&str>, event_suffix: &str, text: &str) {
    let mut params = json!({
        "update": {
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": text}
        },
        "_meta": {
            "eventId": format!("{FIXTURE_SESSION_ID}-{event_suffix}"),
            "agentTimestampMs": 1_786_547_762_000_i64
        }
    });
    if let Some(session_id) = session_id {
        params["sessionId"] = json!(session_id);
    }
    append_jsonl(
        path,
        &json!({
            "timestamp": 1_786_547_762_i64,
            "method": "session/update",
            "params": params
        }),
    );
}

fn append_malformed_line(path: &Path) {
    let mut output = OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open Grok source for malformed test line");
    output
        .write_all(b"{\"intentionally_malformed\":\n")
        .expect("append malformed Grok line");
}

fn append_future_typed_completion(path: &Path, marker: &str) {
    append_jsonl(
        path,
        &json!({
            "timestamp": 1_786_547_762_i64,
            "method": "session/update",
            "params": {
                "sessionId": FIXTURE_SESSION_ID,
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "future-typed-content",
                    "status": "completed",
                    "content": [{
                        "type": "content",
                        "content": {
                            "type": "image_resource_vNext",
                            "resource": {"uri": marker, "mimeType": "image/png"}
                        }
                    }],
                    "rawOutput": {
                        "type": "FutureResourceVNext",
                        "resource": {"uri": marker}
                    }
                },
                "_meta": {
                    "eventId": format!("{FIXTURE_SESSION_ID}-future-typed-content"),
                    "agentTimestampMs": 1_786_547_762_000_i64
                }
            }
        }),
    );
}

fn source_receipt<'a>(report: &'a Value, source_format: &str, rejected: u64) -> &'a Value {
    assert_eq!(report["schema_version"], 2, "{report:#}");
    assert_eq!(
        report["outcome"],
        if rejected == 0 {
            "success"
        } else {
            "completed_with_rejections"
        },
        "{report:#}"
    );
    assert_eq!(
        report["totals"]["current_rejected_records"], rejected,
        "{report:#}"
    );
    let sources = report["sources"]
        .as_array()
        .unwrap_or_else(|| panic!("missing source receipts in {report:#}"));
    assert_eq!(sources.len(), 1, "{report:#}");
    let source = &sources[0];
    assert_eq!(source["provider"], "grok_build", "{report:#}");
    assert_eq!(source["source_format"], source_format, "{report:#}");
    assert_eq!(source["current_rejected_records"], rejected, "{report:#}");
    assert!(source["published_generation"].is_string(), "{report:#}");
    source
}

fn generation(source: &Value) -> &str {
    source["published_generation"]
        .as_str()
        .expect("published generation")
}

fn assert_noop(source: &Value, expected_generation: &str) {
    assert_eq!(source["change"], "no_op", "{source:#}");
    assert_eq!(source["generation_changed"], false, "{source:#}");
    assert_eq!(generation(source), expected_generation, "{source:#}");
}

fn one_matching_result<'a>(search: &'a Value, query: &str) -> &'a Value {
    assert_eq!(search["schema_version"], 2, "{search:#}");
    assert_eq!(search["filters"]["provider"], "grok_build", "{search:#}");
    let matches = search["results"]
        .as_array()
        .expect("search results")
        .iter()
        .filter(|result| {
            result["snippet"]
                .as_str()
                .is_some_and(|snippet| snippet.contains(query))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        matches.len(),
        1,
        "unexpected search result for {query}: {search:#}"
    );
    assert_eq!(matches[0]["provider"], "grok_build", "{search:#}");
    matches[0]
}

fn assert_not_searchable(harness: &Harness, marker: &str) {
    let search = harness.search(marker);
    assert!(
        search["results"]
            .as_array()
            .expect("search results")
            .is_empty(),
        "rejected or future-typed marker became searchable: {search:#}"
    );
}

fn show_event(harness: &Harness, result: &Value) -> Value {
    let event_id = result["ctx_event_id"]
        .as_str()
        .expect("search result event ID");
    harness.json(["show", "event", event_id, "--window", "1", "--format=json"])
}

fn show_session(harness: &Harness, result: &Value) -> Value {
    let session_id = result["ctx_session_id"]
        .as_str()
        .expect("search result session ID");
    harness.json(["show", "session", session_id, "--format=json"])
}

#[test]
fn explicit_fixture_import_search_show_noop_append_resume_is_source_read_only() {
    let harness = Harness::new(None);
    let source = harness.root.path().join("explicit/session/updates.jsonl");
    copy_public_fixture(&source);
    let initial_state = FileSnapshot::capture(&source);
    let _daemon = harness.start_daemon();

    let first = harness.explicit_import(&source, false);
    harness.wait_for_generation(&first);
    let first_source = source_receipt(&first, "grok_build_session_updates_jsonl", 0);
    let first_generation = generation(first_source).to_owned();
    let search = harness.search(EXPLICIT_ORACLE);
    let result = one_matching_result(&search, EXPLICIT_ORACLE);
    let excluded = harness.search_as_active_session(EXPLICIT_ORACLE, false);
    assert!(
        excluded["results"].as_array().unwrap().is_empty(),
        "{excluded:#}"
    );
    one_matching_result(
        &harness.search_as_active_session(EXPLICIT_ORACLE, true),
        EXPLICIT_ORACLE,
    );
    let shown = show_event(&harness, result);
    assert_eq!(shown["payload_type"], "event_window", "{shown:#}");
    assert_eq!(shown["event"]["provider"], "grok_build", "{shown:#}");
    assert!(
        shown["event"]["text"]
            .as_str()
            .is_some_and(|text| text.contains(EXPLICIT_ORACLE)),
        "{shown:#}"
    );

    let replay = harness.explicit_import(&source, false);
    let replay_source = source_receipt(&replay, "grok_build_session_updates_jsonl", 0);
    assert_noop(replay_source, &first_generation);
    initial_state.assert_unchanged(&source);

    let appended_oracle = "grokappendresumeoracle7bd3";
    append_agent_message(&source, Some(FIXTURE_SESSION_ID), "append", appended_oracle);
    let appended_state = FileSnapshot::capture(&source);
    let resumed = harness.explicit_import(&source, true);
    harness.wait_for_generation(&resumed);
    let resumed_source = source_receipt(&resumed, "grok_build_session_updates_jsonl", 0);
    assert_eq!(resumed_source["change"], "changed", "{resumed:#}");
    assert_eq!(resumed_source["generation_changed"], true, "{resumed:#}");
    let resumed_generation = generation(resumed_source).to_owned();
    assert_ne!(resumed_generation, first_generation, "{resumed:#}");
    one_matching_result(&harness.search(appended_oracle), appended_oracle);

    let resumed_replay = harness.explicit_import(&source, true);
    let resumed_replay_source =
        source_receipt(&resumed_replay, "grok_build_session_updates_jsonl", 0);
    assert_noop(resumed_replay_source, &resumed_generation);
    appended_state.assert_unchanged(&source);
}

#[test]
fn automatic_default_and_grok_home_discovery_keep_two_sessions_independent_and_read_only() {
    for (label, use_override) in [("default", false), ("grokhome", true)] {
        let root = tempfile::Builder::new()
            .prefix("ctx-grok-home-selection-")
            .tempdir()
            .expect("create Grok home selector");
        let selected_home = root.path().join("selected-grok-home");
        let harness = Harness::new(use_override.then(|| selected_home.clone()));
        let sessions = if use_override {
            selected_home.join("sessions")
        } else {
            harness.home.join(".grok/sessions")
        };
        let first_oracle = format!("grok{label}firstsessionoracle91c2");
        let second_oracle = format!("grok{label}secondsessionoracle91c2");
        let first = sessions.join("workspace-one/session-one/updates.jsonl");
        let second = sessions.join("workspace-two/session-two/updates.jsonl");
        clone_public_fixture_session(
            &first,
            "01990000-0000-7000-8000-000000000011",
            &first_oracle,
        );
        clone_public_fixture_session(
            &second,
            "01990000-0000-7000-8000-000000000022",
            &second_oracle,
        );
        let first_state = FileSnapshot::capture(&first);
        let second_state = FileSnapshot::capture(&second);
        let _daemon = harness.start_daemon();

        let discovered = harness.json(["sources", "--format=json", "--all"]);
        let source = discovered["sources"]
            .as_array()
            .expect("discovered sources")
            .iter()
            .find(|source| source["provider"] == "grok_build")
            .unwrap_or_else(|| panic!("Grok source was not discovered: {discovered:#}"));
        assert_eq!(source["status"], "available", "{discovered:#}");
        assert_eq!(
            source["source_format"], "grok_build_session_updates_jsonl_tree",
            "{discovered:#}"
        );

        let imported = harness.json([
            "import",
            "--provider",
            "grok-build",
            "--no-daemon",
            "--format=json",
            "--progress",
            "none",
        ]);
        harness.wait_for_generation(&imported);
        assert_eq!(imported["schema_version"], 2, "{imported:#}");
        assert_eq!(imported["outcome"], "success", "{imported:#}");
        assert_eq!(
            imported["totals"]["current_source_count"], 2,
            "{imported:#}"
        );
        assert_eq!(
            imported["totals"]["current_rejected_records"], 0,
            "{imported:#}"
        );

        let first_search = harness.search(&first_oracle);
        let first_result = one_matching_result(&first_search, &first_oracle);
        let second_search = harness.search(&second_oracle);
        let second_result = one_matching_result(&second_search, &second_oracle);
        assert_ne!(
            first_result["ctx_session_id"], second_result["ctx_session_id"],
            "{label} discovery collapsed independent ctx sessions"
        );
        assert_ne!(
            first_result["provider_session_id"], second_result["provider_session_id"],
            "{label} discovery collapsed independent provider sessions"
        );
        assert!(!show_session(&harness, first_result)["events"]
            .as_array()
            .expect("first session events")
            .is_empty());
        assert!(!show_session(&harness, second_result)["events"]
            .as_array()
            .expect("second session events")
            .is_empty());
        first_state.assert_unchanged(&first);
        second_state.assert_unchanged(&second);
    }
}

#[test]
fn malformed_and_missing_session_records_are_rejected_future_content_is_hidden_and_resume_is_stable(
) {
    let harness = Harness::new(None);
    let source = harness.root.path().join("mixed/session/updates.jsonl");
    copy_public_fixture(&source);
    let _daemon = harness.start_daemon();

    let baseline = harness.explicit_import(&source, false);
    harness.wait_for_generation(&baseline);
    let baseline_source = source_receipt(&baseline, "grok_build_session_updates_jsonl", 0);
    let baseline_documents = baseline_source["current_indexed_documents"]
        .as_u64()
        .expect("baseline indexed documents");

    let missing_marker = "grokmissingidentitymarker8f31";
    let blank_marker = "grokblankidentitymarker8f31";
    let future_marker = "grokfuturetypedimagemarker8f31";
    let later_marker = "groklatervalidmarker8f31";
    append_malformed_line(&source);
    append_agent_message(&source, None, "missing-session", missing_marker);
    append_agent_message(&source, Some("   "), "blank-session", blank_marker);
    append_future_typed_completion(&source, future_marker);
    append_agent_message(
        &source,
        Some(FIXTURE_SESSION_ID),
        "later-valid",
        later_marker,
    );
    let authored_state = FileSnapshot::capture(&source);

    let resumed = harness.explicit_import(&source, true);
    harness.wait_for_generation(&resumed);
    let resumed_source = source_receipt(&resumed, "grok_build_session_updates_jsonl", 3);
    assert_eq!(
        resumed_source["current_sources_with_rejections"], 1,
        "{resumed:#}"
    );
    assert_eq!(resumed_source["change"], "changed", "{resumed:#}");
    assert_eq!(resumed_source["generation_changed"], true, "{resumed:#}");
    let resumed_generation = generation(resumed_source).to_owned();
    let resumed_documents = resumed_source["current_indexed_documents"]
        .as_u64()
        .expect("resumed indexed documents");
    assert!(
        (baseline_documents + 1..=baseline_documents + 2).contains(&resumed_documents),
        "the later valid message must import, while a contentless terminal completion may be retained: {resumed:#}"
    );

    assert_not_searchable(&harness, missing_marker);
    assert_not_searchable(&harness, blank_marker);
    assert_not_searchable(&harness, future_marker);
    let later_search = harness.search(later_marker);
    let later_result = one_matching_result(&later_search, later_marker);
    let before_replay_session = show_session(&harness, later_result);
    let before_replay_events = before_replay_session["events"]
        .as_array()
        .expect("session events")
        .len();

    let replay = harness.explicit_import(&source, true);
    let replay_source = source_receipt(&replay, "grok_build_session_updates_jsonl", 3);
    assert_noop(replay_source, &resumed_generation);
    assert_eq!(
        replay_source["current_indexed_documents"], resumed_documents,
        "{replay:#}"
    );
    let replay_search = harness.search(later_marker);
    let replay_result = one_matching_result(&replay_search, later_marker);
    assert_eq!(
        show_session(&harness, replay_result)["events"]
            .as_array()
            .expect("replayed session events")
            .len(),
        before_replay_events,
        "contentless terminal completion changed across no-op replay"
    );
    authored_state.assert_unchanged(&source);
}
