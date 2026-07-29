mod support;

use std::{
    io::Read,
    process::{Child, Command as StdCommand, Stdio},
};

use ctx_history_capture::ingest_codex_source_backed_v0;
use support::*;

const BEGIN_SENTINEL: &str = "CTX_HYDRATION_BEGIN-";
const END_SENTINEL: &str = "-CTX_HYDRATION_END";
const SOURCE_INDEX_QUERY: &str = "sourceindexsentinel";
const SOURCE_INDEX_PROVIDER_SESSION_ID: &str = "019fa000-0000-7000-8000-000000000091";

struct SourceIndexedMessage {
    _daemon: SourceHydrationDaemon,
    temp: TempDir,
    source_root: PathBuf,
    source: PathBuf,
    index_root: PathBuf,
    event_id: String,
    session_id: String,
    complete_text: String,
}

struct SourceHydrationDaemon {
    child: Option<Child>,
}

impl SourceHydrationDaemon {
    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for SourceHydrationDaemon {
    fn drop(&mut self) {
        self.stop();
    }
}

fn start_source_hydration_daemon(temp: &TempDir) -> SourceHydrationDaemon {
    fs::write(
        temp.path().join("config.toml"),
        "[daemon]\nenabled = true\nmode = \"source-refresh-only\"\n\n[search]\nsemantic = false\n",
    )
    .unwrap();
    let binary = copied_ctx_binary(temp);
    let prepared = ctx_from_binary(temp, &binary);
    let mut command = StdCommand::new(prepared.get_program());
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
    command
        .args([
            "daemon",
            "run",
            "--force",
            "--idle-exit-seconds",
            "600",
            "--loop-interval-seconds",
            "600",
        ])
        .env("CTX_DAEMON_MODE", "source-refresh-only")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let spawn_deadline = Instant::now() + Duration::from_secs(1);
    let child = loop {
        match command.spawn() {
            Ok(child) => break child,
            Err(error) if error.raw_os_error() == Some(26) && Instant::now() < spawn_deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("start isolated source-hydration daemon: {error}"),
        }
    };
    let mut daemon = SourceHydrationDaemon { child: Some(child) };
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(exit) = daemon.child.as_mut().unwrap().try_wait().unwrap() {
            let mut stderr = String::new();
            daemon
                .child
                .as_mut()
                .unwrap()
                .stderr
                .as_mut()
                .unwrap()
                .read_to_string(&mut stderr)
                .unwrap();
            panic!("source-hydration daemon exited before becoming ready ({exit}): {stderr}");
        }
        let status = ctx(temp)
            .args(["daemon", "status", "--format=json"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| serde_json::from_slice::<Value>(&output.stdout).ok());
        if status.as_ref().is_some_and(|status| {
            status["daemon"]["running"] == true
                && status["daemon"]["source_refresh_endpoint"]["available"] == true
        }) {
            return daemon;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for source-hydration daemon readiness: {status:#?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn source_indexed_codex_message() -> SourceIndexedMessage {
    let temp = tempdir();
    let source_root = temp.path().join(".codex/sessions");
    let source = source_root.join(format!(
        "2026/07/28/rollout-{SOURCE_INDEX_PROVIDER_SESSION_ID}.jsonl"
    ));
    let index_root = temp.path().join("search").join("lexical");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    let complete_text = format!(
        "{SOURCE_INDEX_QUERY} {BEGIN_SENTINEL}{}{END_SENTINEL}",
        "y".repeat(20_000)
    );
    let records = [
        json!({
            "timestamp": "2026-07-28T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": SOURCE_INDEX_PROVIDER_SESSION_ID,
                "timestamp": "2026-07-28T12:00:00Z",
                "cwd": "/workspace/source-index",
                "originator": "codex_cli_rs",
                "cli_version": "0.1.0",
                "source": "cli",
                "model_provider": "openai"
            }
        }),
        json!({
            "timestamp": "2026-07-28T12:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{
                    "type": "output_text",
                    "text": complete_text
                }],
                "phase": "final_answer"
            }
        }),
    ];
    let transcript = records
        .iter()
        .map(|record| format!("{}\n", serde_json::to_string(record).unwrap()))
        .collect::<String>();
    fs::write(&source, transcript).unwrap();
    let daemon = start_source_hydration_daemon(&temp);
    let bootstrap = json_output(ctx(&temp).args([
        "search",
        SOURCE_INDEX_QUERY,
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_eq!(bootstrap["payload_type"], "search_results");
    assert_eq!(bootstrap["retrieval"]["index"], "source_backed");
    let result = bootstrap["results"]
        .as_array()
        .and_then(|results| results.first())
        .expect("source-backed bootstrap search result");
    assert_eq!(result["provider"], "codex");
    assert_eq!(
        result["provider_session_id"],
        SOURCE_INDEX_PROVIDER_SESSION_ID
    );
    let event_id = result["ctx_event_id"].as_str().unwrap().to_owned();
    let session_id = result["ctx_session_id"].as_str().unwrap().to_owned();

    SourceIndexedMessage {
        _daemon: daemon,
        temp,
        source_root,
        source,
        index_root,
        event_id,
        session_id,
        complete_text,
    }
}

fn assert_no_legacy_store(fixture: &SourceIndexedMessage) {
    assert!(
        !fixture.temp.path().join("work.sqlite").exists(),
        "source-generation show must not initialize or read the legacy Store"
    );
}

fn mcp_initialize() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": "init",
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "ctx-test", "version": "0" }
        }
    })
}

