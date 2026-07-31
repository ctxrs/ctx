mod support;

use std::{
    io::Read,
    process::{Child, Command as StdCommand, Stdio},
};

use support::*;

const BEGIN_SENTINEL: &str = "CTX_CORE_BEGIN-";
const END_SENTINEL: &str = "-CTX_CORE_END";
const CORE_QUERY: &str = "selfcontainedcoresentinel";
const PROVIDER_SESSION_ID: &str = "019fa000-0000-7000-8000-000000000091";

struct CorePublishedMessage {
    temp: TempDir,
    removed_source: PathBuf,
    event_id: String,
    session_id: String,
    complete_text: String,
}

struct PublicationDaemon {
    child: Option<Child>,
}

impl PublicationDaemon {
    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for PublicationDaemon {
    fn drop(&mut self) {
        self.stop();
    }
}

fn start_publication_daemon(temp: &TempDir) -> PublicationDaemon {
    let root = data_root(temp);
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("config.toml"),
        "[daemon]\nenabled = true\nmode = \"source-refresh-only\"\n\n[search]\nsemantic = false\n",
    )
    .unwrap();
    let binary = bind_test_ctx_binary(temp);
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
            Err(error) => panic!("start isolated publication daemon: {error}"),
        }
    };
    let mut daemon = PublicationDaemon { child: Some(child) };
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
            panic!("publication daemon exited before becoming ready ({exit}): {stderr}");
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
            "timed out waiting for publication daemon readiness: {status:#?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn published_codex_message_with_removed_provider_file() -> CorePublishedMessage {
    let temp = tempdir();
    let source_root = temp.path().join(".codex/sessions");
    let source = source_root.join(format!("2026/07/28/rollout-{PROVIDER_SESSION_ID}.jsonl"));
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    let complete_text = format!(
        "{CORE_QUERY} {BEGIN_SENTINEL}{}{END_SENTINEL}",
        "y".repeat(20_000)
    );
    let records = [
        json!({
            "timestamp": "2026-07-28T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": PROVIDER_SESSION_ID,
                "timestamp": "2026-07-28T12:00:00Z",
                "cwd": "/workspace/self-contained-core",
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

    let mut daemon = start_publication_daemon(&temp);
    let bootstrap = json_output(ctx(&temp).args([
        "search",
        CORE_QUERY,
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_eq!(bootstrap["payload_type"], "search_results");
    assert_eq!(bootstrap["retrieval"]["index"], "core");
    let result = bootstrap["results"]
        .as_array()
        .and_then(|results| results.first())
        .expect("published Core bootstrap search result");
    let event_id = result["ctx_event_id"].as_str().unwrap().to_owned();
    let session_id = result["ctx_session_id"].as_str().unwrap().to_owned();

    daemon.stop();
    fs::remove_file(&source).unwrap();
    assert!(!source.exists());
    fs::write(
        data_root(&temp).join("config.toml"),
        "[daemon]\nenabled = false\n\n[search]\nsemantic = false\n",
    )
    .unwrap();

    CorePublishedMessage {
        temp,
        removed_source: source,
        event_id,
        session_id,
        complete_text,
    }
}

fn assert_no_legacy_store(fixture: &CorePublishedMessage) {
    assert!(!fixture.removed_source.exists());
    assert!(
        !fixture.temp.path().join("work.sqlite").exists(),
        "Core search/show must not initialize or read the legacy Store"
    );
}

fn assert_core_event(event: &Value, fixture: &CorePublishedMessage) {
    assert_eq!(event["ctx_event_id"], fixture.event_id);
    assert_eq!(event["provider_session_id"], PROVIDER_SESSION_ID);
    assert_eq!(event["text"], fixture.complete_text);
    assert_eq!(event["content"]["complete"], true);
    assert_eq!(event["content"]["policy_status"], "selected");
    assert!(event.get("source").is_none(), "{event:#}");
    assert!(event.get("source_path").is_none(), "{event:#}");
    assert!(event["content"].get("origin").is_none(), "{event:#}");
    assert!(event.get("preview").is_none(), "{event:#}");
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

fn mcp_call(fixture: &CorePublishedMessage, id: &str, name: &str, arguments: Value) -> Value {
    let responses = mcp_roundtrip(
        &fixture.temp,
        &[
            mcp_initialize(),
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": { "name": name, "arguments": arguments }
            }),
        ],
    );
    responses[1].clone()
}

#[test]
fn cli_search_and_show_use_complete_core_after_provider_file_removal() {
    let fixture = published_codex_message_with_removed_provider_file();

    let search = json_output(ctx(&fixture.temp).args([
        "search",
        CORE_QUERY,
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--format=json",
    ]));
    let result = &search["results"][0];
    assert_eq!(search["retrieval"]["index"], "core");
    assert_eq!(result["provider_session_id"], PROVIDER_SESSION_ID);
    assert_eq!(result["snippet_truncated"], true);
    assert!(result["snippet"].as_str().unwrap().contains(BEGIN_SENTINEL));
    assert!(!result["snippet"].as_str().unwrap().contains(END_SENTINEL));
    assert!(result.get("source_path").is_none());
    assert!(result["citations"][0].get("source_path").is_none());

    let event = json_output(ctx(&fixture.temp).args([
        "show",
        "event",
        &fixture.event_id[..8],
        "--format=json",
    ]));
    assert_core_event(&event["event"], &fixture);

    let session = json_output(ctx(&fixture.temp).args([
        "show",
        "session",
        &fixture.session_id[..8],
        "--mode",
        "full",
        "--format=json",
    ]));
    assert_eq!(session["ctx_session_id"], fixture.session_id);
    assert_eq!(session["provider_session_id"], PROVIDER_SESSION_ID);
    assert_core_event(&session["events"][0], &fixture);

    let resumed = json_output(ctx(&fixture.temp).args([
        "show",
        "session",
        "--provider-session",
        PROVIDER_SESSION_ID,
        "--provider",
        "codex",
        "--mode",
        "full",
        "--format=json",
    ]));
    assert_eq!(resumed["ctx_session_id"], fixture.session_id);
    assert_eq!(resumed["provider_session_id"], PROVIDER_SESSION_ID);
    assert!(resumed["events"][0]["text"]
        .as_str()
        .unwrap()
        .contains(END_SENTINEL));
    assert!(fixture.complete_text.len() > 16 * 1024);
    assert_no_legacy_store(&fixture);
}

#[test]
fn mcp_search_and_show_use_complete_core_after_provider_file_removal() {
    let fixture = published_codex_message_with_removed_provider_file();

    let search_response = mcp_call(
        &fixture,
        "search",
        "search",
        json!({"query": CORE_QUERY, "provider": "codex", "backend": "lexical"}),
    );
    let search = &search_response["result"]["structuredContent"];
    assert_eq!(search["retrieval"]["index"], "core");
    assert_eq!(
        search["results"][0]["provider_session_id"],
        PROVIDER_SESSION_ID
    );
    assert_eq!(search["results"][0]["snippet_truncated"], true);
    assert!(search["results"][0]["citations"][0]
        .get("source_path")
        .is_none());

    let event_response = mcp_call(
        &fixture,
        "show-event",
        "show_event",
        json!({"ctx_event_id": &fixture.event_id[..8]}),
    );
    assert_core_event(
        &event_response["result"]["structuredContent"]["event"],
        &fixture,
    );

    let session_response = mcp_call(
        &fixture,
        "show-session",
        "show_session",
        json!({
            "ctx_session_id": &fixture.session_id[..8],
            "mode": "full"
        }),
    );
    let session = &session_response["result"]["structuredContent"];
    assert_eq!(session["provider_session_id"], PROVIDER_SESSION_ID);
    assert_core_event(&session["events"][0], &fixture);

    assert!(fixture.complete_text.len() > 16 * 1024);
    assert_no_legacy_store(&fixture);
}

#[test]
fn show_requires_a_core_generation_without_initializing_the_store() {
    let temp = tempdir();
    let event_id = "019fa000-0000-7000-8000-000000000099";

    let stderr = failure_stderr(ctx(&temp).args(["show", "event", event_id]));
    assert!(stderr.contains("Core index is not initialized"), "{stderr}");
    assert!(!temp.path().join("work.sqlite").exists());
}
