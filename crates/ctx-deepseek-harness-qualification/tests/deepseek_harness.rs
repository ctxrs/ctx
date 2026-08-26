use assert_cmd::cargo::CommandCargoExt as _;
use ctx_history_index_query::VerifiedIndex;
use serde_json::{json, Value};
use std::{
    ffi::OsStr,
    fs::{self, Metadata, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime},
};
use tempfile::TempDir;
use uuid::Uuid;

const PARENT_SESSION_ID: &str = "11111111-2222-4333-8444-555555555555";
const CHILD_SESSION_ID: &str = "66666666-7777-4888-8999-aaaaaaaaaaaa";
const PROJECT_KEY: &str = "--workspace-deepseek-harness-fixture--";
const PARENT_ORACLE: &str = "deepseekharnessparentoracle7f31";
const CHILD_ORACLE: &str = "deepseekharnesschildoracle2c57";
const TREE_FORMAT: &str = "deepseek_harness_session_jsonl_tree";
const FILE_FORMAT: &str = "deepseek_harness_session_jsonl";

#[derive(Clone, Copy, Debug)]
enum Encoding {
    Raw,
    Zstd,
}

impl Encoding {
    fn fixture_root(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Zstd => "zstd",
        }
    }

    fn filename(self) -> &'static str {
        match self {
            Self::Raw => "session.jsonl",
            Self::Zstd => "session.jsonl.zstd",
        }
    }
}

struct Harness {
    root: TempDir,
    home: PathBuf,
    data_root: PathBuf,
    dsh_home: Option<PathBuf>,
    ctx_binary: PathBuf,
}

impl Harness {
    fn new(use_dsh_home_override: bool) -> Self {
        let root = tempfile::Builder::new()
            .prefix("ctx-deepseek-harness-qualification-")
            .tempdir()
            .expect("create isolated qualification root");
        let home = root.path().join("home");
        let data_root = root.path().join("ctx-data");
        let dsh_home = use_dsh_home_override.then(|| root.path().join("selected-dsh-home"));
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
            dsh_home,
            ctx_binary,
        }
    }

    fn active_dsh_home(&self) -> PathBuf {
        self.dsh_home
            .clone()
            .unwrap_or_else(|| self.home.join(".dsh"))
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
            "ASTRBOT_ROOT",
            "CLAUDE_CONFIG_DIR",
            "CODEX_HOME",
            "COPILOT_HOME",
            "FORGE_CONFIG",
            "GROK_HOME",
            "HERMES_HOME",
            "KILO_DB",
            "MIMOCODE_CONFIG_DIR",
            "MIMOCODE_DB",
            "MIMOCODE_HOME",
            "OPENCLAW_STATE_DIR",
            "VIBE_HOME",
        ] {
            command.env_remove(name);
        }
        if let Some(dsh_home) = &self.dsh_home {
            command.env("DSH_HOME", dsh_home);
        } else {
            command.env_remove("DSH_HOME");
        }
        command
    }

    fn json<I, S>(&self, args: I) -> Value
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self
            .command()
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

    fn failure<I, S>(&self, args: I) -> String
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self
            .command()
            .args(args)
            .output()
            .expect("run isolated failing ctx command");
        assert!(
            !output.status.success(),
            "ctx command unexpectedly succeeded"
        );
        format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    }

    fn failed_explicit_import(&self, path: &Path) -> String {
        self.failure(vec![
            "import".to_owned(),
            "--provider".to_owned(),
            "deepseek-harness".to_owned(),
            "--path".to_owned(),
            path.display().to_string(),
            "--no-daemon".to_owned(),
            "--format=json".to_owned(),
            "--progress".to_owned(),
            "none".to_owned(),
        ])
    }

    fn explicit_import(&self, path: &Path, resume: bool) -> Value {
        let mut args = vec![
            "import".to_owned(),
            "--provider".to_owned(),
            "deepseek-harness".to_owned(),
            "--path".to_owned(),
            path.display().to_string(),
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
        let generation = report["sources"]
            .as_array()
            .expect("source receipts")
            .iter()
            .find_map(|source| source["published_generation"].as_str())
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
            "deepseek-harness",
            "--refresh",
            "off",
            "--format=json",
        ])
    }
}

struct DaemonGuard {
    child: Option<Child>,
}