fn mcp_show_event(fixture: &SourceIndexedMessage, content: &str) -> Value {
    let responses = mcp_roundtrip(
        &fixture.temp,
        &[
            mcp_initialize(),
            json!({
                "jsonrpc": "2.0",
                "id": "show-event",
                "method": "tools/call",
                "params": {
                    "name": "show_event",
                    "arguments": {
                        "ctx_event_id": &fixture.event_id[..8],
                        "content": content
                    }
                }
            }),
        ],
    );
    responses[1].clone()
}

fn mcp_show_session(fixture: &SourceIndexedMessage, content: &str) -> Value {
    let responses = mcp_roundtrip(
        &fixture.temp,
        &[
            mcp_initialize(),
            json!({
                "jsonrpc": "2.0",
                "id": "show-session",
                "method": "tools/call",
                "params": {
                    "name": "show_session",
                    "arguments": {
                        "ctx_session_id": &fixture.session_id[..8],
                        "mode": "full",
                        "content": content
                    }
                }
            }),
        ],
    );
    responses[1].clone()
}

fn assert_provider_authoritative_event(
    event: &Value,
    fixture: &SourceIndexedMessage,
    requested: &str,
) {
    assert_eq!(event["ctx_event_id"], fixture.event_id);
    assert_eq!(event["text"], fixture.complete_text);
    assert_eq!(event["content"]["requested"], requested);
    assert_eq!(event["content"]["complete"], true);
    assert_eq!(event["content"]["origin"], "provider_source");
    assert_eq!(event["content"]["source_verified"], true);
    assert_eq!(event["content"]["complete_content_available"], true);
    assert!(
        event.get("preview").is_none(),
        "show must not duplicate hydrated text in a preview field: {event:#}"
    );
    assert!(
        event["content"].get("stored_truncated").is_none(),
        "provider-authoritative metadata must not expose stored-body truncation: {event:#}"
    );
}