impl DaemonGuard {
    fn stop(mut self) {
        if let Some(mut child) = self.child.take() {
            child.kill().expect("terminate isolated ctx daemon");
            child.wait().expect("reap isolated ctx daemon");
        }
    }
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

#[derive(Debug, PartialEq, Eq)]
struct DirectorySnapshot(Vec<DirectoryEntrySnapshot>);

type DirectoryEntrySnapshot = (Vec<u8>, bool, u64, Option<(u64, u64)>);

impl DirectorySnapshot {
    fn capture(path: &Path) -> Self {
        let mut entries = fs::read_dir(path)
            .expect("inspect provider directory")
            .map(|entry| {
                let entry = entry.expect("inspect provider directory entry");
                let metadata = fs::symlink_metadata(entry.path()).expect("inspect entry metadata");
                let name = entry.file_name().as_encoded_bytes().to_vec();
                (
                    name,
                    metadata.is_file(),
                    metadata.len(),
                    file_identity(&metadata),
                )
            })
            .collect::<Vec<_>>();
        entries.sort();
        Self(entries)
    }

    fn assert_unchanged(&self, path: &Path) {
        assert_eq!(&Self::capture(path), self, "ctx modified directory entries");
    }
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

fn public_fixture_home(encoding: Encoding) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/provider-history/deepseek-harness/v0")
        .join(encoding.fixture_root())
}

fn public_session(encoding: Encoding, session_id: &str) -> PathBuf {
    public_fixture_home(encoding)
        .join("sessions")
        .join(PROJECT_KEY)
        .join(session_id)
        .join(encoding.filename())
}

fn copy_public_session(encoding: Encoding, session_id: &str, destination: &Path) {
    fs::create_dir_all(destination.parent().expect("fixture parent"))
        .expect("create fixture destination");
    fs::copy(public_session(encoding, session_id), destination)
        .expect("copy public DeepSeek Harness fixture");
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("create copied fixture root");
    for entry in fs::read_dir(source).expect("read public fixture tree") {
        let entry = entry.expect("read public fixture entry");
        let target = destination.join(entry.file_name());
        if entry.file_type().expect("fixture entry type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("copy public fixture entry");
        }
    }
}

fn append_bytes(path: &Path, bytes: &[u8]) {
    OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open test-authored source append")
        .write_all(bytes)
        .expect("append test-authored source bytes");
}

fn append_record(path: &Path, record: &Value) {
    let mut bytes = serde_json::to_vec(record).expect("serialize test-authored record");
    bytes.push(b'\n');
    append_bytes(path, &bytes);
}

fn appended_round(start_seq: u64, oracle: &str) -> Vec<Value> {
    vec![
        json!({"type":"turn/start","seq":start_seq,"time":1786580001000_i64,"data":{"turn":2}}),
        json!({"type":"step/start","seq":start_seq + 1,"time":1786580002000_i64,"data":{"turn":2,"step":1}}),
        json!({
            "type":"user/message","seq":start_seq + 2,"time":1786580003000_i64,
            "data":{"content":[{"type":"text","text":"Record one appended qualification turn."}],"source":{"kind":"user"},"role":"user","id":"70000000-0000-4000-8000-000000000001"},
            "surfaceOp":"append"
        }),
        json!({
            "type":"assistant/message","seq":start_seq + 3,"time":1786580004000_i64,
            "data":{"turn":2,"step":1,"message":{"role":"assistant","content":[{"type":"text","text":oracle}],"source":{"kind":"model","provider":"fixture-provider","model":"fixture-model"},"id":"80000000-0000-4000-8000-000000000001"}},
            "surfaceOp":"append"
        }),
        json!({"type":"step/end","seq":start_seq + 4,"time":1786580005000_i64,"data":{"turn":2,"step":1}}),
        json!({"type":"turn/end","seq":start_seq + 5,"time":1786580006000_i64,"data":{"turn":2,"reason":{"kind":"completed"}}}),
    ]
}

fn encode_records(records: &[Value]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, record).expect("serialize appended native record");
        bytes.push(b'\n');
    }
    bytes
}

fn append_round(path: &Path, encoding: Encoding, start_seq: u64, oracle: &str) {
    let plaintext = encode_records(&appended_round(start_seq, oracle));
    match encoding {
        Encoding::Raw => append_bytes(path, &plaintext),
        Encoding::Zstd => {
            let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 19)
                .expect("create deterministic Zstandard append encoder");
            encoder
                .include_checksum(true)
                .expect("enable Zstandard frame checksum");
            encoder
                .write_all(&plaintext)
                .expect("compress appended native records");
            let frame = encoder.finish().expect("finish appended Zstandard frame");
            append_bytes(path, &frame);
        }
    }
}

fn rewrite_header_as_future(path: &Path, marker: &str) {
    let content = fs::read_to_string(path).expect("read raw fixture for future header");
    let header_end = content.find('\n').expect("fixture header newline");
    let mut header: Value =
        serde_json::from_str(&content[..header_end]).expect("parse fixture header");
    header["version"] = json!(1);
    header["futureOnly"] = json!(marker);
    let rewritten = format!(
        "{}\n{}",
        serde_json::to_string(&header).expect("serialize future header"),
        &content[header_end + 1..]
    );
    fs::write(path, rewritten).expect("write test-authored future header");
}

fn rewrite_header_with_unknown_v0_field(path: &Path, marker: &str) {
    let content = fs::read_to_string(path).expect("read raw fixture for unknown header field");
    let header_end = content.find('\n').expect("fixture header newline");
    let mut header: Value =
        serde_json::from_str(&content[..header_end]).expect("parse fixture header");
    header["unknownV0Field"] = json!(marker);
    let rewritten = format!(
        "{}\n{}",
        serde_json::to_string(&header).expect("serialize unknown header field"),
        &content[header_end + 1..]
    );
    fs::write(path, rewritten).expect("write test-authored unknown header field");
}

fn only_source<'a>(report: &'a Value, expected_format: &str) -> &'a Value {
    assert_eq!(report["schema_version"], 2, "{report:#}");
    let sources = report["sources"]
        .as_array()
        .unwrap_or_else(|| panic!("missing source receipts in {report:#}"));
    assert_eq!(sources.len(), 1, "{report:#}");
    let source = &sources[0];
    assert_eq!(source["provider"], "deepseek_harness", "{report:#}");
    assert_eq!(source["source_format"], expected_format, "{report:#}");
    source
}

fn successful_source<'a>(report: &'a Value, expected_format: &str) -> &'a Value {
    assert_eq!(report["outcome"], "success", "{report:#}");
    assert_eq!(
        report["totals"]["current_rejected_records"], 0,
        "{report:#}"
    );
    let source = only_source(report, expected_format);
    assert_eq!(source["current_rejected_records"], 0, "{report:#}");
    assert!(source["published_generation"].is_string(), "{report:#}");
    source
}

fn assert_counts(source: &Value, complete: u64, retained: u64, rejected: u64, ignored: u64) {
    assert_eq!(source["current_complete_records"], complete, "{source:#}");
    assert_eq!(source["current_retained_records"], retained, "{source:#}");
    assert_eq!(source["current_rejected_records"], rejected, "{source:#}");
    assert_eq!(source["current_ignored_records"], ignored, "{source:#}");
    assert_eq!(source["current_indexed_documents"], retained, "{source:#}");
}

fn rejected_source(report: &Value, minimum: u64) -> &Value {
    assert_eq!(report["outcome"], "completed_with_rejections", "{report:#}");
    let source = only_source(report, FILE_FORMAT);
    let rejected = source["current_rejected_records"]
        .as_u64()
        .expect("rejected record count");
    assert!(rejected >= minimum, "{report:#}");
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
    assert_eq!(
        search["filters"]["provider"], "deepseek_harness",
        "{search:#}"
    );
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
    let result = matches[0];
    assert_eq!(result["provider"], "deepseek_harness", "{search:#}");
    result
}

fn assert_citation(result: &Value) {
    let citations = result["citations"].as_array().expect("result citations");
    assert_eq!(citations.len(), 1, "{result:#}");
    let citation = &citations[0];
    assert_eq!(citation["item_id"], result["ctx_event_id"], "{result:#}");
    assert_eq!(citation["target_type"], "event", "{result:#}");
    assert_eq!(
        citation["ctx_event_id"], result["ctx_event_id"],
        "{result:#}"
    );
    assert_eq!(
        citation["ctx_session_id"], result["ctx_session_id"],
        "{result:#}"
    );
    assert_eq!(citation["provider"], "deepseek_harness", "{result:#}");
    assert!(citation.get("source_path").is_none(), "{result:#}");
}