#[test]
fn cli_show_hydrates_exact_provider_source_for_both_policy_tokens_without_preview() {
    let fixture = source_indexed_codex_message();
    let event_prefix = &fixture.event_id[..8];

    for policy in ["indexed", "complete"] {
        let event = json_output(ctx(&fixture.temp).args([
            "show",
            "event",
            event_prefix,
            "--content",
            policy,
            "--format=json",
        ]));
        assert_eq!(event["payload_type"], "event_window");
        assert_eq!(event["content_policy"], policy);
        assert_provider_authoritative_event(&event["event"], &fixture, policy);

        let session = json_output(ctx(&fixture.temp).args([
            "show",
            "session",
            &fixture.session_id[..8],
            "--mode",
            "full",
            "--content",
            policy,
            "--format=json",
        ]));
        assert_eq!(session["payload_type"], "session_transcript");
        assert_eq!(session["ctx_session_id"], fixture.session_id);
        assert_eq!(session["provider"], "codex");
        assert_eq!(session["content_policy"], policy);
        assert_eq!(
            session["provider_session_id"],
            SOURCE_INDEX_PROVIDER_SESSION_ID
        );
        assert_provider_authoritative_event(&session["events"][0], &fixture, policy);
    }

    let text = ctx(&fixture.temp)
        .args(["show", "event", event_prefix])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(String::from_utf8(text).unwrap().contains(END_SENTINEL));

    let export_path = fixture.temp.path().join("nested/transcript.md");
    ctx(&fixture.temp)
        .args([
            "show",
            "session",
            &fixture.session_id,
            "--mode",
            "full",
            "--format",
            "markdown",
            "--out",
            export_path.to_str().unwrap(),
        ])
        .assert()
        .success();
    let exported = fs::read_to_string(export_path).unwrap();
    assert!(exported.contains(&format!(
        "# codex session {SOURCE_INDEX_PROVIDER_SESSION_ID}"
    )));
    assert!(exported.contains(END_SENTINEL));

    let jsonl = ctx(&fixture.temp)
        .args([
            "show",
            "session",
            &fixture.session_id,
            "--mode",
            "full",
            "--format=jsonl",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let jsonl: Value = serde_json::from_slice(&jsonl).unwrap();
    assert_eq!(jsonl["payload_type"], "session_transcript_event");
    assert_provider_authoritative_event(&jsonl["event"], &fixture, "indexed");
    assert_no_legacy_store(&fixture);
}

#[test]
fn mcp_show_hydrates_exact_provider_source_for_both_policy_tokens_without_preview() {
    let fixture = source_indexed_codex_message();

    for policy in ["indexed", "complete"] {
        let event_response = mcp_show_event(&fixture, policy);
        let event = &event_response["result"]["structuredContent"];
        assert_eq!(event["payload_type"], "event_window");
        assert_eq!(event["content_policy"], policy);
        assert_provider_authoritative_event(&event["event"], &fixture, policy);
        assert_useful_mcp_text(
            &event_response["result"],
            &["ctx show event", &fixture.event_id, BEGIN_SENTINEL],
        );

        let session_response = mcp_show_session(&fixture, policy);
        let session = &session_response["result"]["structuredContent"];
        assert_eq!(session["payload_type"], "session_transcript");
        assert_eq!(session["ctx_session_id"], fixture.session_id);
        assert_eq!(session["content_policy"], policy);
        assert_provider_authoritative_event(&session["events"][0], &fixture, policy);
        assert_useful_mcp_text(
            &session_response["result"],
            &["ctx show session", &fixture.session_id, "provider: codex"],
        );
    }
    assert_no_legacy_store(&fixture);
}

#[test]
fn stale_locator_hydration_fails_closed_for_both_policy_tokens() {
    let fixture = source_indexed_codex_message();
    let original = fs::read_to_string(&fixture.source).unwrap();
    let changed = original.replacen(BEGIN_SENTINEL, "STALE_LOCATOR_BEGIN-", 1);
    assert_ne!(changed, original);
    fs::write(&fixture.source, changed).unwrap();

    for policy in ["indexed", "complete"] {
        let stderr = failure_stderr(ctx(&fixture.temp).args([
            "show",
            "event",
            &fixture.event_id,
            "--content",
            policy,
            "--format=json",
        ]));
        assert!(stderr.contains("stale_record_evidence"), "{stderr}");
        assert!(!stderr.contains(END_SENTINEL));

        let response = mcp_show_event(&fixture, policy);
        assert_eq!(response["result"]["isError"], true);
        let error = response["result"]["structuredContent"]["error"]
            .as_str()
            .unwrap();
        assert!(error.contains("stale_record_evidence"), "{error}");
        assert!(!error.contains(END_SENTINEL));
    }
    assert_no_legacy_store(&fixture);
}

#[test]
fn unavailable_source_hydration_fails_closed_for_both_policy_tokens() {
    let mut fixture = source_indexed_codex_message();
    fixture._daemon.stop();

    for policy in ["indexed", "complete"] {
        let stderr = failure_stderr(ctx(&fixture.temp).args([
            "show",
            "event",
            &fixture.event_id,
            "--content",
            policy,
            "--format=json",
        ]));
        assert!(stderr.contains("temporarily_unavailable"), "{stderr}");
        assert!(!stderr.contains(END_SENTINEL));

        let response = mcp_show_event(&fixture, policy);
        assert_eq!(response["result"]["isError"], true);
        let error = response["result"]["structuredContent"]["error"]
            .as_str()
            .unwrap();
        assert!(error.contains("temporarily_unavailable"), "{error}");
        assert!(!error.contains(END_SENTINEL));
    }
    assert_no_legacy_store(&fixture);
}

#[test]
fn confirmed_source_deletion_retires_show_without_store_fallback() {
    let fixture = source_indexed_codex_message();
    fs::remove_file(&fixture.source).unwrap();
    ingest_codex_source_backed_v0(&fixture.source_root, &fixture.index_root).unwrap();

    let stderr = failure_stderr(ctx(&fixture.temp).args([
        "show",
        "event",
        &fixture.event_id,
        "--format=json",
    ]));
    assert!(
        stderr.contains("was not found in the source-backed Core generation"),
        "{stderr}"
    );

    let response = mcp_show_event(&fixture, "indexed");
    assert_eq!(response["result"]["isError"], true);
    let error = response["result"]["structuredContent"]["error"]
        .as_str()
        .unwrap();
    assert!(
        error.contains("was not found in the source-backed Core generation"),
        "{error}"
    );
    assert_no_legacy_store(&fixture);
}

#[test]
fn show_requires_a_source_generation_without_initializing_the_store() {
    let temp = tempdir();
    let event_id = "019fa000-0000-7000-8000-000000000099";

    let stderr = failure_stderr(ctx(&temp).args(["show", "event", event_id]));
    assert!(
        stderr.contains("source-backed Core index is not initialized"),
        "{stderr}"
    );

    let responses = mcp_roundtrip(
        &temp,
        &[
            mcp_initialize(),
            json!({
                "jsonrpc": "2.0",
                "id": "show-event",
                "method": "tools/call",
                "params": {
                    "name": "show_event",
                    "arguments": {"ctx_event_id": event_id}
                }
            }),
        ],
    );
    assert_eq!(responses[1]["result"]["isError"], true);
    assert!(responses[1]["result"]["structuredContent"]["error"]
        .as_str()
        .unwrap()
        .contains("source-backed Core index is not initialized"));
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "missing source generations must fail without initializing the Store"
    );
}