fn assert_not_searchable(harness: &Harness, marker: &str) {
    let search = harness.search(marker);
    assert!(
        search["results"]
            .as_array()
            .expect("search results")
            .is_empty(),
        "rejected or ignored marker became searchable: {search:#}"
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
fn explicit_raw_and_zstd_import_search_show_citation_noop_append_resume_are_read_only() {
    for encoding in [Encoding::Raw, Encoding::Zstd] {
        let harness = Harness::new(false);
        let source = harness
            .root
            .path()
            .join("explicit")
            .join(encoding.filename());
        copy_public_session(encoding, PARENT_SESSION_ID, &source);
        let initial_state = FileSnapshot::capture(&source);
        let parent = source.parent().expect("source parent");
        let initial_directory = DirectorySnapshot::capture(parent);
        let daemon = harness.start_daemon();

        let first = harness.explicit_import(&source, false);
        harness.wait_for_generation(&first);
        let first_source = successful_source(&first, FILE_FORMAT);
        assert_counts(first_source, 19, 8, 0, 11);
        let first_generation = generation(first_source).to_owned();
        let success_search = harness.search("deepseekharnesseditoracle9d42");
        let success_result = one_matching_result(&success_search, "deepseekharnesseditoracle9d42");
        assert_eq!(success_result["title"], "deepseek_harness tool tool_output");
        let failure_search = harness.search("deepseekharnessfailureoracle4b68");
        let failure_result =
            one_matching_result(&failure_search, "deepseekharnessfailureoracle4b68");
        assert_eq!(failure_result["title"], "deepseek_harness tool tool_output");
        let search = harness.search(PARENT_ORACLE);
        let result = one_matching_result(&search, PARENT_ORACLE);
        assert_eq!(
            result["provider_session_id"], PARENT_SESSION_ID,
            "{result:#}"
        );
        assert_citation(result);
        let shown = show_event(&harness, result);
        assert_eq!(shown["payload_type"], "event_window", "{shown:#}");
        assert_eq!(shown["event"]["provider"], "deepseek_harness", "{shown:#}");
        assert!(
            shown["event"]["text"]
                .as_str()
                .is_some_and(|text| text.contains(PARENT_ORACLE)),
            "{shown:#}"
        );
        let session = show_session(&harness, result);
        assert_eq!(session["provider"], "deepseek_harness", "{session:#}");
        assert_eq!(
            session["provider_session_id"], PARENT_SESSION_ID,
            "{session:#}"
        );

        let replay = harness.explicit_import(&source, false);
        assert_noop(successful_source(&replay, FILE_FORMAT), &first_generation);
        initial_state.assert_unchanged(&source);
        initial_directory.assert_unchanged(parent);

        daemon.stop();
        let restarted_daemon = harness.start_daemon();
        let restarted = harness.explicit_import(&source, true);
        assert_noop(
            successful_source(&restarted, FILE_FORMAT),
            &first_generation,
        );
        one_matching_result(&harness.search(PARENT_ORACLE), PARENT_ORACLE);
        initial_state.assert_unchanged(&source);
        initial_directory.assert_unchanged(parent);

        let appended_oracle = format!("deepseekharness{}appendoracle5e19", encoding.fixture_root());
        append_round(&source, encoding, 18, &appended_oracle);
        let appended_state = FileSnapshot::capture(&source);
        let appended_directory = DirectorySnapshot::capture(parent);
        let resumed = harness.explicit_import(&source, true);
        harness.wait_for_generation(&resumed);
        let resumed_source = successful_source(&resumed, FILE_FORMAT);
        assert_counts(resumed_source, 25, 10, 0, 15);
        assert_eq!(resumed_source["change"], "changed", "{resumed:#}");
        assert_eq!(resumed_source["generation_changed"], true, "{resumed:#}");
        let resumed_generation = generation(resumed_source).to_owned();
        assert_ne!(resumed_generation, first_generation, "{resumed:#}");
        one_matching_result(&harness.search(&appended_oracle), &appended_oracle);

        let resumed_replay = harness.explicit_import(&source, true);
        assert_noop(
            successful_source(&resumed_replay, FILE_FORMAT),
            &resumed_generation,
        );
        appended_state.assert_unchanged(&source);
        appended_directory.assert_unchanged(parent);
        drop(restarted_daemon);
    }
}

#[test]
fn secret_shaped_visible_text_is_preserved_without_leaking_into_receipts_or_citations() {
    // This is a static test canary, not a credential.
    const FAKE_SECRET: &str = "sk-test-DEEPSEEK-HARNESS-NOT-REAL-4f91";
    let harness = Harness::new(false);
    let source = harness.root.path().join("sensitive/session.jsonl");
    copy_public_session(Encoding::Raw, PARENT_SESSION_ID, &source);
    append_round(&source, Encoding::Raw, 18, FAKE_SECRET);
    let authored_state = FileSnapshot::capture(&source);
    let authored_directory = DirectorySnapshot::capture(source.parent().unwrap());
    let _daemon = harness.start_daemon();

    let report = harness.explicit_import(&source, false);
    harness.wait_for_generation(&report);
    assert_counts(successful_source(&report, FILE_FORMAT), 25, 10, 0, 15);
    assert!(
        !serde_json::to_string(&report)
            .unwrap()
            .contains(FAKE_SECRET),
        "import receipt exposed provider content: {report:#}"
    );
    let search = harness.search(FAKE_SECRET);
    let result = one_matching_result(&search, FAKE_SECRET);
    assert_citation(result);
    assert!(
        !serde_json::to_string(&result["citations"])
            .unwrap()
            .contains(FAKE_SECRET),
        "citation metadata exposed provider content: {result:#}"
    );
    let shown = show_event(&harness, result);
    assert!(
        shown["event"]["text"]
            .as_str()
            .is_some_and(|text| text.contains(FAKE_SECRET)),
        "show did not preserve selected provider-authored text: {shown:#}"
    );
    authored_state.assert_unchanged(&source);
    authored_directory.assert_unchanged(source.parent().unwrap());
}

#[test]
fn private_reasoning_and_image_only_messages_are_retained_but_not_searchable() {
    let harness = Harness::new(false);
    let source = harness.root.path().join("omitted/session.jsonl");
    copy_public_session(Encoding::Raw, PARENT_SESSION_ID, &source);
    let reasoning_marker = "deepseekharnessprivatereasoningmarker6a42";
    append_record(
        &source,
        &json!({
            "type":"assistant/message","seq":18,"time":1786584000000_i64,
            "data":{"message":{"role":"assistant","content":[
                {"type":"reasoning","text":reasoning_marker}
            ],"source":{"kind":"model","provider":"fixture-provider","model":"fixture-model"},
            "id":"90000000-0000-4000-8000-000000000001"}}
        }),
    );
    append_record(
        &source,
        &json!({
            "type":"user/message","seq":19,"time":1786584001000_i64,
            "data":{"role":"user","id":"90000000-0000-4000-8000-000000000002","content":[{
                "type":"image","attachment":{"attachmentId":format!("sha256:{}", "a".repeat(64)),
                "mediaType":"image/png","bytes":68,"width":1,"height":1,"name":"pixel.png"}
            }]}
        }),
    );
    let authored_state = FileSnapshot::capture(&source);
    let _daemon = harness.start_daemon();

    let report = harness.explicit_import(&source, false);
    harness.wait_for_generation(&report);
    assert_counts(successful_source(&report, FILE_FORMAT), 21, 10, 0, 11);
    assert_not_searchable(&harness, reasoning_marker);

    let lexical = harness.data_root.join("search/lexical");
    let index = VerifiedIndex::open(&lexical).expect("open verified lexical generation");
    let parent_search = harness.search(PARENT_ORACLE);
    let parent_result = one_matching_result(&parent_search, PARENT_ORACLE);
    let parent_session_id = Uuid::parse_str(
        parent_result["ctx_session_id"]
            .as_str()
            .expect("parent ctx session ID"),
    )
    .expect("parse parent ctx session ID");
    let omitted = index
        .core_events_for_session(parent_session_id)
        .expect("enumerate stored Core records")
        .into_iter()
        .filter(|record| record.event_sequence >= 18)
        .collect::<Vec<_>>();
    assert_eq!(omitted.len(), 2);
    assert!(omitted.iter().all(|record| {
        record.core_record.content.normalized_body.is_none()
            && record.core_record.content.structured_content.is_none()
            && record.core_record.content.activity.is_none()
            && format!("{:?}", record.core_record.content.policy_status).starts_with("Omitted")
    }));
    let encoded = serde_json::to_string(
        &omitted
            .iter()
            .map(|record| &record.core_record)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    assert!(!encoded.contains(reasoning_marker));
    assert!(!encoded.contains("sha256:aaaa"));
    authored_state.assert_unchanged(&source);
}

#[test]
fn automatic_default_and_dsh_home_discovery_keep_parent_and_child_independent() {
    for (label, use_override) in [("default", false), ("dshhome", true)] {
        let harness = Harness::new(use_override);
        let selected_home = harness.active_dsh_home();
        copy_tree(&public_fixture_home(Encoding::Raw), &selected_home);
        let parent = selected_home
            .join("sessions")
            .join(PROJECT_KEY)
            .join(PARENT_SESSION_ID)
            .join("session.jsonl");
        let child = selected_home
            .join("sessions")
            .join(PROJECT_KEY)
            .join(CHILD_SESSION_ID)
            .join("session.jsonl");
        let parent_state = FileSnapshot::capture(&parent);
        let child_state = FileSnapshot::capture(&child);
        let _daemon = harness.start_daemon();

        let discovered = harness.json(["sources", "--format=json", "--all"]);
        let source = discovered["sources"]
            .as_array()
            .expect("discovered sources")
            .iter()
            .find(|source| source["provider"] == "deepseek_harness")
            .unwrap_or_else(|| {
                panic!("DeepSeek Harness source was not discovered: {discovered:#}")
            });
        assert_eq!(source["status"], "available", "{discovered:#}");
        assert_eq!(source["source_format"], TREE_FORMAT, "{discovered:#}");

        let imported = harness.json([
            "import",
            "--provider",
            "deepseek-harness",
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

        let parent_search = harness.search(PARENT_ORACLE);
        let parent_result = one_matching_result(&parent_search, PARENT_ORACLE);
        let child_search = harness.search(CHILD_ORACLE);
        let child_result = one_matching_result(&child_search, CHILD_ORACLE);
        assert_eq!(parent_result["provider_session_id"], PARENT_SESSION_ID);
        assert_eq!(child_result["provider_session_id"], CHILD_SESSION_ID);
        assert_eq!(parent_result["agent_scope"], "primary", "{parent_result:#}");
        assert_eq!(child_result["agent_scope"], "subagent", "{child_result:#}");
        assert!(
            parent_result["session_relationship"].is_null(),
            "{parent_result:#}"
        );
        assert!(
            child_result["session_relationship"].is_null(),
            "{child_result:#}"
        );
        assert_ne!(
            parent_result["ctx_session_id"], child_result["ctx_session_id"],
            "{label} discovery collapsed independent ctx sessions"
        );
        assert!(!show_session(&harness, parent_result)["events"]
            .as_array()
            .expect("parent session events")
            .is_empty());
        assert!(!show_session(&harness, child_result)["events"]
            .as_array()
            .expect("child session events")
            .is_empty());
        parent_state.assert_unchanged(&parent);
        child_state.assert_unchanged(&child);
    }
}

#[test]
fn malformed_committed_json_is_rejected_without_losing_the_valid_prefix() {
    let harness = Harness::new(false);
    let source = harness.root.path().join("malformed/session.jsonl");
    copy_public_session(Encoding::Raw, PARENT_SESSION_ID, &source);
    append_bytes(
        &source,
        b"{\"type\":\"fixture/malformed\",\"seq\":18,\"data\":\"deepseekharnessmalformedmarker1a72\"\n",
    );
    append_record(
        &source,
        &json!({"type":"turn/end","seq":18,"time":1786581000000_i64,"data":{"turn":2,"reason":{"kind":"completed"}}}),
    );
    let authored_state = FileSnapshot::capture(&source);
    let authored_directory =
        DirectorySnapshot::capture(source.parent().expect("malformed source parent"));
    let _daemon = harness.start_daemon();

    let report = harness.explicit_import(&source, false);
    assert_counts(rejected_source(&report, 1), 21, 8, 1, 12);
    let diagnostics = report["sources"][0]["rejection_diagnostics"]
        .as_array()
        .expect("bounded malformed-row diagnostics");
    assert_eq!(diagnostics.len(), 1, "{report:#}");
    assert_eq!(diagnostics[0]["provider"], "deepseek_harness", "{report:#}");
    assert_eq!(diagnostics[0]["line"], 20, "{report:#}");
    assert_eq!(diagnostics[0]["class"], "malformed_record", "{report:#}");
    assert!(
        diagnostics[0]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("invalid or has duplicate object keys")),
        "{report:#}"
    );
    harness.wait_for_generation(&report);
    one_matching_result(&harness.search(PARENT_ORACLE), PARENT_ORACLE);
    assert_not_searchable(&harness, "deepseekharnessmalformedmarker1a72");
    authored_state.assert_unchanged(&source);
    authored_directory.assert_unchanged(source.parent().unwrap());
}

#[test]
fn malformed_packed_chunk_reports_a_bounded_line_diagnostic() {
    let harness = Harness::new(false);
    let source = harness.root.path().join("malformed-packed/session.jsonl");
    copy_public_session(Encoding::Raw, PARENT_SESSION_ID, &source);
    append_record(
        &source,
        &json!({
            "type":"text-chunks","seq0":18,"time0":1786581500000_i64,
            "data":{"turn":-1,"step":1,"index":0,"dt":[],"texts":["ignored"]}
        }),
    );
    append_round(
        &source,
        Encoding::Raw,
        18,
        "deepseekharnessaftermalformedpackedmarker9e31",
    );
    let _daemon = harness.start_daemon();

    let report = harness.explicit_import(&source, false);
    harness.wait_for_generation(&report);
    assert_eq!(
        report["sources"][0]["current_rejected_records"], 1,
        "{report:#}"
    );
    let diagnostics = report["sources"][0]["rejection_diagnostics"]
        .as_array()
        .expect("bounded malformed-packed diagnostics");
    assert_eq!(diagnostics.len(), 1, "{report:#}");
    assert_eq!(diagnostics[0]["line"], 20, "{report:#}");
    assert!(
        diagnostics[0]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("malformed text-chunks storage row")),
        "{report:#}"
    );
}

#[test]
fn packed_chunk_rows_advance_native_sequence_without_becoming_searchable() {
    let harness = Harness::new(false);
    let source = harness.root.path().join("packed/session.jsonl");
    copy_public_session(Encoding::Raw, PARENT_SESSION_ID, &source);
    let packed_marker = "deepseekharnesspackedignoredmarker2d91";
    append_record(
        &source,
        &json!({
            "type":"text-chunks","seq0":18,"time0":1786581500000_i64,
            "data":{"turn":2,"step":1,"index":0,"dt":[1,1],
                "texts":[packed_marker,"middle","end"]}
        }),
    );
    let later_marker = "deepseekharnessafterpackedmarker7a14";
    append_round(&source, Encoding::Raw, 21, later_marker);
    let authored_state = FileSnapshot::capture(&source);
    let _daemon = harness.start_daemon();

    let report = harness.explicit_import(&source, false);
    harness.wait_for_generation(&report);
    assert_counts(successful_source(&report, FILE_FORMAT), 28, 10, 0, 18);
    assert_not_searchable(&harness, packed_marker);
    one_matching_result(&harness.search(later_marker), later_marker);
    authored_state.assert_unchanged(&source);
}

#[test]
fn native_sequence_gaps_and_duplicates_fail_closed() {
    for (label, seq) in [("gap", 19_u64), ("duplicate", 17_u64)] {
        let harness = Harness::new(false);
        let source = harness.root.path().join(label).join("session.jsonl");
        copy_public_session(Encoding::Raw, PARENT_SESSION_ID, &source);
        append_record(
            &source,
            &json!({"type":"turn/end","seq":seq,"time":1786581750000_i64,
                "data":{"turn":2,"reason":{"kind":"completed"}}}),
        );
        let authored_state = FileSnapshot::capture(&source);
        let authored_directory = DirectorySnapshot::capture(source.parent().unwrap());
        let _daemon = harness.start_daemon();
        let failure = harness.failed_explicit_import(&source);
        assert!(
            failure.contains(&format!(
                "corrupt DeepSeek Harness session sequence: expected 18, got {seq}"
            )),
            "{failure}"
        );
        authored_state.assert_unchanged(&source);
        authored_directory.assert_unchanged(source.parent().unwrap());
    }
}

#[test]
fn duplicate_or_foreign_later_headers_fail_closed() {
    for (label, session_id) in [
        ("duplicate", PARENT_SESSION_ID),
        ("foreign", CHILD_SESSION_ID),
    ] {
        let harness = Harness::new(false);
        let source = harness.root.path().join(label).join("session.jsonl");
        copy_public_session(Encoding::Raw, PARENT_SESSION_ID, &source);
        append_record(
            &source,
            &json!({
                "type":"session","version":0,"id":session_id,
                "createdAt":1786579200000_i64,
                "cwd":"/workspace/deepseek-harness-fixture",
                "delegationDepth":0,"agentPreset":"fixture-agent"
            }),
        );
        let _daemon = harness.start_daemon();
        let failure = harness.failed_explicit_import(&source);
        assert!(
            failure.contains("DeepSeek Harness session header is not the first row"),
            "{label}: {failure}"
        );
    }
}

#[test]
fn complete_tail_body_is_searchable_and_tantivy_stores_only_core_record() {
    let harness = Harness::new(false);
    let source = harness.root.path().join("tail/session.jsonl");
    copy_public_session(Encoding::Raw, PARENT_SESSION_ID, &source);
    let tail = "deepseekharnesstailonlymarker5f63";
    let body = format!("{} {tail}", "long-body-segment ".repeat(1_200));
    append_round(&source, Encoding::Raw, 18, &body);
    let _daemon = harness.start_daemon();

    let report = harness.explicit_import(&source, false);
    harness.wait_for_generation(&report);
    one_matching_result(&harness.search(tail), tail);

    let lexical = harness.data_root.join("search/lexical");
    let index = VerifiedIndex::open(&lexical).expect("open verified lexical generation");
    let hits = index
        .search_event_candidates(tail, 10)
        .expect("search tail term in Tantivy");
    assert_eq!(hits.len(), 1);
    let expected_event_id = hits[0].event.event_id;
    let stored = index
        .core_event_by_id(expected_event_id)
        .expect("load tail record from stored Core")
        .expect("tail record exists in stored Core");
    assert_eq!(
        stored.core_record.content.normalized_body.as_deref(),
        Some(body.as_str())
    );
}

#[test]
fn interrupted_daemon_import_is_read_only_and_restart_recovers() {
    let harness = Harness::new(false);
    let source = harness.root.path().join("cancelled/session.jsonl");
    copy_public_session(Encoding::Raw, PARENT_SESSION_ID, &source);
    let ignored_marker = "deepseekharnesscancelledignoredmarker4c29";
    let mut writer = std::io::BufWriter::new(
        OpenOptions::new()
            .append(true)
            .open(&source)
            .expect("open large cancellation source"),
    );
    for offset in 0..25_000_u64 {
        serde_json::to_writer(
            &mut writer,
            &json!({
                "type":"turn/end","seq":18 + offset,"time":1786584000000_u64 + offset,
                "data":{"turn":2,"reason":{"kind":"completed"},
                    "padding":format!("{}{}", "x".repeat(2_000), ignored_marker)}
            }),
        )
        .expect("serialize cancellation row");
        writer.write_all(b"\n").expect("append cancellation row");
    }
    writer.flush().expect("flush cancellation source");
    let authored_state = FileSnapshot::capture(&source);
    let parent = source.parent().unwrap();
    let authored_directory = DirectorySnapshot::capture(parent);
    let daemon = harness.start_daemon();
    let mut import = harness
        .command()
        .args([
            "import",
            "--provider",
            "deepseek-harness",
            "--path",
            source.to_str().unwrap(),
            "--no-daemon",
            "--format=json",
            "--progress",
            "json",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start cancellable import");
    let mut progress = std::io::BufReader::new(
        import
            .stderr
            .take()
            .expect("capture import progress output"),
    );
    let mut first_progress_line = String::new();
    std::io::BufRead::read_line(&mut progress, &mut first_progress_line)
        .expect("read import progress milestone");
    assert!(
        first_progress_line.contains("\"type\":\"ctx_progress\""),
        "import did not report a progress milestone: {first_progress_line:?}"
    );
    drop(progress);
    daemon.stop();
    assert!(!import.wait().expect("reap cancelled import").success());
    authored_state.assert_unchanged(&source);
    authored_directory.assert_unchanged(parent);

    let _restarted = harness.start_daemon();
    let report = harness.explicit_import(&source, false);
    harness.wait_for_generation(&report);
    assert_counts(
        successful_source(&report, FILE_FORMAT),
        25_019,
        8,
        0,
        25_011,
    );
    assert_not_searchable(&harness, ignored_marker);
    authored_state.assert_unchanged(&source);
    authored_directory.assert_unchanged(parent);
}

#[test]
fn future_version_and_unknown_required_events_fail_closed() {
    let future_marker = "deepseekharnessfutureheadermarker3c84";
    let future_harness = Harness::new(false);
    let future = future_harness.root.path().join("future/session.jsonl");
    copy_public_session(Encoding::Raw, PARENT_SESSION_ID, &future);
    rewrite_header_as_future(&future, future_marker);
    let future_state = FileSnapshot::capture(&future);
    let future_directory = DirectorySnapshot::capture(future.parent().unwrap());
    let _future_daemon = future_harness.start_daemon();
    let future_failure = future_harness.failed_explicit_import(&future);
    assert!(
        future_failure.contains("unsupported DeepSeek Harness session format version 1"),
        "{future_failure}"
    );
    future_state.assert_unchanged(&future);
    future_directory.assert_unchanged(future.parent().unwrap());

    let required_marker = "deepseekharnessunknownrequiredmarker6d20";
    let required_harness = Harness::new(false);
    let required = required_harness
        .root
        .path()
        .join("unknown-required/session.jsonl");
    copy_public_session(Encoding::Raw, PARENT_SESSION_ID, &required);
    append_record(
        &required,
        &json!({
            "type":"fixture/unknown-required","seq":18,"time":1786582000000_i64,
            "data":{"text":required_marker}
        }),
    );
    let required_state = FileSnapshot::capture(&required);
    let required_directory = DirectorySnapshot::capture(required.parent().unwrap());
    let _required_daemon = required_harness.start_daemon();
    let required_failure = required_harness.failed_explicit_import(&required);
    assert!(
        required_failure.contains("unsupported required DeepSeek Harness semantic event type"),
        "{required_failure}"
    );
    required_state.assert_unchanged(&required);
    required_directory.assert_unchanged(required.parent().unwrap());
}

#[test]
fn unknown_v0_header_fields_fail_closed() {
    let harness = Harness::new(false);
    let marker = "deepseekharnessunknownv0headermarker7b1a";
    let source = harness.root.path().join("unknown-v0-header/session.jsonl");
    copy_public_session(Encoding::Raw, PARENT_SESSION_ID, &source);
    rewrite_header_with_unknown_v0_field(&source, marker);

    let _daemon = harness.start_daemon();
    let output = harness.failed_explicit_import(&source);
    assert!(
        output.contains("unknown DeepSeek Harness session header field"),
        "{output}"
    );
}

#[test]
fn unknown_ignorable_future_event_is_skipped_and_later_native_events_import() {
    let harness = Harness::new(false);
    let source = harness.root.path().join("unknown-ignorable/session.jsonl");
    copy_public_session(Encoding::Raw, PARENT_SESSION_ID, &source);
    let ignored_marker = "deepseekharnessignoredfuturemarker8b43";
    append_record(
        &source,
        &json!({
            "type":"fixture/future-note","seq":18,"time":1786583000000_i64,
            "data":{"text":ignored_marker},"ignorable":true
        }),
    );
    let later_marker = "deepseekharnessafterfuturemarker9f51";
    append_round(&source, Encoding::Raw, 19, later_marker);
    let authored_state = FileSnapshot::capture(&source);
    let _daemon = harness.start_daemon();

    let report = harness.explicit_import(&source, false);
    harness.wait_for_generation(&report);
    successful_source(&report, FILE_FORMAT);
    assert_not_searchable(&harness, ignored_marker);
    one_matching_result(&harness.search(later_marker), later_marker);
    authored_state.assert_unchanged(&source);
}
